use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use crate::javac::{CompileSpec, compile, find_javac};
use crate::lockfile::{LOCKFILE_NAME, Lockfile};
use crate::manifest::Manifest;
use crate::resolver::{Fetcher, Resolution, default_repos, resolve as resolve_deps};
use crate::workspace::Workspace;

pub struct BuildArgs {
    pub release: bool,
    /// Force re-resolution of dependencies (ignore jet.lock).
    pub force_resolve: bool,
    /// Limit to a single workspace member (and its path-dep ancestors).
    pub package: Option<String>,
    /// Number of parallel build jobs (default: available_parallelism).
    pub jobs: Option<usize>,
}

/// Build outputs (paths the caller can use). Returned to `run` so it can
/// reuse the classpath without redoing the work.
pub struct BuildOutputs {
    pub project_root: PathBuf,
    /// Root of the shared `target/` directory (workspace root in workspace
    /// mode; project root in single-package mode).
    pub target_dir: PathBuf,
    pub manifest: Manifest,
    pub classes_dir: PathBuf,
    pub classpath: Vec<PathBuf>,
}

pub fn cmd_build(args: BuildArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let workspace = Workspace::discover(&cwd)?;

    if !workspace.is_explicit_workspace() {
        do_build(args)?;
        return Ok(());
    }

    let order = workspace_build_order(&workspace, args.package.as_deref())?;
    let target_dir = workspace.root.join("target");

    // Generate the shared workspace lockfile once before any per-member build
    // touches it. Members will load from <workspace_root>/jet.lock.
    let any_maven_deps = workspace
        .members
        .iter()
        .any(|m| !m.manifest.dependencies.is_empty() || !m.manifest.dev_dependencies.is_empty());
    if any_maven_deps {
        let fetcher = Fetcher::new(default_repos())?;
        workspace.ensure_lockfile(&fetcher, args.force_resolve)?;
    }
    // Warn about legacy per-member jet.lock files (left over from v0.5.x).
    let legacy = workspace.find_legacy_member_locks();
    if !legacy.is_empty() {
        eprintln!("  warning: legacy per-member jet.lock files detected (now superseded by the workspace lock at {}):", workspace.root.join("jet.lock").display());
        for p in &legacy {
            eprintln!("    {}", p.display());
        }
        eprintln!("  help: delete them; the workspace lock at the root is authoritative.");
    }

    let jobs = args.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    });
    if jobs > 1 && order.len() > 1 {
        run_parallel(&workspace, order, target_dir, args.release, args.force_resolve, jobs)
    } else {
        run_sequential(&workspace, order, target_dir, args.release, args.force_resolve)
    }
}

fn run_sequential(
    workspace: &crate::workspace::Workspace,
    order: Vec<usize>,
    target_dir: PathBuf,
    release: bool,
    force_resolve: bool,
) -> Result<()> {
    let mut built: HashMap<usize, BuildOutputs> = HashMap::new();
    for idx in order {
        let extra = collect_path_dep_classpath(workspace, idx, &built);
        let member = &workspace.members[idx];
        let outputs = do_build_at(
            member.path.clone(),
            member.manifest.clone(),
            target_dir.clone(),
            Some(member.name.clone()),
            release,
            force_resolve,
            extra,
            Some(workspace.root.clone()),
        )?;
        built.insert(idx, outputs);
    }
    Ok(())
}

