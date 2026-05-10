//! Workspace discovery, member loading, and the path-dep DAG.
//!
//! v0.5 MVP: literal member paths (no glob expansion), no field inheritance
//! (`workspace = true` syntax is deferred), Kahn's-algorithm topological order
//! over path dependencies. A future revision will add globs, parallel build
//! scheduling, and workspace-package / workspace-dependencies inheritance.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use toml_edit::{DocumentMut, Item, Value};

use crate::lockfile::{LOCKFILE_NAME, Lockfile};
use crate::manifest::{DepSpec, DetailedDep, Manifest, PackageMeta, WorkspacePackage, MANIFEST_FILENAME};
use crate::resolver::{Fetcher, default_repos, resolve::resolve_with_dev};

/// One workspace member, fully loaded.
pub struct Member {
    /// Member directory (absolute, canonicalized when possible).
    pub path: PathBuf,
    pub manifest: Manifest,
    pub name: String,
}

pub struct Workspace {
    pub root: PathBuf,
    pub members: Vec<Member>,
    /// `name -> index in members`.
    pub by_name: HashMap<String, usize>,
}

impl Workspace {
    /// Discover the workspace by walking up from `start`. Returns:
    /// - the workspace if a `[workspace]` manifest is found, or
    /// - a single-member "implicit" workspace wrapping the nearest `jet.toml`.
    pub fn discover(start: &Path) -> Result<Self> {
        let root_manifest_dir = Manifest::find_root(start)?;
        let root_manifest = Manifest::load(&root_manifest_dir)?;

        if let Some(ws) = &root_manifest.workspace {
            let mut excluded: std::collections::HashSet<PathBuf> = Default::default();
            for e in &ws.exclude {
                let canon = canonicalize(&root_manifest_dir.join(e));
                excluded.insert(canon);
            }

            // Expand member patterns through `glob`. Patterns without globbing
            // metacharacters fall through as literal paths, matching cargo.
            let mut paths: Vec<PathBuf> = Vec::new();
            let mut seen: std::collections::HashSet<PathBuf> = Default::default();
            for raw in &ws.members {
                let pattern = root_manifest_dir.join(raw);
                let pattern_str = pattern.to_string_lossy();
                let is_glob = raw.contains('*') || raw.contains('?') || raw.contains('[');
                if is_glob {
                    let entries = glob::glob(&pattern_str)
                        .with_context(|| format!("invalid glob `{raw}`"))?;
                    for entry in entries.filter_map(|e| e.ok()) {
                        if entry.is_dir() && entry.join("jet.toml").is_file() {
                            let canon = canonicalize(&entry);
                            if !excluded.contains(&canon) && seen.insert(canon.clone()) {
                                paths.push(entry);
                            }
                        }
                    }
                } else {
                    let canon = canonicalize(&pattern);
                    if !excluded.contains(&canon) && seen.insert(canon.clone()) {
                        paths.push(pattern);
                    }
                }
            }
            // Stable order: lex by canonical path so builds are reproducible.
            paths.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));

