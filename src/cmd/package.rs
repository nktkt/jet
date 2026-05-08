//! `jet package [--uber]` — build a (reproducible) JAR.
//!
//! Thin mode: only the project's compiled classes + `src/main/resources` end up
//! in the output. The user is expected to ship `lib/` separately or rely on
//! `Class-Path` (not yet emitted — TODO 0.5).
//!
//! Uber mode: all main `[dependencies]` JAR contents are merged into the
//! output, with `META-INF/services/*` line-merged, signatures stripped, and
//! per-jar `LICENSE` / `NOTICE` files renamed.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;
use zip::ZipArchive;

use super::build::{BuildArgs, do_build};
use crate::classes::detect_main_classes;
use crate::jar::{Entry, JarBuilder, render_manifest};
use crate::lockfile::{LOCKFILE_NAME, Lockfile};
use crate::manifest::Manifest;
use crate::resolver::{Fetcher, default_repos};

pub struct PackageArgs {
    pub uber: bool,
}

pub fn cmd_package(args: PackageArgs) -> Result<()> {
    let started = Instant::now();
    let outputs = do_build(BuildArgs { release: false, force_resolve: false })?;
    let root = outputs.project_root.clone();
    let manifest = outputs.manifest;

    let main_class = resolve_main_class(&manifest, &outputs.classes_dir)?;

    let mut builder = JarBuilder::new();
    let mut warnings: Vec<String> = Vec::new();

    // 1. META-INF/ + MANIFEST.MF (manifest content built once `--uber` adds
    // entries; manifest itself doesn't depend on dep contents).
    builder.put(Entry::dir("META-INF"));
    let manifest_body = build_manifest(&manifest, main_class.as_deref(), args.uber);
    builder.put(Entry::file("META-INF/MANIFEST.MF", manifest_body));

    // 2. Project's compiled classes.
    add_directory_tree(&mut builder, &outputs.classes_dir, "")?;

    // 3. src/main/resources (added at package time, not copied to target/classes).
    let resources_dir = root.join("src/main/resources");
    if resources_dir.is_dir() {
        add_resources(&mut builder, &resources_dir, &mut warnings)?;
    }

    // 4. Uber mode: walk dep JARs.
    if args.uber {
        let fetcher = Fetcher::new(default_repos())?;
        let lockfile = match Lockfile::load(&root) {
            Ok(l) => l,
            Err(e) if !root.join(LOCKFILE_NAME).exists() && manifest.dependencies.is_empty() => {
                eprintln!("  note: no jet.lock and no dependencies; skipping ({e:#})");
                return write_jar(&builder, &output_path(&root, &manifest, args.uber)?, started);
            }
            Err(e) => return Err(e),
        };

        let dep_jars = fetch_main_jars(&fetcher, &lockfile)?;
        let mut services: HashMap<String, Vec<String>> = HashMap::new();
        let mut classes: HashMap<String, [u8; 32]> = HashMap::new();

        for jar_path in &dep_jars {
            merge_dep_jar(
                jar_path,
                &mut builder,
                &mut services,
                &mut classes,
                &mut warnings,
            )
            .with_context(|| format!("merging {}", jar_path.display()))?;
        }

        // Materialize merged services entries.
        for (path, lines) in services {
            let body = lines.join("\n").into_bytes();
            // Project entries already in builder may shadow these — we only
            // wrote services after dep merge, but project services should
            // already have been added in step 2/3 if present. Last-write-wins
            // here is fine because services entries from main classes are rare.
            builder.put(Entry::file(path, body));
        }
    }

    // 5. Write JAR.
    let out = output_path(&root, &manifest, args.uber)?;
    write_jar(&builder, &out, started)?;

    // 6. Print warnings (after success).
    for w in &warnings {
        eprintln!("  warning: {w}");
    }
    Ok(())
}

fn output_path(root: &Path, manifest: &Manifest, uber: bool) -> Result<PathBuf> {
    let target = root.join("target");
    fs::create_dir_all(&target)
        .with_context(|| format!("creating {}", target.display()))?;
    let stem = if uber {
        format!("{}-{}-uber.jar", manifest.package.name, manifest.package.version)
    } else {
        format!("{}-{}.jar", manifest.package.name, manifest.package.version)
    };
    Ok(target.join(stem))
}