fn run_parallel(
    workspace: &crate::workspace::Workspace,
    order: Vec<usize>,
    target_dir: PathBuf,
    release: bool,
    force_resolve: bool,
    jobs: usize,
) -> Result<()> {
    use std::sync::{Arc, Mutex};
    let built: Arc<Mutex<HashMap<usize, BuildOutputs>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Snapshot Arc-friendly references for the worker closure.
    let workspace_root = workspace.root.clone();
    // Pre-clone manifests + paths so the closure doesn't borrow `workspace`.
    let members: Vec<(PathBuf, crate::manifest::Manifest, String)> = workspace
        .members
        .iter()
        .map(|m| (m.path.clone(), m.manifest.clone(), m.name.clone()))
        .collect();
    let members = Arc::new(members);
    // Pre-compute the path-dep ancestor index for each member (transitive).
    let n = workspace.members.len();
    let mut transitive_path_deps: Vec<Vec<usize>> = vec![Vec::new(); n];
    for i in 0..n {
        let mut stack = vec![i];
        let mut seen = std::collections::HashSet::new();
        while let Some(u) = stack.pop() {
            if !seen.insert(u) {
                continue;
            }
            for (_, spec) in &workspace.members[u].manifest.dependencies {
                let Some(rel) = spec.path() else { continue };
                let target = workspace.members[u]
                    .path
                    .join(rel)
                    .canonicalize()
                    .unwrap_or_else(|_| workspace.members[u].path.join(rel));
                if let Some(j) = workspace.members.iter().position(|m| {
                    m.path.canonicalize().unwrap_or_else(|_| m.path.clone()) == target
                }) {
                    if j != i {
                        stack.push(j);
                        if u == i {
                            // Direct ancestors counted at user level; transitives at scheduling.
                        }
                        transitive_path_deps[i].push(j);
                    }
                }
            }
        }
    }
    let transitive_path_deps = Arc::new(transitive_path_deps);
    let workspace_root_arc = Arc::new(workspace_root);

    let built_for_worker = Arc::clone(&built);
    let members_for_worker = Arc::clone(&members);
    let transitive_for_worker = Arc::clone(&transitive_path_deps);
    let target_dir_arc = Arc::new(target_dir);
    let workspace_root_for_worker = Arc::clone(&workspace_root_arc);
    let _ = crate::workspace::parallel_run::<(), _>(
        workspace,
        &order,
        jobs,
        move |idx| {
            // Collect extras from already-built ancestors.
            let extras: Vec<PathBuf> = {
                let map = built_for_worker.lock().unwrap();
                let mut acc: Vec<PathBuf> = Vec::new();
                for &j in &transitive_for_worker[idx] {
                    if let Some(out) = map.get(&j) {
                        for p in &out.classpath {
                            if !acc.contains(p) {
                                acc.push(p.clone());
                            }
                        }
                    }
                }
                acc
            };
            let (path, manifest, name) = members_for_worker[idx].clone();
            let outputs = do_build_at(
                path,
                manifest,
                (*target_dir_arc).clone(),
                Some(name),
                release,
                force_resolve,
                extras,
                Some((*workspace_root_for_worker).clone()),
            )?;
            built_for_worker.lock().unwrap().insert(idx, outputs);
            Ok(())
        },
    )?;
    Ok(())
}

/// Compute the topological build order for a workspace. With `package` set,
/// scopes to the closure of that member and its path-dep ancestors.
fn workspace_build_order(
    workspace: &Workspace,
    package: Option<&str>,
) -> Result<Vec<usize>> {
    if let Some(name) = package {
        let idx = workspace.find_member(name)?;
        return workspace.closure(idx);
    }
    let topo = workspace.topological_order()?;
    let defaults = workspace.default_members();
    let allowed: std::collections::HashSet<usize> = defaults.into_iter().collect();
    // Always include path-dep ancestors of any default member, even if not
    // themselves in default_members — otherwise we can't compile the target.
    let mut needed: std::collections::HashSet<usize> = Default::default();
    for &i in &topo {
        if allowed.contains(&i) {
            for j in workspace.closure(i)? {
                needed.insert(j);
            }
        }
    }
    Ok(topo.into_iter().filter(|i| needed.contains(i)).collect())
}

/// Collect the classpath additions for a member that has path deps. Each
/// path-dep contributes its compiled `target/classes` directory plus its
/// transitively-resolved Maven JARs (already fetched during its own build).
fn collect_path_dep_classpath(
    workspace: &Workspace,
    member_idx: usize,
    built: &HashMap<usize, BuildOutputs>,
) -> Vec<PathBuf> {
    let mut extras: Vec<PathBuf> = Vec::new();
    let mut stack: Vec<usize> = vec![member_idx];
    let mut seen: std::collections::HashSet<usize> = Default::default();
    seen.insert(member_idx);
    while let Some(u) = stack.pop() {
        for (_, spec) in &workspace.members[u].manifest.dependencies {
            let Some(rel) = spec.path() else { continue };
            let target_dir = workspace.members[u].path.join(rel);
            let canonical = target_dir.canonicalize().unwrap_or(target_dir);
            let Some(j) = workspace.members.iter().position(|m| {
                m.path.canonicalize().unwrap_or_else(|_| m.path.clone()) == canonical
            }) else {
                continue;
            };
            if seen.insert(j) {
                stack.push(j);
                if let Some(outputs) = built.get(&j) {
                    // Member's classpath = its classes_dir + its dep jars.
                    for p in &outputs.classpath {
                        if !extras.contains(p) {
                            extras.push(p.clone());
                        }
                    }
                }
            }
        }
    }
    extras
}

/// Single-project build (walks up from cwd to find `jet.toml`).
pub fn do_build(args: BuildArgs) -> Result<BuildOutputs> {
    let cwd = std::env::current_dir()?;
    let root = Manifest::find_root(&cwd)?;
    let manifest = Manifest::load(&root)?;
    let target_dir = root.join("target");
    do_build_at(
        root,
        manifest,
        target_dir,
        None,
        args.release,
        args.force_resolve,
        Vec::new(),
        None,
    )
}