            let ws_package = ws.package.clone().unwrap_or_default();
            let mut members: Vec<Member> = Vec::with_capacity(paths.len());
            for path in paths {
                let manifest = load_member_manifest(&path, &ws_package)?;
                let name = manifest
                    .pkg()
                    .with_context(|| {
                        format!("member at `{}` has no [package] table", path.display())
                    })?
                    .name
                    .clone();
                members.push(Member { path, manifest, name });
            }
            // Reject duplicate member names.
            let mut seen: std::collections::HashSet<&str> = Default::default();
            for m in &members {
                if !seen.insert(&m.name) {
                    bail!("duplicate workspace member name `{}`", m.name);
                }
            }
            let by_name = members
                .iter()
                .enumerate()
                .map(|(i, m)| (m.name.clone(), i))
                .collect();
            let ws_deps = ws.dependencies.clone();
            let mut workspace = Self { root: root_manifest_dir, members, by_name };
            workspace.resolve_inheritance(&ws_deps)?;
            Ok(workspace)
        } else {
            // Implicit single-member workspace.
            let pkg = root_manifest.pkg().with_context(|| {
                format!(
                    "manifest at `{}` has neither [package] nor [workspace]",
                    root_manifest_dir.display()
                )
            })?;
            let name = pkg.name.clone();
            let mut by_name = HashMap::new();
            by_name.insert(name.clone(), 0);
            Ok(Self {
                root: root_manifest_dir.clone(),
                members: vec![Member {
                    path: root_manifest_dir,
                    manifest: root_manifest,
                    name,
                }],
                by_name,
            })
        }
    }

    /// Walk every member and substitute `dep.workspace = true` entries with the
    /// matching value from `[workspace.dependencies]` at the workspace root.
    /// Member-side keys (`scope`, `classifier`, `type`, `exclude`, `optional`)
    /// override the workspace defaults.
    fn resolve_inheritance(&mut self, ws_deps: &BTreeMap<String, DepSpec>) -> Result<()> {
        for member in &mut self.members {
            substitute_dep_table(&member.name, &mut member.manifest.dependencies, ws_deps)?;
            substitute_dep_table(&member.name, &mut member.manifest.dev_dependencies, ws_deps)?;
        }
        Ok(())
    }

    pub fn is_explicit_workspace(&self) -> bool {
        self.members.len() > 1
            || self
                .members
                .first()
                .is_some_and(|m| m.manifest.workspace.is_some())
    }

    /// Build the path-dep DAG and return members in topological build order
    /// (deps first).
    pub fn topological_order(&self) -> Result<Vec<usize>> {
        // adjacency[u] = set of members u depends on (its path-deps)
        let n = self.members.len();
        let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut in_degree: Vec<usize> = vec![0; n];

        for (i, member) in self.members.iter().enumerate() {
            for (key, spec) in &member.manifest.dependencies {
                let Some(rel_path) = spec.path() else { continue };
                let target_dir = canonicalize(&member.path.join(rel_path));
                let dep_idx = self.members.iter().position(|m| {
                    canonicalize(&m.path) == target_dir
                });
                let dep_idx = match dep_idx {
                    Some(j) => j,
                    None => bail!(
                        "member `{}` depends on path `{}` (key `{key}`) \
                         which is not a workspace member",
                        member.name,
                        rel_path
                    ),
                };
                if dep_idx == i {
                    bail!("member `{}` cannot depend on itself", member.name);
                }
                adjacency[i].push(dep_idx);
                in_degree[i] += 1;
            }
        }

        // Kahn's algorithm.
        let mut queue: VecDeque<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
        let mut order: Vec<usize> = Vec::with_capacity(n);
        let mut remaining_in: Vec<usize> = in_degree.clone();
        // Reverse adjacency for decrementing in-degree on completion.
        let mut dependents_of: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (u, deps) in adjacency.iter().enumerate() {
            for &d in deps {
                dependents_of[d].push(u);
            }
        }

        while let Some(u) = queue.pop_front() {
            order.push(u);
            for &v in &dependents_of[u] {
                remaining_in[v] -= 1;
                if remaining_in[v] == 0 {
                    queue.push_back(v);
                }
            }
        }
        if order.len() != n {
            // Cycle: identify members still with in-degree > 0.
            let stuck: Vec<&str> = (0..n)
                .filter(|i| remaining_in[*i] > 0)
                .map(|i| self.members[i].name.as_str())
                .collect();
            bail!("path-dep cycle among members: [{}]", stuck.join(", "));
        }
        Ok(order)
    }

    /// Compute the closure of a member plus all its path-dep ancestors
    /// (transitive). Used by `-p`.
    pub fn closure(&self, root_idx: usize) -> Result<Vec<usize>> {
        let mut included: std::collections::HashSet<usize> = Default::default();
        let mut stack: Vec<usize> = vec![root_idx];
        while let Some(u) = stack.pop() {
            if !included.insert(u) {
                continue;
            }
            for (_, spec) in &self.members[u].manifest.dependencies {
                let Some(rel_path) = spec.path() else { continue };
                let target_dir = canonicalize(&self.members[u].path.join(rel_path));
                if let Some(j) = self
                    .members
                    .iter()
                    .position(|m| canonicalize(&m.path) == target_dir)
                {
                    stack.push(j);
                }
            }
        }
        // Filter the topological order to just the included members.
        let topo = self.topological_order()?;
        Ok(topo.into_iter().filter(|i| included.contains(i)).collect())
    }

    /// Generate (or refresh) the shared workspace `jet.lock` at the workspace
    /// root. Unions every member's `[dependencies]` + `[dev-dependencies]`,
    /// skipping path-deps and any entry still flagged `workspace = true`
    /// (resolve_inheritance should already have substituted those). Runs the
    /// resolver once over the union and writes to `<workspace_root>/jet.lock`.
    ///
    /// Detects cross-member version conflicts (same coord pinned to different
    /// versions) and bails with the offending members listed.
    pub fn ensure_lockfile(&self, fetcher: &Fetcher, force: bool) -> Result<Lockfile> {
        let lock_path = self.root.join(LOCKFILE_NAME);
        if !force && lock_path.is_file() {
            // Trust existing lock for this build invocation. A future revision
            // can hash member dep tables and compare against a stored input
            // hash for staleness detection.
            if let Ok(lf) = Lockfile::load(&self.root) {
                return Ok(lf);
            }
        }

        // Union every member's deps. Detect conflicts: same key with differing
        // version, classifier, or path-vs-Maven mismatch.
        let mut main: BTreeMap<String, (String, DepSpec)> = BTreeMap::new();
        let mut dev: BTreeMap<String, (String, DepSpec)> = BTreeMap::new();
        for member in &self.members {
            collect_into(&member.name, &member.manifest.dependencies, &mut main)?;
            collect_into(&member.name, &member.manifest.dev_dependencies, &mut dev)?;
        }

        if main.is_empty() && dev.is_empty() {
            // Empty workspace lock. Persist for consistency.
            let lf = Lockfile::from_resolution(
                "workspace",
                "0.0.0",
                &Default::default(),
                &default_repos()[0],
            );
            lf.save(&self.root)?;
            return Ok(lf);
        }

        // Build a synthetic manifest just to feed the existing resolver API.
        let synthetic = Manifest {
            package: Some(PackageMeta {
                name: "workspace_root".into(),
                version: "0.0.0".into(),
                java: 21,
                group: None,
                package: None,
                main: None,
                license: None,
                authors: vec![],
                description: None,
            }),
            workspace: None,
            dependencies: main.into_iter().map(|(k, (_, v))| (k, v)).collect(),
            dev_dependencies: dev.into_iter().map(|(k, (_, v))| (k, v)).collect(),
            repositories: BTreeMap::new(),
            build: Default::default(),
        };
        let resolution = resolve_with_dev(&synthetic, fetcher)?;
        let lf = Lockfile::from_resolution(
            "workspace",
            "0.0.0",
            &resolution,
            &default_repos()[0],
        );
        lf.save(&self.root)?;
        println!(
            "  Resolved workspace: {}",
            crate::resolver::resolve::summary(&resolution)
        );
        Ok(lf)
    }

    /// Walk every member directory and warn if a legacy per-member jet.lock
    /// exists alongside the new workspace-root lock. Returns the list of
    /// stale paths found (empty when clean).
    pub fn find_legacy_member_locks(&self) -> Vec<PathBuf> {
        self.members
            .iter()
            .map(|m| m.path.join(LOCKFILE_NAME))
            .filter(|p| p.is_file())
            .collect()
    }

    pub fn find_member(&self, name: &str) -> Result<usize> {
        self.by_name.get(name).copied().ok_or_else(|| {
            let names: Vec<&str> = self.members.iter().map(|m| m.name.as_str()).collect();
            anyhow::anyhow!(
                "no workspace member named `{name}` (members: [{}])",
                names.join(", ")
            )
        })
    }

    pub fn default_members(&self) -> Vec<usize> {
        let ws = match self.members.first().and_then(|m| m.manifest.workspace.as_ref()) {
            Some(ws) if !ws.default_members.is_empty() => ws,
            _ => return (0..self.members.len()).collect(),
        };
        ws.default_members
            .iter()
            .filter_map(|name| self.by_name.get(name).copied())
            .collect()
    }
}

