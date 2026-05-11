//! `jet doctor` — diagnose project + environment issues.
//!
//! Runs a battery of cheap checks (no network, no compilation) and prints
//! grouped ✓ / ⚠ / ✗ findings. Exits non-zero only on actual errors;
//! warnings inform but don't fail.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use walkdir::WalkDir;

use crate::javac;
use crate::lockfile::{LOCKFILE_NAME, Lockfile};
use crate::manifest::{KNOWN_EDITIONS, MANIFEST_FILENAME, Manifest};
use crate::workspace::Workspace;

pub fn cmd_doctor() -> Result<()> {
    let mut report = Report::new();

    // Anchor everything at a discoverable project root, but tolerate
    // running from outside one (e.g. just to see the toolchain check).
    let cwd = std::env::current_dir()?;
    let project_root = Manifest::find_root(&cwd).ok();

    check_manifest(&mut report, project_root.as_deref());
    check_workspace_and_lockfile(&mut report, project_root.as_deref());
    check_toolchain(&mut report, project_root.as_deref());
    check_caches(&mut report);
    check_target_orphans(&mut report, project_root.as_deref());
    check_plugins(&mut report);

    report.print();
    if report.error_count() > 0 {
        anyhow::bail!("doctor found {} issue(s)", report.error_count());
    }
    Ok(())
}

// ───── Checks ──────────────────────────────────────────────────────────────

fn check_manifest(report: &mut Report, project_root: Option<&Path>) {
    let mut g = report.group("Manifest");
    let Some(root) = project_root else {
        g.warn(
            "no `jet.toml` found in this directory or any parent — \
             environment checks only",
        );
        return;
    };
    g.pass(format!("jet.toml at {}", root.join(MANIFEST_FILENAME).display()));

    let manifest = match Manifest::load(root) {
        Ok(m) => m,
        Err(e) => {
            g.error(format!("failed to parse jet.toml: {e:#}"));
            return;
        }
    };

    if let Ok(pkg) = manifest.pkg() {
        match pkg.edition.as_deref() {
            Some(ed) if KNOWN_EDITIONS.contains(&ed) => {
                g.pass(format!("edition = \"{ed}\""));
            }
            Some(ed) => g.error(format!(
                "[package].edition = \"{ed}\" is not known to jet {} \
                 (known: {})",
                env!("CARGO_PKG_VERSION"),
                KNOWN_EDITIONS.join(", "),
            )),
            None => g.warn(
                "no `[package].edition` set — defaults to \"2026\". \
                 Pin it so future jet releases stay backward-compatible.",
            ),
        }
        if pkg.group.is_none() {
            g.warn("no `[package].group` — `jet publish` will refuse to run.");
        }
        if pkg.license.is_none() {
            g.warn("no `[package].license` — recommended for any published artifact.");
        }
    } else if manifest.workspace.is_some() {
        g.pass("virtual workspace manifest (no [package] of its own)");
    }

    // Workspace inheritance markers that survived parsing → caller misspelled a key.
    if manifest.workspace.is_none() {
        for (key, spec) in &manifest.dependencies {
            if spec.inherits_workspace() {
                g.error(format!(
                    "`{key}.workspace = true` outside a workspace — there's nothing to inherit from."
                ));
            }
        }
    }
}

