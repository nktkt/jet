//! `jet licenses [--detail] [--scope <s>]` — list all transitive license
//! grants pulled in by `jet.lock`. Aggregated by license name by default
//! (most production teams want a "which licenses are we shipping under?"
//! summary at glance), `--detail` switches to a per-package listing.
//!
//! POMs are fetched through the existing `Fetcher`, so the local Maven
//! cache is shared with builds — first run touches the network for every
//! coord we haven't built before, subsequent runs are local-disk-only.

use std::collections::BTreeMap;

use anyhow::Result;

use crate::cmd::info::parse_info;
use crate::coord::Coord;
use crate::lockfile::{LOCKFILE_NAME, Lockfile, Package};
use crate::manifest::Manifest;
use crate::resolver::{Fetcher, default_repos};

pub struct LicensesArgs {
    /// Show every package's license individually, instead of grouping by
    /// license name.
    pub detail: bool,
    /// `compile`, `runtime`, `test`, or `all` (default: compile+runtime).
    /// Anything else bails with a clear error.
    pub scope: Option<String>,
}

pub fn cmd_licenses(args: LicensesArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = Manifest::find_root(&cwd)?;
    if !root.join(LOCKFILE_NAME).is_file() {
        anyhow::bail!(
            "no jet.lock at {} — run `jet build` first so the lockfile is generated",
            root.join(LOCKFILE_NAME).display()
        );
    }
    let lock = Lockfile::load(&root)?;
    let filter = parse_scope(args.scope.as_deref())?;
    let packages: Vec<&Package> = lock
        .packages
        .iter()
        .filter(|p| filter.matches(&p.scope))
        .collect();

    if packages.is_empty() {
        println!("No packages match the selected scope.");
        return Ok(());
    }

    let fetcher = Fetcher::new(default_repos())?;
    let mut rows: Vec<Row> = Vec::with_capacity(packages.len());
    for p in &packages {
        let coord = Coord {
            group: p.name.split_once(':').map(|(g, _)| g).unwrap_or("").into(),
            artifact: p.name.split_once(':').map(|(_, a)| a).unwrap_or("").into(),
            version: p.version.clone(),
            classifier: p.classifier.clone(),
            ty: "pom".into(),
        };
        let licenses = match fetcher.fetch_pom(&coord) {
            Ok(bytes) => parse_info(&bytes)
                .map(|i| {
                    i.licenses
                        .into_iter()
                        .map(|l| l.name.unwrap_or_else(|| "(unnamed)".into()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            Err(e) => {
                eprintln!("  warning: failed to read POM for {coord}: {e:#}");
                Vec::new()
            }
        };
        rows.push(Row {
            key: p.name.clone(),
            version: p.version.clone(),
            scope: p.scope.clone(),
            licenses: if licenses.is_empty() {
                vec!["(no license declared)".into()]
            } else {
                licenses
            },
        });
    }

    if args.detail {
        print_detail(&rows);
    } else {
        print_aggregated(&rows);
    }
    Ok(())
}

struct Row {
    key: String,
    version: String,
    scope: String,
    licenses: Vec<String>,
}

#[derive(Clone, Copy)]
enum ScopeFilter {
    CompileRuntime,
    All,
    Only(&'static str),
}

impl ScopeFilter {
    fn matches(&self, scope: &str) -> bool {
        match self {
            ScopeFilter::All => true,
            ScopeFilter::CompileRuntime => scope == "compile" || scope == "runtime",
            ScopeFilter::Only(s) => scope == *s,
        }
    }
}

fn parse_scope(s: Option<&str>) -> Result<ScopeFilter> {
    Ok(match s {
        None => ScopeFilter::CompileRuntime,
        Some("all") => ScopeFilter::All,
        Some("compile") => ScopeFilter::Only("compile"),
        Some("runtime") => ScopeFilter::Only("runtime"),
        Some("test") => ScopeFilter::Only("test"),
        Some(other) => anyhow::bail!(
            "unknown --scope `{other}` (expected: compile, runtime, test, all)"
        ),
    })
}

fn print_aggregated(rows: &[Row]) {
    let mut by_license: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in rows {
        // A package may declare multiple licenses (dual-licensed) — count it
        // under each so the totals reflect the real grant surface.
        for lic in &r.licenses {
            by_license
                .entry(normalize_license(lic))
                .or_default()
                .push(format!("{} = \"{}\"  [{}]", r.key, r.version, r.scope));
        }
    }
    let mut entries: Vec<(&String, &Vec<String>)> = by_license.iter().collect();
    entries.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    let total_pkgs = rows.len();
    println!(
        "{} packages, {} distinct license entries",
        total_pkgs,
        entries.len(),
    );
    println!();
    for (lic, pkgs) in entries {
        println!("  {lic}  ({} packages)", pkgs.len());
        for p in pkgs {
            println!("    {p}");
        }
        println!();
    }
}

fn print_detail(rows: &[Row]) {
    println!("{} packages", rows.len());
    println!();
    let key_w = rows
        .iter()
        .map(|r| r.key.len() + r.version.len() + 1)
        .max()
        .unwrap_or(0);
    for r in rows {
        let coord = format!("{}:{}", r.key, r.version);
        let pad = " ".repeat(key_w.saturating_sub(coord.len()));
        let lics = r
            .licenses
            .iter()
            .map(|s| normalize_license(s))
            .collect::<Vec<_>>()
            .join(", ");
        println!("  {coord}{pad}  [{}]  {lics}", r.scope);
    }
}

/// Best-effort normalization of free-text license names to a canonical form
/// so duplicates aggregate cleanly. The full SPDX mapping table is overkill
/// for v1; cover the handful of names that actually appear on Maven Central
/// in different spellings and let the rest pass through verbatim.
fn normalize_license(raw: &str) -> String {
    let t = raw.trim();
    let lc = t.to_ascii_lowercase();
    // Common name → SPDX identifier collapses.
    if lc.contains("apache") && lc.contains("2") {
        return "Apache-2.0".into();
    }
    if lc == "mit license" || lc == "mit" || lc.contains("mit license") {
        return "MIT".into();
    }
    if lc.contains("bsd 2") || lc.contains("bsd-2") {
        return "BSD-2-Clause".into();
    }
    if lc.contains("bsd 3") || lc.contains("bsd-3") || lc.contains("new bsd") {
        return "BSD-3-Clause".into();
    }
    if lc.contains("eclipse public license") && (lc.contains("2") || lc.contains("v2")) {
        return "EPL-2.0".into();
    }
    if lc.contains("eclipse public license") && (lc.contains("1") || lc.contains("v1")) {
        return "EPL-1.0".into();
    }
    if lc.contains("lgpl") && lc.contains("2.1") {
        return "LGPL-2.1".into();
    }
    if lc.contains("lgpl") && lc.contains("3") {
        return "LGPL-3.0".into();
    }
    if lc.contains("gpl") && lc.contains("3") && !lc.contains("lgpl") {
        return "GPL-3.0".into();
    }
    if lc.contains("cddl") {
        return "CDDL-1.1".into();
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_common_names() {
        assert_eq!(normalize_license("Apache License, Version 2.0"), "Apache-2.0");
        assert_eq!(normalize_license("The Apache Software License, Version 2.0"), "Apache-2.0");
        assert_eq!(normalize_license("Apache-2.0"), "Apache-2.0");
        assert_eq!(normalize_license("MIT License"), "MIT");
        assert_eq!(normalize_license("Eclipse Public License v2.0"), "EPL-2.0");
        assert_eq!(normalize_license("Eclipse Public License - v 1.0"), "EPL-1.0");
        assert_eq!(normalize_license("BSD 3-Clause License"), "BSD-3-Clause");
        assert_eq!(normalize_license("New BSD License"), "BSD-3-Clause");
        // Unknown free text passes through.
        assert_eq!(normalize_license("Acme Corp. License"), "Acme Corp. License");
    }

    #[test]
    fn parse_scope_accepts_known_values() {
        assert!(matches!(parse_scope(None).unwrap(), ScopeFilter::CompileRuntime));
        assert!(matches!(parse_scope(Some("all")).unwrap(), ScopeFilter::All));
        assert!(matches!(parse_scope(Some("compile")).unwrap(), ScopeFilter::Only("compile")));
        assert!(matches!(parse_scope(Some("runtime")).unwrap(), ScopeFilter::Only("runtime")));
        assert!(matches!(parse_scope(Some("test")).unwrap(), ScopeFilter::Only("test")));
        assert!(parse_scope(Some("bogus")).is_err());
    }
}