fn canonicalize(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Load a workspace member's manifest, applying `[workspace.package]`
/// inheritance via TOML pre-processing before typed parsing.
fn load_member_manifest(path: &Path, ws_pkg: &WorkspacePackage) -> Result<Manifest> {
    let manifest_path = path.join(MANIFEST_FILENAME);
    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let processed = preprocess_inheritance(&raw, ws_pkg, &manifest_path)?;
    Manifest::from_str(&processed)
        .with_context(|| format!("parsing {}", manifest_path.display()))
}

/// Walk the `[package]` table in a member's TOML and replace any field set to
/// `{ workspace = true }` with the corresponding value from
/// `[workspace.package]`. Errors when the workspace value is missing or when
/// the inheritance marker is malformed.
fn preprocess_inheritance(
    raw: &str,
    ws_pkg: &WorkspacePackage,
    member_path: &Path,
) -> Result<String> {
    let mut doc: DocumentMut = raw
        .parse()
        .with_context(|| format!("parsing TOML in {}", member_path.display()))?;

    let pkg_table = match doc.get_mut("package") {
        Some(Item::Table(t)) => t,
        _ => return Ok(doc.to_string()), // virtual manifest or no [package]
    };

    // Inheritable fields: matches WorkspacePackage's keys.
    let fields: &[(&str, InheritKind)] = &[
        ("version", InheritKind::String),
        ("java", InheritKind::Integer),
        ("group", InheritKind::String),
        ("license", InheritKind::String),
        ("authors", InheritKind::StringArray),
        ("description", InheritKind::String),
    ];

    for (key, kind) in fields {
        let Some(item) = pkg_table.get(*key) else { continue };
        if !is_workspace_marker(item)? {
            continue;
        }
        let new_item = resolve_workspace_value(*key, *kind, ws_pkg, member_path)?;
        pkg_table.insert(*key, new_item);
    }
    Ok(doc.to_string())
}

#[derive(Clone, Copy)]
enum InheritKind {
    String,
    Integer,
    StringArray,
}

fn is_workspace_marker(item: &Item) -> Result<bool> {
    let table = match item {
        Item::Value(Value::InlineTable(t)) => {
            // Convert inline table to comparable form.
            let has_workspace = t.get("workspace").and_then(|v| v.as_bool()) == Some(true);
            let extra: Vec<&str> =
                t.iter().filter(|(k, _)| *k != "workspace").map(|(k, _)| k).collect();
            if has_workspace && !extra.is_empty() {
                bail!(
                    "`workspace = true` cannot be combined with other keys: {}",
                    extra.join(", ")
                );
            }
            return Ok(has_workspace);
        }
        Item::Table(t) => t,
        _ => return Ok(false),
    };
    let has_workspace = table.get("workspace").and_then(|i| match i {
        Item::Value(Value::Boolean(f)) => Some(*f.value()),
        _ => None,
    }) == Some(true);
    if has_workspace {
        let extras: Vec<&str> =
            table.iter().filter(|(k, _)| *k != "workspace").map(|(k, _)| k).collect();
        if !extras.is_empty() {
            bail!(
                "`workspace = true` cannot be combined with other keys: {}",
                extras.join(", ")
            );
        }
    }
    Ok(has_workspace)
}

fn resolve_workspace_value(
    key: &str,
    kind: InheritKind,
    ws_pkg: &WorkspacePackage,
    member_path: &Path,
) -> Result<Item> {
    let missing = || {
        anyhow::anyhow!(
            "in `{}`, `[package].{key}.workspace = true`, but the workspace root \
             has no `[workspace.package].{key}`.\n\
             help: add `{key} = ...` under `[workspace.package]` in the root jet.toml, \
             or set `{key}` directly in the member.",
            member_path.display()
        )
    };
    match (key, kind) {
        ("version", InheritKind::String) => {
            let v = ws_pkg.version.as_deref().ok_or_else(missing)?;
            Ok(toml_edit::value(v))
        }
        ("java", InheritKind::Integer) => {
            let v = ws_pkg.java.ok_or_else(missing)?;
            Ok(toml_edit::value(v as i64))
        }
        ("group", InheritKind::String) => {
            let v = ws_pkg.group.as_deref().ok_or_else(missing)?;
            Ok(toml_edit::value(v))
        }
        ("license", InheritKind::String) => {
            let v = ws_pkg.license.as_deref().ok_or_else(missing)?;
            Ok(toml_edit::value(v))
        }
        ("authors", InheritKind::StringArray) => {
            let v = ws_pkg.authors.as_ref().ok_or_else(missing)?;
            let mut arr = toml_edit::Array::new();
            for s in v {
                arr.push(s.as_str());
            }
            Ok(Item::Value(Value::Array(arr)))
        }
        ("description", InheritKind::String) => {
            let v = ws_pkg.description.as_deref().ok_or_else(missing)?;
            Ok(toml_edit::value(v))
        }
        _ => unreachable!("unknown inheritable key: {key}"),
    }
}

/// Merge a member's deps into the workspace-wide map, detecting cross-member
/// conflicts (same coord pinned to a different version by another member).
/// Path-dep and unsubstituted workspace-inherited entries are skipped — the
/// former are workspace-internal, the latter would have been resolved already.
fn collect_into(
    member: &str,
    src: &BTreeMap<String, DepSpec>,
    dest: &mut BTreeMap<String, (String, DepSpec)>,
) -> Result<()> {
    for (key, spec) in src {
        if spec.path().is_some() || spec.inherits_workspace() {
            continue;
        }
        match dest.get(key) {
            Some((prior_member, prior_spec)) => {
                let prior_v = prior_spec.version();
                let cur_v = spec.version();
                if prior_v != cur_v {
                    bail!(
                        "workspace dependency conflict on `{key}`: \
                         member `{prior_member}` requires {prior_v}, \
                         member `{member}` requires {cur_v}.\n\
                         help: pin a single version under [workspace.dependencies] \
                         and have both members use `{key}.workspace = true`."
                    );
                }
                // Versions match: keep the first occurrence (deterministic).
            }
            None => {
                dest.insert(key.clone(), (member.to_string(), spec.clone()));
            }
        }
    }
    Ok(())
}

fn substitute_dep_table(
    member: &str,
    deps: &mut BTreeMap<String, DepSpec>,
    ws_deps: &BTreeMap<String, DepSpec>,
) -> Result<()> {
    for (key, spec) in deps.iter_mut() {
        if !spec.inherits_workspace() {
            continue;
        }
        let ws_entry = ws_deps.get(key).ok_or_else(|| {
            anyhow::anyhow!(
                "member `{member}` declares `{key}.workspace = true`, but the \
                 workspace root has no `[workspace.dependencies].{key}`.\n\
                 help: add `\"{key}\" = \"<version>\"` under `[workspace.dependencies]` \
                 in the root jet.toml, or specify the version directly in the member."
            )
        })?;
        *spec = merge_workspace_dep(ws_entry, spec, key, member)?;
    }
    Ok(())
}

/// Merge a workspace-defined dep with the member-side overrides. Workspace
/// owns `version`; member can layer on `scope`, `classifier`, `type`,
/// `exclude`, `optional`. Source-defining fields (`path`) on member side are
/// rejected when `workspace = true`.
fn merge_workspace_dep(
    ws_entry: &DepSpec,
    member_spec: &DepSpec,
    key: &str,
    member_name: &str,
) -> Result<DepSpec> {
    // The workspace entry is the base; copy its concrete fields.
    let (base_version, base_classifier, base_ty, base_scope, base_exclude, base_optional) =
        match ws_entry {
            DepSpec::Version(v) => (Some(v.clone()), None, "jar".into(), None, vec![], false),
            DepSpec::Detailed(d) => (
                d.version.clone(),
                d.classifier.clone(),
                d.ty.clone().unwrap_or_else(|| "jar".into()),
                d.scope.clone(),
                d.exclude.clone(),
                d.optional,
            ),
        };
    if base_version.as_deref().map(str::trim).unwrap_or("").is_empty() {
        bail!(
            "[workspace.dependencies].{key} has no version; either add one or \
             remove the entry"
        );
    }
    // Member overrides.
    let DepSpec::Detailed(m) = member_spec else {
        // shouldn't happen: inherits_workspace() returned true
        return Ok(ws_entry.clone());
    };
    if m.path.is_some() {
        bail!(
            "member `{member_name}` cannot use `path` together with `workspace = true` \
             on `{key}`"
        );
    }
    let merged = DetailedDep {
        version: base_version,
        path: None,
        workspace: false, // resolved
        classifier: m.classifier.clone().or(base_classifier),
        ty: Some(m.ty.clone().unwrap_or(base_ty)),
        scope: m.scope.clone().or(base_scope),
        exclude: {
            // Union: workspace excludes + member excludes, dedup.
            let mut acc = base_exclude;
            for e in &m.exclude {
                if !acc.contains(e) {
                    acc.push(e.clone());
                }
            }
            acc
        },
        optional: m.optional || base_optional,
    };
    Ok(DepSpec::Detailed(merged))
}

/// Run a closure across workspace members in topological order, with up to
/// `jobs` of them executing concurrently. Members are kept in `order` and
/// the per-member dependency edges (member-idx → its path-dep ancestors)
/// determine readiness. The closure receives the member index and a list of
/// the indices of its already-finished path-dep ancestors so it can collect
/// their outputs without locking the shared map (callers pass in an
/// `Arc<Mutex<HashMap>>` of outputs they own).
///
/// Fails fast on the first error: stops dispatching new work and waits for
/// in-flight workers to drain before returning.
pub fn parallel_run<T, F>(
    workspace: &Workspace,
    order: &[usize],
    jobs: usize,
    work: F,
) -> Result<std::collections::HashMap<usize, T>>
where
    T: Send + 'static,
    F: Fn(usize) -> Result<T> + Send + Sync + 'static,
{
    use std::sync::{Arc, Mutex};
    use std::sync::atomic::{AtomicBool, Ordering};

    let n = order.len();
    if n == 0 {
        return Ok(Default::default());
    }

    // Per-member in-degree (count of path-dep ancestors that must finish first).
    let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); workspace.members.len()];
    let mut in_degree: Vec<usize> = vec![0; workspace.members.len()];
    let included: std::collections::HashSet<usize> = order.iter().copied().collect();
    for &i in order {
        for (_, spec) in &workspace.members[i].manifest.dependencies {
            let Some(rel) = spec.path() else { continue };
            let target = canonicalize(&workspace.members[i].path.join(rel));
            if let Some(j) = workspace.members.iter().position(|m| canonicalize(&m.path) == target) {
                if included.contains(&j) {
                    adjacency[j].push(i); // j -> i (j unlocks i)
                    in_degree[i] += 1;
                }
            }
        }
    }

    let work = Arc::new(work);
    let outputs: Arc<Mutex<std::collections::HashMap<usize, T>>> =
        Arc::new(Mutex::new(std::collections::HashMap::with_capacity(n)));
    let in_degree = Arc::new(Mutex::new(in_degree));
    let cancel = Arc::new(AtomicBool::new(false));
    let first_error: Arc<Mutex<Option<anyhow::Error>>> = Arc::new(Mutex::new(None));

    // Channels: ready queue (coordinator → workers), completion (workers → coordinator).
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<usize>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<(usize, Result<T>)>();
    let ready_rx = Arc::new(Mutex::new(ready_rx));

    // Seed initially-ready members.
    let mut remaining = n;
    {
        let deg = in_degree.lock().unwrap();
        for &i in order {
            if deg[i] == 0 {
                ready_tx.send(i).expect("ready queue open");
            }
        }
    }

    let n_workers = jobs.max(1);
    let mut handles = Vec::with_capacity(n_workers);
    for _ in 0..n_workers {
        let rx = Arc::clone(&ready_rx);
        let tx = done_tx.clone();
        let work = Arc::clone(&work);
        let cancel = Arc::clone(&cancel);
        handles.push(std::thread::spawn(move || {
            loop {
                let next = {
                    let rx = rx.lock().unwrap();
                    rx.recv()
                };
                let idx = match next {
                    Ok(i) => i,
                    Err(_) => break,
                };
                if cancel.load(Ordering::Relaxed) {
                    // Drain quietly without running.
                    continue;
                }
                let result = work(idx);
                if tx.send((idx, result)).is_err() {
                    break;
                }
            }
        }));
    }
    drop(done_tx);

    while remaining > 0 {
        let (idx, result) = match done_rx.recv() {
            Ok(v) => v,
            Err(_) => break,
        };
        remaining -= 1;
        match result {
            Ok(out) => {
                outputs.lock().unwrap().insert(idx, out);
                // Decrement in-degrees of dependents.
                let mut deg = in_degree.lock().unwrap();
                for &dep in &adjacency[idx] {
                    deg[dep] = deg[dep].saturating_sub(1);
                    if deg[dep] == 0 && !cancel.load(Ordering::Relaxed) {
                        let _ = ready_tx.send(dep);
                    }
                }
            }
            Err(e) => {
                cancel.store(true, Ordering::Relaxed);
                let mut slot = first_error.lock().unwrap();
                if slot.is_none() {
                    *slot = Some(e);
                }
            }
        }
    }
    drop(ready_tx);

    for h in handles {
        let _ = h.join();
    }

    if let Some(e) = first_error.lock().unwrap().take() {
        return Err(e);
    }
    let map = Arc::try_unwrap(outputs)
        .map_err(|_| anyhow::anyhow!("internal: outputs Arc not unique"))?
        .into_inner()
        .map_err(|_| anyhow::anyhow!("internal: outputs Mutex poisoned"))?;
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, contents: &str) {
        fs::create_dir_all(dir.parent().unwrap()).unwrap();
        fs::write(dir, contents).unwrap();
    }

    #[test]
    fn implicit_single_member_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            "[package]\nname = \"solo\"\nversion = \"0.1.0\"\njava = 21\n",
        );
        let ws = Workspace::discover(tmp.path()).unwrap();
        assert_eq!(ws.members.len(), 1);
        assert_eq!(ws.members[0].name, "solo");
        assert!(!ws.is_explicit_workspace());
    }

    #[test]
    fn explicit_workspace_topo_order() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            "[workspace]\nmembers = [\"core\", \"api\", \"cli\"]\n",
        );
        write(
            &tmp.path().join("core/jet.toml"),
            "[package]\nname = \"core\"\nversion = \"0.1.0\"\njava = 21\n",
        );
        write(
            &tmp.path().join("api/jet.toml"),
            "[package]\nname = \"api\"\nversion = \"0.1.0\"\njava = 21\n\n\
             [dependencies]\n\"local:core\" = { path = \"../core\" }\n",
        );
        write(
            &tmp.path().join("cli/jet.toml"),
            "[package]\nname = \"cli\"\nversion = \"0.1.0\"\njava = 21\n\n\
             [dependencies]\n\"local:api\" = { path = \"../api\" }\n",
        );
        let ws = Workspace::discover(tmp.path()).unwrap();
        assert_eq!(ws.members.len(), 3);
        let order: Vec<&str> = ws
            .topological_order()
            .unwrap()
            .into_iter()
            .map(|i| ws.members[i].name.as_str())
            .collect();
        // core must come before api, api before cli.
        let pos = |n: &str| order.iter().position(|&s| s == n).unwrap();
        assert!(pos("core") < pos("api"));
        assert!(pos("api") < pos("cli"));
    }

    #[test]
    fn detects_path_dep_cycle() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            "[workspace]\nmembers = [\"a\", \"b\"]\n",
        );
        write(
            &tmp.path().join("a/jet.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\njava = 21\n\n\
             [dependencies]\n\"x:b\" = { path = \"../b\" }\n",
        );
        write(
            &tmp.path().join("b/jet.toml"),
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\njava = 21\n\n\
             [dependencies]\n\"x:a\" = { path = \"../a\" }\n",
        );
        let ws = Workspace::discover(tmp.path()).unwrap();
        let err = ws.topological_order().expect_err("should detect cycle");
        let msg = format!("{err:#}");
        assert!(msg.contains("cycle"), "expected cycle error, got: {msg}");
    }

    #[test]
    fn workspace_dependencies_inheritance_basic() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            r#"[workspace]
