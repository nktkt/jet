//! `jet tree` — print the resolved dependency tree.
//!
//! Reads `jet.lock` (the resolver's output) plus each cached POM (the
//! resolver's input) to reconstruct the full edge graph. Each `[[package]]`
//! in the lockfile is the chosen version after nearest-wins resolution; the
//! POMs tell us which children each one declared. We render a box-drawing
//! tree mirroring `cargo tree` / `mvn dependency:tree`.

use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result, bail};

use crate::coord::Coord;
use crate::lockfile::{LOCKFILE_NAME, Lockfile, Package};
use crate::manifest::{DepSpec, Manifest};
use crate::resolver::{Fetcher, default_repos};

pub struct TreeArgs {
    /// Restrict to deps with this scope (`compile`, `runtime`, `test`).
    pub scope: Option<String>,
}

pub fn cmd_tree(args: TreeArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = Manifest::find_root(&cwd)?;
    let manifest = Manifest::load(&root)?;
    let lock_path = root.join(LOCKFILE_NAME);
    if !lock_path.is_file() {
        bail!(
            "no `jet.lock` at `{}`; run `jet build` first to populate the lockfile.",
            root.display()
        );
    }
    let lockfile = Lockfile::load(&root)?;

    // Index lockfile by `(group, artifact)` so children resolve to the
    // chosen version regardless of what each parent declared.
    let mut by_ga: HashMap<(String, String), &Package> = HashMap::new();
    for p in &lockfile.packages {
        if let Some((g, a)) = p.name.split_once(':') {
            by_ga.insert((g.into(), a.into()), p);
        }
    }

    // Gather the manifest's direct deps as roots.
    let mut roots: Vec<RootDep> = Vec::new();
    for (key, spec) in &manifest.dependencies {
        if let Some((g, a)) = key.split_once(':') {
            roots.push(RootDep {
                group: g.into(),
                artifact: a.into(),
                origin: "main",
                manifest_scope: scope_from_spec(spec),
            });
        }
    }
    for (key, spec) in &manifest.dev_dependencies {
        if let Some((g, a)) = key.split_once(':') {
            roots.push(RootDep {
                group: g.into(),
                artifact: a.into(),
                origin: "dev",
                manifest_scope: scope_from_spec(spec),
            });
        }
    }

    let fetcher = Fetcher::new(default_repos())?;

    println!(
        "{} v{}",
        manifest.pkg()?.name,
        manifest.pkg()?.version
    );
    let n = roots.len();
    for (i, root_dep) in roots.iter().enumerate() {
        let last = i + 1 == n;
        print_branch(
            root_dep,
            &by_ga,
            &fetcher,
            "",
            last,
            args.scope.as_deref(),
        )?;
    }
    Ok(())
}

struct RootDep {
    group: String,
    artifact: String,
    origin: &'static str,
    manifest_scope: String,
}

fn scope_from_spec(spec: &DepSpec) -> String {
    match spec {
        DepSpec::Version(_) => "compile".into(),
        DepSpec::Detailed(d) => d.scope.clone().unwrap_or_else(|| "compile".into()),
    }
}

fn print_branch(
    root: &RootDep,
    by_ga: &HashMap<(String, String), &Package>,
    fetcher: &Fetcher,
    prefix: &str,
    last: bool,
    scope_filter: Option<&str>,
) -> Result<()> {
    let key = (root.group.clone(), root.artifact.clone());
    let Some(pkg) = by_ga.get(&key).copied() else {
        // Lockfile didn't pick this dep up; show as unresolved.
        let connector = if last { "└── " } else { "├── " };
        println!(
            "{prefix}{connector}{}:{}  (not in lockfile — run jet build)",
            root.group, root.artifact
        );
        return Ok(());
    };
    if let Some(filter) = scope_filter {
        if pkg.scope != filter && root.origin != filter {
            return Ok(());
        }
    }
    let connector = if last { "└── " } else { "├── " };
    let scope_tag = if root.origin == "dev" {
        " (dev)".to_string()
    } else if pkg.scope != "compile" {
        format!(" ({})", pkg.scope)
    } else {
        String::new()
    };
    println!(
        "{prefix}{connector}{}:{}:{}{scope_tag}",
        pkg.name.split_once(':').map(|x| x.0).unwrap_or(""),
        pkg.name.split_once(':').map(|x| x.1).unwrap_or(""),
        pkg.version
    );

    // Recurse over the POM's declared dependencies (chosen version is what
    // ended up in the lockfile, even if this parent declared something else).
    let coord = Coord {
        group: root.group.clone(),
        artifact: root.artifact.clone(),
        version: pkg.version.clone(),
        classifier: pkg.classifier.clone(),
        ty: pkg.ty.clone(),
    };
    let pom_bytes = fetcher.fetch_pom(&coord)
        .with_context(|| format!("fetching POM for {coord}"))?;
    let pom = crate::resolver::pom::Pom::parse(&pom_bytes)?;
    let pom = crate::resolver::resolve::resolve_pom_chain(pom, fetcher)?;

    // Collect children that propagate transitively (compile/runtime, non-optional,
    // not workspace-test scope).
    let mut children: Vec<RootDep> = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for child in &pom.dependencies {
        if child.optional {
            continue;
        }
        let child_scope = child.scope.as_deref().unwrap_or("compile");
        if matches!(child_scope, "test" | "provided" | "system" | "import") {
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
        children.push(RootDep {
            group: g,
            artifact: a,
            origin: root.origin,
            manifest_scope: child_scope.into(),
        });
    }

    let new_prefix = format!("{prefix}{}", if last { "    " } else { "│   " });
    let cn = children.len();
    for (i, child) in children.iter().enumerate() {
        print_branch(child, by_ga, fetcher, &new_prefix, i + 1 == cn, scope_filter)?;
    }
    let _ = root.manifest_scope;
    Ok(())
}