fn write_jar(builder: &JarBuilder, path: &Path, started: Instant) -> Result<()> {
    builder.write_to(path)?;
    let bytes = fs::metadata(path)?.len();
    println!(
        "  Packaged {} ({} entries, {} bytes) in {:.0}ms",
        path.display(),
        builder.len(),
        bytes,
        started.elapsed().as_secs_f64() * 1000.0
    );
    Ok(())
}

fn resolve_main_class(manifest: &Manifest, classes_dir: &Path) -> Result<Option<String>> {
    if let Some(m) = &manifest.package.main {
        return Ok(Some(m.clone()));
    }
    let candidates = detect_main_classes(classes_dir)?;
    match candidates.len() {
        0 => Ok(None), // library JAR: no Main-Class header
        1 => Ok(Some(candidates.into_iter().next().unwrap())),
        _ => {
            eprintln!(
                "  warning: multiple main classes found: [{}]. Producing a library JAR. \
                 Set [package].main in jet.toml to make it executable.",
                candidates.join(", ")
            );
            Ok(None)
        }
    }
}

fn build_manifest(manifest: &Manifest, main_class: Option<&str>, uber: bool) -> Vec<u8> {
    let mut headers: Vec<(&str, String)> = Vec::with_capacity(8);
    headers.push(("Manifest-Version", "1.0".into()));
    headers.push(("Created-By", format!("jet {}", env!("CARGO_PKG_VERSION"))));
    headers.push(("Build-Jdk-Spec", manifest.package.java.to_string()));
    headers.push(("Implementation-Title", manifest.package.name.clone()));
    headers.push(("Implementation-Version", manifest.package.version.clone()));
    if let Some(vendor) = manifest.package.authors.first() {
        headers.push(("Implementation-Vendor", vendor.clone()));
    }
    if let Some(m) = main_class {
        headers.push(("Main-Class", m.to_string()));
    }
    if uber {
        headers.push(("Multi-Release", "false".into()));
    }
    render_manifest(&headers)
}

fn add_directory_tree(
    builder: &mut JarBuilder,
    dir: &Path,
    prefix: &str,
) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let rel = p
            .strip_prefix(dir)
            .with_context(|| format!("strip_prefix {}", p.display()))?;
        let mut entry_path = String::new();
        if !prefix.is_empty() {
            entry_path.push_str(prefix);
            if !prefix.ends_with('/') {
                entry_path.push('/');
            }
        }
        entry_path.push_str(&rel.to_string_lossy().replace('\\', "/"));
        let bytes = fs::read(p).with_context(|| format!("reading {}", p.display()))?;
        builder.put(Entry::file(entry_path, bytes));
    }
    Ok(())
}

fn add_resources(
    builder: &mut JarBuilder,
    resources_dir: &Path,
    warnings: &mut Vec<String>,
) -> Result<()> {
    for entry in WalkDir::new(resources_dir).into_iter().filter_map(|e| e.ok()) {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        if is_default_excluded(p) {
            continue;
        }
        let rel = p
            .strip_prefix(resources_dir)
            .with_context(|| format!("strip_prefix {}", p.display()))?;
        let entry_path = rel.to_string_lossy().replace('\\', "/");
        let bytes = fs::read(p).with_context(|| format!("reading {}", p.display()))?;
        if !builder.put_if_absent(Entry::file(entry_path.clone(), bytes)) {
            warnings.push(format!(
                "resource `{entry_path}` shadowed by an existing JAR entry (compiled output wins)"
            ));
        }
    }
    Ok(())
}

fn is_default_excluded(p: &Path) -> bool {
    let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
    matches!(name, ".DS_Store" | "Thumbs.db" | "desktop.ini")
        || name.ends_with('~')
        || name.ends_with(".swp")
        || name.ends_with(".swo")
}

