//! `jet test [filter]` — compile tests, fetch JUnit deps, run JUnit Platform Console Launcher.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use walkdir::WalkDir;

use crate::coord::Coord;
use crate::javac::{CompileSpec, compile, find_java, find_javac, join_classpath};
use crate::lockfile::{LOCKFILE_NAME, Lockfile};
use crate::manifest::Manifest;
use crate::resolver::{Fetcher, default_repos, resolve::resolve_with_dev};

use super::build::{BuildArgs, do_build};

/// Pinned default version. Users can declare junit-jupiter explicitly to vary.
const CONSOLE_LAUNCHER_VERSION: &str = "1.10.2";

pub struct TestArgs {
    pub filter: Option<String>,
}

pub fn cmd_test(args: TestArgs) -> Result<()> {
    let started = Instant::now();

    // 1. Build main first (fail-fast if main doesn't compile).
    let main = do_build(BuildArgs { release: false, force_resolve: false, package: None, jobs: None, no_cache: false })?;
    let root = main.project_root.clone();

    // 2. Resolve main + dev deps unified, regenerate jet.lock if needed.
    let fetcher = Fetcher::new(default_repos())?;
    let lockfile = ensure_lockfile_with_dev(&root, &main.manifest, &fetcher)?;

    // 3. Fetch all main + dev jars.
    let main_jars = fetch_origin(&fetcher, &lockfile, "main")?;
    let dev_jars = fetch_origin(&fetcher, &lockfile, "dev")?;
    if dev_jars.is_empty() && !contains_junit_jupiter(&main.manifest) {
        bail!(
            "no test framework found.

To run tests, add JUnit 5 to [dev-dependencies]:

    jet add --dev org.junit.jupiter:junit-jupiter:5.10.2

(or edit jet.toml manually):

    [dev-dependencies]
    \"org.junit.jupiter:junit-jupiter\" = \"5.10.2\""
        );
    }

    // 4. Compile src/test/java to target/test-classes.
    let test_src_dir = root.join("src/test/java");
    let test_sources = collect_java_sources(&test_src_dir)?;
    if test_sources.is_empty() {
        println!("  No tests found in {}", test_src_dir.display());
        return Ok(());
    }
    let test_classes_dir = main.target_dir.join("test-classes");
    let test_compile_cp: Vec<PathBuf> = std::iter::once(main.classes_dir.clone())
        .chain(main_jars.iter().cloned())
        .chain(dev_jars.iter().cloned())
        .collect();

    let javac = find_javac()?;
    let encoding = main
        .manifest
        .build
        .encoding
        .clone()
        .unwrap_or_else(|| "UTF-8".into());
    let extra_args: Vec<String> = main
        .manifest
        .build
        .javac_args
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a != "-Werror") // tests legitimately use deprecated APIs etc.
        .chain(std::iter::once("-parameters".to_string())) // useful for JUnit reflection
        .collect();

    println!("  Compiling tests ({} files)", test_sources.len());
    compile(CompileSpec {
        javac: &javac,
        release: main.manifest.pkg()?.java,
        classpath: &test_compile_cp,
        output_dir: &test_classes_dir,
        sources: &test_sources,
        encoding: &encoding,
        extra_args: &extra_args,
    })?;

    // 5. Ensure JUnit Platform Console Launcher is in cache.
    let console_jar = ensure_console_launcher(&fetcher)?;

    // 6. Build full test runtime classpath.
    let mut test_runtime_cp: Vec<PathBuf> =
        Vec::with_capacity(2 + main_jars.len() + dev_jars.len() + 1);
    test_runtime_cp.push(test_classes_dir.clone());
    test_runtime_cp.push(main.classes_dir.clone());
    test_runtime_cp.extend(main_jars);
    test_runtime_cp.extend(dev_jars);
    test_runtime_cp.push(console_jar.clone());

    // 7. Invoke launcher.
    let java = find_java()?;
    let reports_dir = main.target_dir.join("test-reports");
    fs::create_dir_all(&reports_dir).ok();

    let mut cmd = Command::new(&java);
    cmd.arg("-cp")
        .arg(join_classpath(&test_runtime_cp))
        .arg("org.junit.platform.console.ConsoleLauncher")
        .arg("execute")
        .arg("--reports-dir")
        .arg(&reports_dir)
        .arg("--details=tree")
        .arg("--disable-banner");

    apply_filter(&mut cmd, args.filter.as_deref(), &test_classes_dir);

    println!("  Running tests");
    let status = cmd.status().context("spawning java for JUnit launcher")?;
    let elapsed = started.elapsed();
    let exit_code = status.code().unwrap_or(1);
    println!(
        "  test run finished in {:.2}s (reports: {})",
        elapsed.as_secs_f64(),
        reports_dir.display()
    );
    if !status.success() {
        bail!("tests failed (junit launcher exit {exit_code})");
    }
    Ok(())
}