fn check_workspace_and_lockfile(report: &mut Report, project_root: Option<&Path>) {
    let mut g = report.group("Workspace & lockfile");
    let Some(root) = project_root else {
        return;
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| root.to_path_buf());

    // Workspace discovery surfaces glob errors, member load failures, and
    // path-dep cycles all at once.
    let workspace = match Workspace::discover(&cwd) {
        Ok(ws) => ws,
        Err(e) => {
            g.error(format!("workspace discovery failed: {e:#}"));
            return;
        }
    };
    if workspace.is_explicit_workspace() {
        g.pass(format!("{} workspace members loaded", workspace.members.len()));
        match workspace.topological_order() {
            Ok(_) => g.pass("no path-dep cycles"),
            Err(e) => g.error(format!("path-dep graph: {e:#}")),
        }
        let legacy = workspace.find_legacy_member_locks();
        if !legacy.is_empty() {
            g.warn(format!(
                "{} stale per-member `jet.lock` file(s) under workspace members — \
                 the authoritative lock is at the workspace root.",
                legacy.len()
            ));
        }
    }

    let lock_path = root.join(LOCKFILE_NAME);
    let manifest = match Manifest::load(root) {
        Ok(m) => m,
        Err(_) => return, // manifest check already reported
    };
    let has_maven_deps = !manifest.dependencies.is_empty()
        || !manifest.dev_dependencies.is_empty()
        || workspace.members.iter().any(|m| {
            !m.manifest.dependencies.is_empty() || !m.manifest.dev_dependencies.is_empty()
        });
    if !has_maven_deps {
        g.pass("no [dependencies]; lockfile not needed");
        return;
    }
    if !lock_path.is_file() {
        g.warn(format!(
            "jet.lock missing at {} — run `jet build` to generate it",
            lock_path.display()
        ));
        return;
    }
    let lockfile = match Lockfile::load(root) {
        Ok(lf) => lf,
        Err(e) => {
            g.error(format!("failed to parse jet.lock: {e:#}"));
            return;
        }
    };
    g.pass(format!(
        "jet.lock present ({} package{})",
        lockfile.packages.len(),
        if lockfile.packages.len() == 1 { "" } else { "s" }
    ));

    // Manifest deps missing from the lockfile = stale lock.
    let mut all_decls: Vec<&String> = manifest.dependencies.keys().collect();
    all_decls.extend(manifest.dev_dependencies.keys());
    for m in &workspace.members {
        all_decls.extend(m.manifest.dependencies.keys());
        all_decls.extend(m.manifest.dev_dependencies.keys());
    }
    let locked_keys: std::collections::HashSet<&str> =
        lockfile.packages.iter().map(|p| p.name.as_str()).collect();
    let mut missing: Vec<&str> = Vec::new();
    for key in all_decls {
        // Path deps and workspace-inherited markers don't appear in lock.
        if !locked_keys.contains(key.as_str()) && !key.starts_with("local:") {
            missing.push(key);
        }
    }
    missing.sort();
    missing.dedup();
    if !missing.is_empty() && missing.len() <= 8 {
        g.warn(format!(
            "jet.lock is missing {} declared dep(s): [{}] — run `jet build` to refresh",
            missing.len(),
            missing.join(", ")
        ));
    } else if missing.len() > 8 {
        g.warn(format!(
            "jet.lock is missing {} declared deps — run `jet build` to refresh",
            missing.len()
        ));
    }
}

fn check_toolchain(report: &mut Report, project_root: Option<&Path>) {
    let mut g = report.group("Toolchain");
    let manifest = project_root.and_then(|root| Manifest::load(root).ok());

    if let Some(m) = &manifest {
        if let Some(tc) = &m.toolchain {
            let dir_name = format!("{}-{}", tc.vendor, tc.version);
            let installed =
                crate::toolchain::list_installed().ok().unwrap_or_default();
            if installed
                .iter()
                .any(|j| j.vendor == tc.vendor && j.version == tc.version)
            {
                g.pass(format!("[toolchain] {dir_name} installed"));
            } else {
                g.warn(format!(
                    "[toolchain] {dir_name} not installed — \
                     `jet build` will auto-install on next run, \
                     or run `jet jdk install {} --vendor {}`",
                    tc.version, tc.vendor
                ));
            }
            return; // managed toolchain takes precedence; skip system check
        }
    }

    match javac::find_javac() {
        Ok(p) => {
            let version = system_javac_version(&p);
            g.pass(format!(
                "javac on PATH: {} ({})",
                version.as_deref().unwrap_or("version unknown"),
                p.display()
            ));
            if let (Some(m), Some(v)) = (&manifest, version.as_deref()) {
                if let Ok(pkg) = m.pkg() {
                    if let Some(actual) = parse_major(v) {
                        if actual < pkg.java {
                            g.warn(format!(
                                "system javac is {actual}, but [package].java = {} — \
                                 add a `[toolchain] version = {}` to auto-install the right JDK",
                                pkg.java, pkg.java
                            ));
                        }
                    }
                }
            }
        }
        Err(_) => g.error(
            "javac not found on PATH and no [toolchain] declared. \
             Install a JDK (e.g. `jet jdk install 21`) or set JAVA_HOME.",
        ),
    }
}