members = ["a"]

[workspace.dependencies]
"org.slf4j:slf4j-api" = "2.0.13"
"#,
        );
        write(
            &tmp.path().join("a/jet.toml"),
            r#"[package]
name    = "a"
version = "0.1.0"
java    = 21

[dependencies]
"org.slf4j:slf4j-api".workspace = true
"#,
        );
        let ws = Workspace::discover(tmp.path()).unwrap();
        let dep = &ws.members[0].manifest.dependencies["org.slf4j:slf4j-api"];
        assert_eq!(dep.version(), "2.0.13");
        assert!(!dep.inherits_workspace(), "should be substituted post-discover");
    }

    #[test]
    fn workspace_dependencies_inheritance_with_scope_override() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            r#"[workspace]
members = ["a"]

[workspace.dependencies]
"org.junit.jupiter:junit-jupiter" = "5.10.2"
"#,
        );
        write(
            &tmp.path().join("a/jet.toml"),
            r#"[package]
name    = "a"
version = "0.1.0"
java    = 21

[dev-dependencies]
"org.junit.jupiter:junit-jupiter" = { workspace = true, scope = "test" }
"#,
        );
        let ws = Workspace::discover(tmp.path()).unwrap();
        let dep = &ws.members[0].manifest.dev_dependencies["org.junit.jupiter:junit-jupiter"];
        assert_eq!(dep.version(), "5.10.2");
        match dep {
            crate::manifest::DepSpec::Detailed(d) => {
                assert_eq!(d.scope.as_deref(), Some("test"));
            }
            _ => panic!("expected detailed"),
        }
    }

    #[test]
    fn workspace_dependencies_missing_workspace_entry_errors() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            r#"[workspace]
