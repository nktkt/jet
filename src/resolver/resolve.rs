//! Transitive resolution implementing Maven's nearest-wins semantics.
//!
//! Approach: BFS from each root dependency, tracking depth and declaration
//! order. For each `(group, artifact)` we keep the version selected by the
//! shallowest path (with first-declared as the tiebreaker). Scopes that don't
//! propagate transitively (`test`, `provided`) are skipped past depth 0.
//! Optional dependencies are skipped transitively. Per-edge `<exclusions>`
//! prune subtrees rooted at the excluded coordinate.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use anyhow::{Context, Result};

use crate::coord::Coord;
use crate::manifest::Manifest;
use crate::resolver::fetcher::Fetcher;
use crate::resolver::pom::{DependencyDecl, Pom, fetch_and_parse};

/// Where in `jet.toml` a resolved dep was sourced from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Main,
    Dev,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Main => "main",
            Origin::Dev => "dev",
        }
    }
}

/// One resolved dependency entry.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub coord: Coord,
    pub scope: String,
    pub origin: Origin,
}

#[derive(Debug, Default)]
pub struct Resolution {
    pub items: Vec<Resolved>,
}

impl Resolution {
    #[allow(dead_code)]
    pub fn compile_and_runtime(&self) -> impl Iterator<Item = &Resolved> {
        self.items
            .iter()
            .filter(|r| r.scope == "compile" || r.scope == "runtime")
    }

    /// Iterate items reachable from main `[dependencies]`.
    pub fn main(&self) -> impl Iterator<Item = &Resolved> {
        self.items.iter().filter(|r| r.origin == Origin::Main)
    }
}

fn decls_from(
    deps: &std::collections::BTreeMap<String, crate::manifest::DepSpec>,
    default_scope: &str,
) -> Result<Vec<DependencyDecl>> {
    let mut out = Vec::with_capacity(deps.len());
    for (key, spec) in deps {
        // Path deps are workspace-local; not resolved through Maven Central.
        if spec.path().is_some() {
            continue;
        }
        let (group, artifact) = split_ga(key)?;
        out.push(DependencyDecl {
            group_id: group.into(),
            artifact_id: artifact.into(),
            version: Some(spec.version().into()),
            classifier: spec.classifier().map(str::to_string),
            ty: Some(spec.ty().into()),
            scope: Some(default_scope.into()),
            optional: false,
            exclusions: spec
                .exclusions()
                .iter()
                .filter_map(|e| {
                    let (g, a) = split_ga(e).ok()?;
                    Some(crate::resolver::pom::Exclusion {
                        group_id: g.into(),
                        artifact_id: a.into(),
                    })
                })
                .collect(),
        });
    }
    Ok(out)
}

/// Resolve `[dependencies]` only.
pub fn resolve(manifest: &Manifest, fetcher: &Fetcher) -> Result<Resolution> {
    let main = decls_from(&manifest.dependencies, "compile")?;
    do_resolve(&[(Origin::Main, main)], fetcher)
}

/// Resolve `[dependencies]` + `[dev-dependencies]`. Main wins on overlap.
pub fn resolve_with_dev(manifest: &Manifest, fetcher: &Fetcher) -> Result<Resolution> {
    let main = decls_from(&manifest.dependencies, "compile")?;
    let dev = decls_from(&manifest.dev_dependencies, "compile")?;
    do_resolve(&[(Origin::Main, main), (Origin::Dev, dev)], fetcher)
}

fn split_ga(key: &str) -> Result<(&str, &str)> {
    key.split_once(':')
        .with_context(|| format!("dependency key `{key}` must be `group:artifact`"))
}

#[derive(Clone)]
struct Walk {
    decl: DependencyDecl,
    depth: usize,
    decl_order: usize,
    origin: Origin,
    /// Inherited exclusions from ancestors.
    excluded: HashSet<(String, String)>,
    /// Inherited dependencyManagement (from BOMs and ancestors). Maps
    /// `(group, artifact)` to a managed version (and possibly scope).
    managed: HashMap<(String, String), DependencyDecl>,
}