/// Build a specific manifest at `root`, with optional extra classpath entries
/// (used by workspace builds to inject path-dep `target/classes` + JARs).
///
/// `target_dir` is the shared output root. If `member` is `Some`, outputs are
/// written under `<target_dir>/classes/<member>/` etc. (workspace mode);
/// otherwise they go to `<target_dir>/classes/` (single-project mode, which
/// preserves the existing on-disk layout).
/// `workspace_root` (when `Some`) is the directory holding the shared
/// `jet.lock`; per-member regeneration is skipped because the caller has
/// already invoked `Workspace::ensure_lockfile`.
pub fn do_build_at(
    root: PathBuf,
    manifest: Manifest,
    target_dir: PathBuf,
    member: Option<String>,
    release: bool,
    force_resolve: bool,
    extra_classpath: Vec<PathBuf>,
    workspace_root: Option<PathBuf>,
) -> Result<BuildOutputs> {
    let started = Instant::now();
    let prefix = match &member {
        Some(name) => format!("[{name}] "),
        None => String::new(),
    };
    println!(
        "{prefix}Building `{}` v{} (Java {})",
        manifest.pkg()?.name,
        manifest.pkg()?.version,
        manifest.pkg()?.java
    );
    let args = BuildArgs { release, force_resolve, package: None, jobs: None };

    // Workspace-aware path layout. Single-project keeps the legacy paths.
    let classes_dir = match (&member, release) {
        (Some(m), true) => target_dir.join("release/classes").join(m),
        (Some(m), false) => target_dir.join("classes").join(m),
        (None, true) => target_dir.join("release/classes"),
        (None, false) => target_dir.join("classes"),
    };
    let info_dir = match &member {
        Some(m) => target_dir.join("jet-info").join(m),
        None => target_dir.join("jet-info"),
    };

    // 1. Resolve / load lockfile, fetch JARs.
    let dep_jars = if manifest.dependencies.is_empty() {
        Vec::new()
    } else {
        let fetcher = Fetcher::new(default_repos())?;
        // In workspace mode, the shared lockfile lives at workspace_root and
        // was pre-generated by Workspace::ensure_lockfile; load it without
        // any per-member regeneration. Single-project mode keeps the legacy
        // member-level regenerate-if-stale flow.
        let lockfile = if let Some(ws_root) = &workspace_root {
            Lockfile::load(ws_root).with_context(|| {
                format!(
                    "loading workspace lockfile at {}/jet.lock",
                    ws_root.display()
                )
            })?
        } else {
            let lock_path = root.join(LOCKFILE_NAME);
            if !args.force_resolve && lock_path.is_file() {
                match Lockfile::load(&root) {
                    Ok(l) => l,
                    Err(e) => {
                        eprintln!("  warning: jet.lock unreadable ({e:#}); regenerating");
                        regenerate_lockfile(&root, &manifest, &fetcher)?
                    }
                }
            } else {
                regenerate_lockfile(&root, &manifest, &fetcher)?
            }
        };

        // Fetch every package in the lock that's compile/runtime.
        let mut jars = Vec::with_capacity(lockfile.packages.len());
        for pkg in &lockfile.packages {
            if pkg.scope != "compile" && pkg.scope != "runtime" {
                continue;
            }
            let coord = lockfile_coord(pkg);
            let path = fetcher
                .fetch_artifact(&coord, pkg.sha256.as_deref())
                .with_context(|| format!("fetching {coord}"))?;
            jars.push(path);
        }
        jars
    };

    // 1b. Path-dep classpath additions (workspace builds inject these).
    let mut dep_jars = dep_jars;
    for p in extra_classpath {
        if !dep_jars.contains(&p) {
            dep_jars.push(p);
        }
    }

    // 2. Discover sources.
    let src_dir = root.join("src/main/java");
    let sources = collect_java_sources(&src_dir)?;
    if sources.is_empty() {
        bail!(
            "no Java sources found under `{}`",
            src_dir.display()
        );
    }

    // 3. classes_dir computed above (workspace-aware).

    // 4. Incremental check.
    let cache_dir = info_dir.clone();
    fs::create_dir_all(&cache_dir).ok();
    let cache_path = cache_dir.join(if args.release { "build-release.json" } else { "build.json" });

    let fingerprint = compute_fingerprint(
        &manifest,
        &sources,
        &dep_jars,
        args.release,
    )?;
    let prior: Option<BuildCache> = fs::read_to_string(&cache_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());

    if classes_dir.is_dir()
        && prior.as_ref().map(|p| &p.fingerprint) == Some(&fingerprint.fingerprint)
    {
        let elapsed = started.elapsed();
        println!(
            "  Up-to-date ({:.0}ms)",
            elapsed.as_secs_f64() * 1000.0
        );
        return Ok(BuildOutputs {
            project_root: root,
            target_dir: target_dir.clone(),
            manifest,
            classes_dir: classes_dir.clone(),
            classpath: build_classpath(&classes_dir, &dep_jars),
        });
    }

    // 5. Invoke javac.
    let javac = find_javac()?;
    let mut classpath_for_javac = dep_jars.clone();
    // For incremental, javac also needs prior .class outputs on the classpath.
    if classes_dir.is_dir() {
        classpath_for_javac.push(classes_dir.clone());
    }
    let encoding = manifest
        .build
        .encoding
        .clone()
        .unwrap_or_else(|| "UTF-8".into());
    let extra_args = manifest.build.javac_args.clone().unwrap_or_default();

    compile(CompileSpec {
        javac: &javac,
        release: manifest.pkg()?.java,
        classpath: &classpath_for_javac,
        output_dir: &classes_dir,
        sources: &sources,
        encoding: &encoding,
        extra_args: &extra_args,
    })?;

    // 6. Write incremental cache.
    fs::write(&cache_path, serde_json::to_string_pretty(&fingerprint)?)
        .with_context(|| format!("writing {}", cache_path.display()))?;

    let elapsed = started.elapsed();
    println!(
        "{prefix}Compiled {} source files in {:.0}ms",
        sources.len(),
        elapsed.as_secs_f64() * 1000.0
    );

    Ok(BuildOutputs {
        project_root: root,
        target_dir: target_dir.clone(),
        manifest,
        classes_dir: classes_dir.clone(),
        classpath: build_classpath(&classes_dir, &dep_jars),
    })
}