members = ["a"]
"#,
        );
        write(
            &tmp.path().join("a/jet.toml"),
            r#"[package]
name    = "a"
version = "0.1.0"
java    = 21

[dependencies]
"org.slf4j:slf4j-api".workspace = true
"#,
        );
        let err = Workspace::discover(tmp.path()).err().expect("should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("workspace.dependencies"), "got: {msg}");
        assert!(msg.contains("org.slf4j:slf4j-api"), "got: {msg}");
    }

    #[test]
    fn workspace_inherited_with_explicit_version_errors() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            r#"[workspace]
members = ["a"]

[workspace.dependencies]
"org.slf4j:slf4j-api" = "2.0.13"
"#,
        );
        write(
            &tmp.path().join("a/jet.toml"),
            r#"[package]
name    = "a"
version = "0.1.0"
java    = 21

[dependencies]
"org.slf4j:slf4j-api" = { workspace = true, version = "1.7.36" }
"#,
        );
        let err = Workspace::discover(tmp.path()).err().expect("should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("workspace = true") && msg.contains("version"), "got: {msg}");
    }

    #[test]
    fn workspace_package_inherits_version_and_java() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            r#"[workspace]
members = ["a", "b"]

[workspace.package]
version = "1.2.3"
java    = 21
group   = "io.example"
license = "Apache-2.0"
"#,
        );
        for name in ["a", "b"] {
            write(
                &tmp.path().join(name).join("jet.toml"),
                &format!(
                    r#"[package]
name    = "{name}"
version.workspace = true
java.workspace    = true
group.workspace   = true
license.workspace = true
"#
                ),
            );
        }
        let ws = Workspace::discover(tmp.path()).unwrap();
        for m in &ws.members {
            let p = m.manifest.pkg().unwrap();
            assert_eq!(p.version, "1.2.3");
            assert_eq!(p.java, 21);
            assert_eq!(p.group.as_deref(), Some("io.example"));
            assert_eq!(p.license.as_deref(), Some("Apache-2.0"));
        }
    }

    #[test]
    fn workspace_package_inherits_authors_array() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            r#"[workspace]