fn check_caches(report: &mut Report) {
    let mut g = report.group("Caches");
    let base = match dirs::cache_dir() {
        Some(p) => p.join("jet"),
        None => {
            g.warn("could not locate a system cache directory");
            return;
        }
    };
    let maven_cache = std::env::var("JET_CACHE_DIR")
        .map(PathBuf::from)
        .ok()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .map(|h| h.join(".jet/cache"))
                .unwrap_or_default()
        });
    report_dir_size(&mut g, "Maven artifacts", &maven_cache);
    report_dir_size(&mut g, "content build cache", &base.join("build"));

    if let Some(home) = dirs::home_dir() {
        let jdks = home.join(".jet/jdks");
        if jdks.is_dir() {
            let count = fs::read_dir(&jdks)
                .map(|it| it.filter_map(|e| e.ok()).count())
                .unwrap_or(0);
            g.pass(format!(
                "managed JDKs: {} installed at {}",
                count,
                jdks.display()
            ));
        }
    }
}

fn check_target_orphans(report: &mut Report, project_root: Option<&Path>) {
    let mut g = report.group("Build outputs");
    let Some(root) = project_root else { return };
    let target = root.join("target");
    if !target.is_dir() {
        return;
    }
    let classes_dir = target.join("classes");
    if !classes_dir.is_dir() {
        return;
    }
    let workspace = match Workspace::discover(root) {
        Ok(w) => w,
        Err(_) => return,
    };
    if !workspace.is_explicit_workspace() {
        return;
    }
    let known: std::collections::HashSet<&str> =
        workspace.members.iter().map(|m| m.name.as_str()).collect();
    let mut orphans: Vec<String> = Vec::new();
    if let Ok(entries) = fs::read_dir(&classes_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if let Some(name) = entry.file_name().to_str() {
                if !known.contains(name) {
                    orphans.push(name.to_string());
                }
            }
        }
    }
    if orphans.is_empty() {
        g.pass(format!("target/classes/ matches {} member(s)", known.len()));
    } else {
        orphans.sort();
        g.warn(format!(
            "{} orphan directory(ies) under target/classes/: [{}] — \
             leftover from a renamed/removed member; `jet clean` to remove",
            orphans.len(),
            orphans.join(", ")
        ));
    }
}

fn check_plugins(report: &mut Report) {
    let mut g = report.group("Plugins");
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return,
    };
    let mut found: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = Default::default();
    for entry in std::env::split_paths(&path) {
        let Ok(dir) = fs::read_dir(&entry) else { continue };
        for f in dir.filter_map(|f| f.ok()) {
            let name = f.file_name().to_string_lossy().into_owned();
            let stem = name.strip_suffix(".exe").map(str::to_string).unwrap_or(name);
            let Some(plugin) = stem.strip_prefix("jet-") else { continue };
            if plugin.is_empty() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let Ok(meta) = f.metadata() else { continue };
                if meta.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            if seen.insert(plugin.to_string()) {
                found.push(plugin.to_string());
            }
        }
    }
    if found.is_empty() {
        g.pass("no jet-* plugins on PATH (use `jet plugins` to confirm)");
    } else {
        found.sort();
        g.pass(format!("{} plugin(s) on PATH: {}", found.len(), found.join(", ")));
    }
}

// ───── Helpers ─────────────────────────────────────────────────────────────

fn report_dir_size(g: &mut Group<'_>, label: &str, dir: &Path) {
    if !dir.is_dir() {
        return;
    }
    let mut total: u64 = 0;
    let mut count: u64 = 0;
    for e in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if e.path().is_file() {
            if let Ok(meta) = e.metadata() {
                total += meta.len();
                count += 1;
            }
        }
    }
    g.pass(format!(
        "{label}: {} file(s), {} ({})",
        count,
        format_size(total),
        dir.display()
    ));
}

fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes >= GIB {
        format!("{:.1} GB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} kB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn system_javac_version(javac: &Path) -> Option<String> {
    let out = Command::new(javac).arg("-version").output().ok()?;
    // javac writes the version to stdout on JDK 9+, stderr historically.
    for stream in [&out.stdout, &out.stderr] {
        let s = String::from_utf8_lossy(stream);
        if let Some(rest) = s.trim().strip_prefix("javac ") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

fn parse_major(version: &str) -> Option<u32> {
    // Accepts "21", "21.0.2", "1.8.0_392" (legacy → 8).
    let head = version.split(['.', '_', '+']).next()?;
    let n: u32 = head.parse().ok()?;
    if n == 1 {
        // legacy 1.X form: take the X
        version.split('.').nth(1).and_then(|x| x.parse().ok())
    } else {
        Some(n)
    }
}

// ───── Report data model ──────────────────────────────────────────────────

struct Report {
    groups: Vec<GroupData>,
}

struct GroupData {
    title: String,
    findings: Vec<Finding>,
}

enum Finding {
    Pass(String),
    Warn(String),
    Error(String),
}

struct Group<'a> {
    data: &'a mut GroupData,
}

impl Report {
    fn new() -> Self {
        Self { groups: Vec::new() }
    }
    fn group(&mut self, title: &str) -> Group<'_> {
        self.groups.push(GroupData {
            title: title.to_string(),
            findings: Vec::new(),
        });
        Group { data: self.groups.last_mut().unwrap() }
    }
    fn error_count(&self) -> usize {
        self.groups
            .iter()
            .flat_map(|g| &g.findings)
            .filter(|f| matches!(f, Finding::Error(_)))
            .count()
    }
    fn print(&self) {
        let mut pass = 0;
        let mut warn = 0;
        let mut err = 0;
        for g in &self.groups {
            if g.findings.is_empty() {
                continue;
            }
            println!();
            println!("  {}", g.title);
            for f in &g.findings {
                match f {
                    Finding::Pass(s) => {
                        println!("    ✓ {s}");
                        pass += 1;
                    }
                    Finding::Warn(s) => {
                        println!("    ⚠ {s}");
                        warn += 1;
                    }
                    Finding::Error(s) => {
                        println!("    ✗ {s}");
                        err += 1;
                    }
                }
            }
        }
        println!();
        if err > 0 {
            println!("  {pass} passed, {warn} warning(s), {err} error(s). ✗");
        } else if warn > 0 {
            println!("  {pass} passed, {warn} warning(s). ✓");
        } else {
            println!("  {pass} checks passed. ✓");
        }
    }
}

impl Group<'_> {
    fn pass(&mut self, msg: impl Into<String>) {
        self.data.findings.push(Finding::Pass(msg.into()));
    }
    fn warn(&mut self, msg: impl Into<String>) {
        self.data.findings.push(Finding::Warn(msg.into()));
    }
    fn error(&mut self, msg: impl Into<String>) {
        self.data.findings.push(Finding::Error(msg.into()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2 * 1024), "2.0 kB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.0 MB");
        assert_eq!(format_size(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    #[test]
    fn parse_javac_major() {
        assert_eq!(parse_major("21"), Some(21));
        assert_eq!(parse_major("21.0.2"), Some(21));
        assert_eq!(parse_major("1.8.0_392"), Some(8));
        assert_eq!(parse_major("17.0.9+11-LTS"), Some(17));
        assert_eq!(parse_major("garbage"), None);
    }

    #[test]
    fn report_summary_buckets() {
        let mut r = Report::new();
        {
            let mut g = r.group("X");
            g.pass("a");
            g.pass("b");
            g.warn("c");
        }
        assert_eq!(r.error_count(), 0);
        {
            let mut g = r.group("Y");
            g.error("e");
        }
        assert_eq!(r.error_count(), 1);
    }
}