fn fetch_main_jars(fetcher: &Fetcher, lockfile: &Lockfile) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for pkg in &lockfile.packages {
        if pkg.origin != "main" {
            continue;
        }
        if pkg.scope != "compile" && pkg.scope != "runtime" {
            continue;
        }
        let coord = crate::coord::Coord {
            group: pkg.name.split_once(':').map(|(g, _)| g).unwrap_or("").into(),
            artifact: pkg.name.split_once(':').map(|(_, a)| a).unwrap_or("").into(),
            version: pkg.version.clone(),
            classifier: pkg.classifier.clone(),
            ty: pkg.ty.clone(),
        };
        let path = fetcher
            .fetch_artifact(&coord, pkg.sha256.as_deref())
            .with_context(|| format!("fetching {coord}"))?;
        out.push(path);
    }
    Ok(out)
}

/// Merge entries from a single dependency JAR into the builder.
fn merge_dep_jar(
    jar_path: &Path,
    builder: &mut JarBuilder,
    services: &mut HashMap<String, Vec<String>>,
    classes: &mut HashMap<String, [u8; 32]>,
    warnings: &mut Vec<String>,
) -> Result<()> {
    let jar_name = jar_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("dep")
        .to_string();
    let file = fs::File::open(jar_path)
        .with_context(|| format!("opening {}", jar_path.display()))?;
    let mut zip = ZipArchive::new(file)
        .with_context(|| format!("reading zip {}", jar_path.display()))?;

    let mut seen_in_this_jar: HashSet<String> = HashSet::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        if seen_in_this_jar.contains(&name) {
            continue;
        }
        seen_in_this_jar.insert(name.clone());

        // Skip rules.
        if name == "META-INF/MANIFEST.MF" {
            continue;
        }
        if name == "module-info.class" || name.starts_with("META-INF/versions/") {
            // Skip module-info; skip MR-jar version-specific classes (TODO 0.5).
            continue;
        }
        if let Some(rest) = name.strip_prefix("META-INF/") {
            // Strip signatures (cannot survive shading).
            let upper = rest.to_ascii_uppercase();
            if upper.ends_with(".SF")
                || upper.ends_with(".RSA")
                || upper.ends_with(".DSA")
                || upper.ends_with(".EC")
                || upper.starts_with("SIG-")
            {
                continue;
            }
        }

        // Read body.
        let mut body = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut body)?;

        // Service file: line-merge.
        if name.starts_with("META-INF/services/") && !name.ends_with('/') {
            let lines = services.entry(name.clone()).or_default();
            for line in String::from_utf8_lossy(&body).lines() {
                let trimmed = line.split('#').next().unwrap_or("").trim();
                if trimmed.is_empty() {
                    continue;
                }
                if !lines.iter().any(|l| l == trimmed) {
                    lines.push(trimmed.to_string());
                }
            }
            continue;
        }

        // LICENSE / NOTICE: rename to per-jar to preserve attribution.
        if let Some(rest) = name.strip_prefix("META-INF/") {
            let upper = rest.to_ascii_uppercase();
            if upper == "LICENSE" || upper.starts_with("LICENSE.") || upper.starts_with("LICENSE-")
                || upper == "NOTICE" || upper.starts_with("NOTICE.") || upper.starts_with("NOTICE-")
            {
                let renamed = format!("META-INF/{}-{}", rest, jar_name);
                builder.put_if_absent(Entry::file(renamed, body));
                continue;
            }
        }

        // Class file: dedupe by content hash; warn on conflict-with-different-bytes.
        if name.ends_with(".class") {
            let hash = sha256(&body);
            if let Some(prior) = classes.get(&name) {
                if prior == &hash {
                    continue; // identical, silent dedupe
                }
                warnings.push(format!(
                    "class `{name}` differs between {} and earlier dep (kept earlier)",
                    jar_name
                ));
                continue;
            }
            classes.insert(name.clone(), hash);
            // Project's classes were added before deps; project wins.
            builder.put_if_absent(Entry::file(name, body));
            continue;
        }

        // Other resources: first-wins.
        if !builder.put_if_absent(Entry::file(name.clone(), body)) {
            // Identical bytes are common (META-INF/MANIFEST.MF was already
            // skipped); only warn if the path looks meaningful.
            if !name.ends_with('/') && !name.starts_with("META-INF/maven/") {
                warnings.push(format!(
                    "resource `{name}` from {jar_name} shadowed by an earlier source"
                ));
            }
        }
    }
    Ok(())
}

fn sha256(b: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b);
    h.finalize().into()
}