members = ["a"]

[workspace.package]
version = "0.1.0"
java    = 21
authors = ["Ada <ada@example.org>", "Linus <linus@example.org>"]
"#,
        );
        write(
            &tmp.path().join("a/jet.toml"),
            r#"[package]
name              = "a"
version.workspace = true
java.workspace    = true
authors.workspace = true
"#,
        );
        let ws = Workspace::discover(tmp.path()).unwrap();
        let p = ws.members[0].manifest.pkg().unwrap();
        assert_eq!(
            p.authors,
            vec![
                "Ada <ada@example.org>".to_string(),
                "Linus <linus@example.org>".to_string()
            ]
        );
    }

    #[test]
    fn workspace_package_member_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            r#"[workspace]
members = ["a"]

[workspace.package]
version = "1.0.0"
java    = 21
"#,
        );
        write(
            &tmp.path().join("a/jet.toml"),
            r#"[package]
name              = "a"
version.workspace = true
java              = 17
"#,
        );
        let ws = Workspace::discover(tmp.path()).unwrap();
        let p = ws.members[0].manifest.pkg().unwrap();
        assert_eq!(p.version, "1.0.0", "version inherited");
        assert_eq!(p.java, 17, "java overridden locally");
    }

    #[test]
    fn workspace_package_missing_field_errors() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            r#"[workspace]