fn contains_junit_jupiter(m: &Manifest) -> bool {
    m.dev_dependencies
        .keys()
        .any(|k| k.starts_with("org.junit.jupiter:") || k == "org.junit:junit-bom")
}

/// Apply filter selector: `Foo::bar`, `Foo`, `com.acme.*`, lowercase substring,
/// or none → scan-class-path.
fn apply_filter(cmd: &mut Command, filter: Option<&str>, test_classes: &Path) {
    match filter {
        None => {
            cmd.arg("--scan-class-path").arg(test_classes);
        }
        Some(f) => {
            // Method form: `Foo::bar` or `Foo#bar`.
            if let Some((class, method)) = split_method(f) {
                cmd.arg("--select-method").arg(format!("{class}#{method}"));
                return;
            }
            // Wildcard package form: `com.acme.*`.
            if let Some(pkg) = f.strip_suffix(".*") {
                cmd.arg("--select-package").arg(pkg);
                return;
            }
            // Capitalized class name (FQN or simple) → select-class or include-classname.
            let starts_upper = f
                .split('.')
                .next_back()
                .and_then(|s| s.chars().next())
                .is_some_and(|c| c.is_ascii_uppercase());
            if starts_upper {
                if f.contains('.') {
                    cmd.arg("--select-class").arg(f);
                } else {
                    cmd.arg("--scan-class-path").arg(test_classes);
                    cmd.arg("--include-classname")
                        .arg(format!("^(?:.*\\.)?{f}$"));
                }
                return;
            }
            // Otherwise: lowercase substring filter.
            cmd.arg("--scan-class-path").arg(test_classes);
            cmd.arg("--include-classname")
                .arg(format!(".*{}.*", regex_escape(f)));
        }
    }
}

fn split_method(s: &str) -> Option<(&str, &str)> {
    if let Some(idx) = s.find("::") {
        return Some((&s[..idx], &s[idx + 2..]));
    }
    if let Some(idx) = s.find('#') {
        return Some((&s[..idx], &s[idx + 1..]));
    }
    None
}

fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if matches!(
            c,
            '.' | '\\' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn ensure_lockfile_with_dev(
    root: &Path,
    manifest: &Manifest,
    fetcher: &Fetcher,
) -> Result<Lockfile> {
    let lock_path = root.join(LOCKFILE_NAME);
    let needs_dev = !manifest.dev_dependencies.is_empty();
    if !needs_dev && manifest.dependencies.is_empty() {
        // Empty lockfile.
        let lf = Lockfile::from_resolution(
            &manifest.pkg()?.name,
            &manifest.pkg()?.version,
            &Default::default(),
            &default_repos()[0],
        );
        lf.save(root)?;
        return Ok(lf);
    }

    let need_resolve = if !lock_path.is_file() {
        true
    } else {
        match Lockfile::load(root) {
            Ok(lf) => {
                // Re-resolve if any dev-dep is missing from the lockfile.
                let dev_keys: Vec<&str> =
                    manifest.dev_dependencies.keys().map(String::as_str).collect();
                dev_keys.iter().any(|k| {
                    !lf.packages
                        .iter()
                        .any(|p| p.name == *k && p.origin == "dev" || p.origin == "main" && p.name == *k)
                })
            }
            Err(_) => true,
        }
    };

    if need_resolve {
        println!("  Resolving dependencies (with dev-dependencies)…");
        let resolution = resolve_with_dev(manifest, fetcher)?;
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
    } else {
        Lockfile::load(root)
    }
}

fn fetch_origin(fetcher: &Fetcher, lockfile: &Lockfile, origin: &str) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for pkg in &lockfile.packages {
        if pkg.origin != origin {
            continue;
        }
        if pkg.scope != "compile" && pkg.scope != "runtime" {
            continue;
        }
        let coord = pkg_to_coord(pkg);
        let path = fetcher
            .fetch_artifact(&coord, pkg.sha256.as_deref())
            .with_context(|| format!("fetching {coord}"))?;
        out.push(path);
    }
    Ok(out)
}

fn pkg_to_coord(p: &crate::lockfile::Package) -> Coord {
    let (g, a) = p.name.split_once(':').unwrap_or((&p.name, ""));
    Coord {
        group: g.into(),
        artifact: a.into(),
        version: p.version.clone(),
        classifier: p.classifier.clone(),
        ty: p.ty.clone(),
    }
}

fn ensure_console_launcher(fetcher: &Fetcher) -> Result<PathBuf> {
    let coord = Coord {
        group: "org.junit.platform".into(),
        artifact: "junit-platform-console-standalone".into(),
        version: CONSOLE_LAUNCHER_VERSION.into(),
        classifier: None,
        ty: "jar".into(),
    };
    fetcher
        .fetch_artifact(&coord, None)
        .with_context(|| format!("fetching {coord}"))
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