fn do_resolve(
    root_groups: &[(Origin, Vec<DependencyDecl>)],
    fetcher: &Fetcher,
) -> Result<Resolution> {
    // (g, a) -> (depth, decl_order, scope, origin, coord)
    let mut chosen: HashMap<(String, String), (usize, usize, String, Origin, Coord)> =
        HashMap::new();
    let mut queue: VecDeque<Walk> = VecDeque::new();

    let mut decl_idx = 0usize;
    for (origin, decls) in root_groups {
        for d in decls {
            queue.push_back(Walk {
                decl: d.clone(),
                depth: 0,
                decl_order: decl_idx,
                origin: *origin,
                excluded: HashSet::new(),
                managed: HashMap::new(),
            });
            decl_idx += 1;
        }
    }

    while let Some(w) = queue.pop_front() {
        // Apply managed defaults if version is missing.
        let mut decl = w.decl.clone();
        let key = (decl.group_id.clone(), decl.artifact_id.clone());

        if w.excluded.contains(&key) {
            continue;
        }
        // Skip optional past depth 0.
        if decl.optional && w.depth > 0 {
            continue;
        }

        if decl.version.is_none() {
            if let Some(m) = w.managed.get(&key) {
                decl.version = m.version.clone();
            }
        }

        // Effective scope: from the dep itself or "compile"; transitively
        // narrowed/eliminated by Maven's matrix.
        let eff_scope = decl
            .scope
            .clone()
            .or_else(|| w.managed.get(&key).and_then(|m| m.scope.clone()))
            .unwrap_or_else(|| "compile".into());

        if w.depth > 0 && (eff_scope == "test" || eff_scope == "provided" || eff_scope == "system") {
            continue;
        }
        // BOM imports never appear as runtime entries — they only contribute
        // dependencyManagement.
        if eff_scope == "import" {
            // Should already be handled before enqueue; skip defensively.
            continue;
        }

        let version = match decl.version.as_deref() {
            Some(v) if !v.is_empty() => v.to_string(),
            _ => {
                // We can't resolve a version; skip with a warning rather than fail
                // outright (real-world POMs do this for optional/managed entries).
                eprintln!(
                    "  warning: skipping {}:{} — no version available",
                    decl.group_id, decl.artifact_id
                );
                continue;
            }
        };

        let coord = Coord {
            group: decl.group_id.clone(),
            artifact: decl.artifact_id.clone(),
            version,
            classifier: decl.classifier.clone(),
            ty: decl.ty.clone().unwrap_or_else(|| "jar".into()),
        };

        // Nearest-wins: insert if shallower; tie-break by decl_order.
        // Origin: main wins on overlap (preserve any prior Main marker).
        let new_origin = match chosen.get(&key) {
            Some((_, _, _, prior, _)) if *prior == Origin::Main => Origin::Main,
            _ => w.origin,
        };
        match chosen.get(&key) {
            None => {
                chosen.insert(
                    key.clone(),
                    (w.depth, w.decl_order, eff_scope.clone(), new_origin, coord.clone()),
                );
            }
            Some((d, o, _, _, _))
                if w.depth < *d || (w.depth == *d && w.decl_order < *o) =>
            {
                chosen.insert(
                    key.clone(),
                    (w.depth, w.decl_order, eff_scope.clone(), new_origin, coord.clone()),
                );
            }
            Some(_) => {
                // Existing chosen wins, but still upgrade origin if needed.
                if let Some(entry) = chosen.get_mut(&key) {
                    entry.3 = new_origin;
                }
                continue;
            }
        }

        // Recurse: fetch this coord's POM and enqueue its dependencies.
        let pom = match fetch_and_parse(&coord, fetcher) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("  warning: could not fetch POM for {coord}: {e:#}");
                continue;
            }
        };
        let pom = resolve_pom_chain(pom, fetcher)?;

        // Build the merged dependencyManagement view: ancestors + this POM.
        let mut managed = w.managed.clone();
        for d in &pom.dependency_management {
            handle_managed(d, &pom, &mut managed, fetcher);
        }

        // Enqueue children with inherited exclusions.
        for (i, child) in pom.dependencies.iter().enumerate() {
            if child.optional {
                continue;
            }
            let scope = child.scope.as_deref().unwrap_or("compile");
            // Skip non-propagating scopes when not a root.
            if scope == "test" || scope == "provided" || scope == "system" {
                continue;
            }
            let ck = (child.group_id.clone(), child.artifact_id.clone());
            let mut excluded = w.excluded.clone();
            for e in &child.exclusions {
                excluded.insert((e.group_id.clone(), e.artifact_id.clone()));
            }
            // Apply property interpolation on version + groupId/artifactId.
            let mut decl = child.clone();
            decl.group_id = pom.interpolate(&decl.group_id, &HashMap::new());
            decl.artifact_id = pom.interpolate(&decl.artifact_id, &HashMap::new());
            if let Some(v) = decl.version.as_deref() {
                decl.version = Some(pom.interpolate(v, &HashMap::new()));
            }
            // If still no version, look in managed.
            if decl.version.is_none() {
                if let Some(m) = managed.get(&ck) {
                    decl.version = m.version.clone();
                }
            }
            queue.push_back(Walk {
                decl,
                depth: w.depth + 1,
                decl_order: i,
                origin: w.origin,
                excluded,
                managed: managed.clone(),
            });
            let _ = ck; // silence unused
        }
    }

    // Build sorted output (deterministic).
    let mut sorted: Vec<((String, String), (usize, usize, String, Origin, Coord))> =
        chosen.into_iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let mut items = Vec::with_capacity(sorted.len());
    for (_, (_, _, scope, origin, coord)) in sorted {
        items.push(Resolved { coord, scope, origin });
    }
    // Ensure deterministic ordering by Coord display.
    items.sort_by(|a, b| a.coord.to_string().cmp(&b.coord.to_string()));
    Ok(Resolution { items })
}

