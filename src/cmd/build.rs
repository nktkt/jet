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
}

/// Build outputs (paths the caller can use). Returned to `run` so it can
/// reuse the classpath without redoing the work.
pub struct BuildOutputs {
    pub project_root: PathBuf,
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
    let mut built: HashMap<usize, BuildOutputs> = HashMap::new();
    for idx in order {
        let extra = collect_path_dep_classpath(&workspace, idx, &built);
        let member = &workspace.members[idx];
        let outputs = do_build_at(
            member.path.clone(),
            member.manifest.clone(),
            args.release,
            args.force_resolve,
            extra,
        )?;
        built.insert(idx, outputs);
    }
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
    do_build_at(root, manifest, args.release, args.force_resolve, Vec::new())
}

/// Build a specific manifest at `root`, with optional extra classpath entries
/// (used by workspace builds to inject path-dep `target/classes` + JARs).
pub fn do_build_at(
    root: PathBuf,
    manifest: Manifest,
    release: bool,
    force_resolve: bool,
    extra_classpath: Vec<PathBuf>,
) -> Result<BuildOutputs> {
    let started = Instant::now();
    println!(
        "  Building `{}` v{} (Java {})",
        manifest.pkg()?.name,
        manifest.pkg()?.version,
        manifest.pkg()?.java
    );
    let args = BuildArgs { release, force_resolve, package: None };

    // 1. Resolve / load lockfile, fetch JARs.
    let dep_jars = if manifest.dependencies.is_empty() {
        Vec::new()
    } else {
        let fetcher = Fetcher::new(default_repos())?;
        let lock_path = root.join(LOCKFILE_NAME);
        let lockfile = if !args.force_resolve && lock_path.is_file() {
            match Lockfile::load(&root) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("  warning: jet.lock unreadable ({e:#}); regenerating");
                    regenerate_lockfile(&root, &manifest, &fetcher)?
                }
            }
        } else {
            regenerate_lockfile(&root, &manifest, &fetcher)?
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

    // 3. Determine output classes dir.
    let classes_dir = if args.release {
        root.join("target/release/classes")
    } else {
        root.join("target/classes")
    };

    // 4. Incremental check.
    let cache_dir = root.join("target/jet-info");
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
        "  Compiled {} source files in {:.0}ms",
        sources.len(),
        elapsed.as_secs_f64() * 1000.0
    );

    Ok(BuildOutputs {
        project_root: root,
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
