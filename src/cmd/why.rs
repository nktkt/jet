//! `jet why <coord>` — explain why a coord ended up in the resolved graph.
//!
//! Walks every path from a manifest root through the cached POMs to the
//! requested `group:artifact`. Prints each path plus the version finally
//! chosen by nearest-wins (read straight off `jet.lock`).

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{Context, Result, bail};

use crate::coord::Coord;
use crate::lockfile::{LOCKFILE_NAME, Lockfile, Package};
use crate::manifest::Manifest;
use crate::resolver::{Fetcher, default_repos};

pub struct WhyArgs {
    /// `group:artifact` (or `group:artifact:version` — version ignored).
    pub coord: String,
}

pub fn cmd_why(args: WhyArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = Manifest::find_root(&cwd)?;
    let manifest = Manifest::load(&root)?;
    let lock_path = root.join(LOCKFILE_NAME);
    if !lock_path.is_file() {
        bail!("no `jet.lock`; run `jet build` first.");
    }
    let lockfile = Lockfile::load(&root)?;

    let (target_group, target_artifact) = parse_target(&args.coord)?;

    // Index lockfile by (group, artifact).
    let mut by_ga: HashMap<(String, String), &Package> = HashMap::new();
    for p in &lockfile.packages {
        if let Some((g, a)) = p.name.split_once(':') {
            by_ga.insert((g.into(), a.into()), p);
        }
    }

    let chosen = by_ga.get(&(target_group.clone(), target_artifact.clone()));

    // BFS from each root, recording the path. Stop at the first match per
    // root. Multi-edge to same target is reported once per root (the
    // shortest path each root sees), to avoid swamping output.
    let fetcher = Fetcher::new(default_repos())?;
    let mut paths: Vec<Vec<String>> = Vec::new();

    let mut roots: Vec<(String, String, &'static str)> = Vec::new();
    for k in manifest.dependencies.keys() {
        if let Some((g, a)) = k.split_once(':') {
            roots.push((g.into(), a.into(), "main"));
        }
    }
    for k in manifest.dev_dependencies.keys() {
        if let Some((g, a)) = k.split_once(':') {
            roots.push((g.into(), a.into(), "dev"));
        }
    }

    let project_label = format!(
        "{} v{}",
        manifest.pkg()?.name,
        manifest.pkg()?.version
    );

    for (rg, ra, origin) in &roots {
        if rg == &target_group && ra == &target_artifact {
            paths.push(vec![project_label.clone(), format!("{rg}:{ra} (root, {origin})")]);
            continue;
        }
        if let Some(path) = bfs_path(
            rg,
            ra,
            &target_group,
            &target_artifact,
            &fetcher,
            &by_ga,
        )? {
            let mut full = vec![project_label.clone()];
            full.extend(path);
            paths.push(full);
        }
    }

    if paths.is_empty() {
        if chosen.is_some() {
            println!(
                "{}:{} is in jet.lock but no path was found from any manifest root.\n\
                 (it may have come from a dev-dep, BOM, or transitive that's hidden)",
                target_group, target_artifact
            );
        } else {
            println!(
                "{}:{} is not in jet.lock — nothing in this project depends on it.",
                target_group, target_artifact
            );
        }
        return Ok(());
    }

    println!("{}:{}", target_group, target_artifact);
    if let Some(p) = chosen {
        println!("  selected: {}", p.version);
        println!("  origin:   {}, scope: {}", p.origin, p.scope);
    }
    println!();
    for (i, path) in paths.iter().enumerate() {
        println!("path {} ({} hops):", i + 1, path.len() - 1);
        for (j, hop) in path.iter().enumerate() {
            let connector = if j == 0 { "  " } else { "  → " };
            println!("{connector}{hop}");
        }
        println!();
    }
    Ok(())
}

fn parse_target(s: &str) -> Result<(String, String)> {
    let mut parts = s.splitn(3, ':');
    let g = parts.next().filter(|s| !s.is_empty());
    let a = parts.next().filter(|s| !s.is_empty());
    match (g, a) {
        (Some(g), Some(a)) => Ok((g.to_string(), a.to_string())),
        _ => bail!("expected `group:artifact[:version]`, got `{s}`"),
    }
}

/// BFS through cached POMs from `(start_g, start_a)` toward
/// `(target_g, target_a)`. Returns the first reaching path as a list of
/// `group:artifact:version` strings, or `None`.
fn bfs_path(
    start_g: &str,
    start_a: &str,
    target_g: &str,
    target_a: &str,
    fetcher: &Fetcher,
    by_ga: &HashMap<(String, String), &Package>,
) -> Result<Option<Vec<String>>> {
    #[derive(Clone)]
    struct Node {
        g: String,
        a: String,
        path: Vec<String>,
    }
    let start_pkg = match by_ga.get(&(start_g.into(), start_a.into())) {
        Some(p) => p,
        None => return Ok(None),
    };
    let mut q: VecDeque<Node> = VecDeque::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    seen.insert((start_g.into(), start_a.into()));
    q.push_back(Node {
        g: start_g.into(),
        a: start_a.into(),
        path: vec![format!("{}:{}:{}", start_g, start_a, start_pkg.version)],
    });

    while let Some(node) = q.pop_front() {
        if node.g == target_g && node.a == target_a {
            return Ok(Some(node.path));
        }
        let pkg = match by_ga.get(&(node.g.clone(), node.a.clone())) {
            Some(p) => *p,
            None => continue,
        };
        let coord = Coord {
            group: node.g.clone(),
            artifact: node.a.clone(),
            version: pkg.version.clone(),
            classifier: pkg.classifier.clone(),
            ty: pkg.ty.clone(),
        };
        let pom_bytes = match fetcher.fetch_pom(&coord) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let pom = crate::resolver::pom::Pom::parse(&pom_bytes)
            .with_context(|| format!("parsing POM for {coord}"))?;
        for child in &pom.dependencies {
            if child.optional {
                continue;
            }
            let scope = child.scope.as_deref().unwrap_or("compile");
            if matches!(scope, "test" | "provided" | "system" | "import") {
                continue;
            }
            let g = pom.interpolate(&child.group_id, &HashMap::new());
            let a = pom.interpolate(&child.artifact_id, &HashMap::new());
            if g.is_empty() || a.is_empty() {
                continue;
            }
            if !seen.insert((g.clone(), a.clone())) {
                continue;
            }
            // Use chosen version from lockfile.
            let child_pkg = match by_ga.get(&(g.clone(), a.clone())) {
                Some(p) => *p,
                None => continue,
            };
            let mut path = node.path.clone();
            path.push(format!("{g}:{a}:{}", child_pkg.version));
            q.push_back(Node { g, a, path });
        }
    }
    Ok(None)
}