/// Walk the parent chain (if any) and merge properties + dependencyManagement
/// upward into the returned Pom. Bounded to 32 hops to defend against cycles.
pub fn resolve_pom_chain(mut pom: Pom, fetcher: &Fetcher) -> Result<Pom> {
    let mut depth = 0;
    while let Some(parent_ref) = pom.parent.clone() {
        if depth >= 32 {
            break;
        }
        depth += 1;
        let parent_coord = Coord::new(&parent_ref.group_id, &parent_ref.artifact_id, &parent_ref.version);
        let parent_pom = match fetch_and_parse(&parent_coord, fetcher) {
            Ok(p) => p,
            Err(e) => {
                eprintln!(
                    "  warning: could not fetch parent POM {parent_coord}: {e:#}"
                );
                break;
            }
        };
        // Merge: parent values fill in missing pieces of `pom`.
        for (k, v) in &parent_pom.properties {
            pom.properties.entry(k.clone()).or_insert_with(|| v.clone());
        }
        let mut merged_mgmt = parent_pom.dependency_management.clone();
        merged_mgmt.extend(pom.dependency_management.drain(..));
        pom.dependency_management = merged_mgmt;
        // Move up the chain.
        pom.parent = parent_pom.parent.clone();
    }
    Ok(pom)
}

fn handle_managed(
    d: &DependencyDecl,
    enclosing: &Pom,
    managed: &mut HashMap<(String, String), DependencyDecl>,
    fetcher: &Fetcher,
) {
    let scope = d.scope.as_deref().unwrap_or("");
    let ty = d.ty.as_deref().unwrap_or("jar");
    if scope == "import" && ty == "pom" {
        // BOM import: fetch the POM and merge its dependencyManagement.
        let group = enclosing.interpolate(&d.group_id, &HashMap::new());
        let artifact = enclosing.interpolate(&d.artifact_id, &HashMap::new());
        let version = match d.version.as_deref() {
            Some(v) => enclosing.interpolate(v, &HashMap::new()),
            None => return,
        };
        let coord = Coord {
            group,
            artifact,
            version,
            classifier: None,
            ty: "pom".into(),
        };
        match fetch_and_parse(&coord, fetcher) {
            Ok(bom) => {
                let bom = match resolve_pom_chain(bom, fetcher) {
                    Ok(p) => p,
                    Err(_) => return,
                };
                for inner in &bom.dependency_management {
                    handle_managed(inner, &bom, managed, fetcher);
                }
            }
            Err(e) => {
                eprintln!("  warning: BOM {coord} could not be fetched: {e:#}");
            }
        }
        return;
    }
    let mut entry = d.clone();
    if let Some(v) = entry.version.as_deref() {
        entry.version = Some(enclosing.interpolate(v, &HashMap::new()));
    }
    managed.insert(
        (entry.group_id.clone(), entry.artifact_id.clone()),
        entry,
    );
}

/// Quick summary of a resolution for printing.
pub fn summary(resolution: &Resolution) -> String {
    let mut by_scope: BTreeMap<String, usize> = BTreeMap::new();
    for r in &resolution.items {
        *by_scope.entry(r.scope.clone()).or_insert(0) += 1;
    }
    let parts: Vec<String> = by_scope.iter().map(|(k, v)| format!("{v} {k}")).collect();
    format!("{} dependencies ({})", resolution.items.len(), parts.join(", "))
}