members = ["a"]

[workspace.package]
java = 21
"#,
        );
        write(
            &tmp.path().join("a/jet.toml"),
            r#"[package]
name              = "a"
version.workspace = true
java.workspace    = true
"#,
        );
        let err = Workspace::discover(tmp.path()).err().expect("should fail");
        let msg = format!("{err:#}");
        assert!(msg.contains("workspace.package"), "got: {msg}");
        assert!(msg.contains("version"), "got: {msg}");
    }

    #[test]
    fn workspace_marker_rejects_extra_keys() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            r#"[workspace]
members = ["a"]

[workspace.package]
version = "1.0.0"
"#,
        );
        write(
            &tmp.path().join("a/jet.toml"),
            r#"[package]
name    = "a"
version = { workspace = true, junk = "x" }
java    = 21
"#,
        );
        let err = Workspace::discover(tmp.path()).err().expect("should fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("workspace = true") && msg.contains("junk"),
            "got: {msg}"
        );
    }

    #[test]
    fn closure_includes_only_path_ancestors() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            &tmp.path().join("jet.toml"),
            "[workspace]\nmembers = [\"core\", \"api\", \"cli\", \"unrelated\"]\n",
        );
        write(
            &tmp.path().join("core/jet.toml"),
            "[package]\nname = \"core\"\nversion = \"0.1.0\"\njava = 21\n",
        );
        write(
            &tmp.path().join("api/jet.toml"),
            "[package]\nname = \"api\"\nversion = \"0.1.0\"\njava = 21\n\n\
             [dependencies]\n\"x:core\" = { path = \"../core\" }\n",
        );
        write(
            &tmp.path().join("cli/jet.toml"),
            "[package]\nname = \"cli\"\nversion = \"0.1.0\"\njava = 21\n\n\
             [dependencies]\n\"x:api\" = { path = \"../api\" }\n",
        );
        write(
            &tmp.path().join("unrelated/jet.toml"),
            "[package]\nname = \"unrelated\"\nversion = \"0.1.0\"\njava = 21\n",
        );
        let ws = Workspace::discover(tmp.path()).unwrap();
        let cli_idx = ws.find_member("cli").unwrap();
        let names: Vec<&str> = ws
            .closure(cli_idx)
            .unwrap()
            .into_iter()
            .map(|i| ws.members[i].name.as_str())
            .collect();
        assert!(names.contains(&"core"));
        assert!(names.contains(&"api"));
        assert!(names.contains(&"cli"));
        assert!(!names.contains(&"unrelated"));
    }
}