fn build_classpath(classes_dir: &Path, dep_jars: &[PathBuf]) -> Vec<PathBuf> {
    let mut cp = Vec::with_capacity(dep_jars.len() + 1);
    cp.push(classes_dir.to_path_buf());
    cp.extend(dep_jars.iter().cloned());
    cp
}

fn lockfile_coord(p: &crate::lockfile::Package) -> crate::coord::Coord {
    let (g, a) = p.name.split_once(':').unwrap_or((&p.name, ""));
    crate::coord::Coord {
        group: g.into(),
        artifact: a.into(),
        version: p.version.clone(),
        classifier: p.classifier.clone(),
        ty: p.ty.clone(),
    }
}

fn regenerate_lockfile(
    root: &Path,
    manifest: &Manifest,
    fetcher: &Fetcher,
) -> Result<Lockfile> {
    let resolution: Resolution = resolve_deps(manifest, fetcher)?;
    let lf = Lockfile::from_resolution(
        &manifest.pkg()?.name,
        &manifest.pkg()?.version,
        &resolution,
        &default_repos()[0],
    );
    lf.save(root)?;
    println!(
        "  Resolved {}",
        crate::resolver::resolve::summary(&resolution)
    );
    Ok(lf)
}

fn collect_java_sources(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let p = entry.path();
        if p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("java") {
            out.push(p.to_path_buf());
        }
    }
    out.sort();
    Ok(out)
}

#[derive(Debug, Serialize, Deserialize)]
struct BuildCache {
    fingerprint: String,
    /// Per-source mtime+sha for diagnostics. Not used in incremental decision
    /// (we hash the full set into `fingerprint`).
    sources: BTreeMap<String, String>,
    java: u32,
    release: bool,
}

fn compute_fingerprint(
    manifest: &Manifest,
    sources: &[PathBuf],
    dep_jars: &[PathBuf],
    release: bool,
) -> Result<BuildCache> {
    let mut hasher = Sha256::new();
    hasher.update(manifest.pkg()?.name.as_bytes());
    hasher.update(b"\0");
    hasher.update(manifest.pkg()?.version.as_bytes());
    hasher.update(b"\0");
    hasher.update(manifest.pkg()?.java.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update([release as u8]);
    let mut per_source: BTreeMap<String, String> = BTreeMap::new();
    for s in sources {
        let bytes = fs::read(s).with_context(|| format!("reading {}", s.display()))?;
        let h = sha256(&bytes);
        hasher.update(s.display().to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(h.as_bytes());
        hasher.update(b"\0");
        per_source.insert(s.display().to_string(), h);
    }
    for j in dep_jars {
        hasher.update(j.display().to_string().as_bytes());
        hasher.update(b"\0");
    }
    let fp = hex::encode(hasher.finalize());
    Ok(BuildCache {
        fingerprint: fp,
        sources: per_source,
        java: manifest.pkg()?.java,
        release,
    })
}

fn sha256(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    hex::encode(h.finalize())
}
