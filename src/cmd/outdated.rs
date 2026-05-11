//! `jet outdated` — report dependencies that have a newer version on Maven Central.
//!
//! Walks `[dependencies]` and `[dev-dependencies]` in the current project's
//! `jet.toml`, queries `search.maven.org` for each `group:artifact`, and prints
//! the deltas. Path-deps and workspace-inherited deps are skipped (we don't
//! own their version here). Exit code is always 0 even when updates are
//! available — `jet outdated` is informational; `jet update` does the work.

use anyhow::Result;

use crate::manifest::{DepSpec, Manifest};
use crate::registry;

pub struct OutdatedArgs {
    /// Include `-M*`, `-RC*`, `-alpha*`, `-beta*`, `-SNAPSHOT`, `-pre*`,
    /// `-dev*` versions in the "latest" suggestion. A dep whose current
    /// pin is itself a prerelease auto-allows prereleases for that dep
    /// regardless of this flag (so users don't get stuck).
    pub allow_prereleases: bool,
}

pub fn cmd_outdated(args: OutdatedArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = Manifest::find_root(&cwd)?;
    let manifest = Manifest::load(&root)?;

    let mut rows: Vec<Row> = Vec::new();
    collect(&manifest.dependencies, "dependencies", &mut rows);
    collect(&manifest.dev_dependencies, "dev-dependencies", &mut rows);

    if rows.is_empty() {
        println!("No checkable dependencies in jet.toml.");
        return Ok(());
    }

    let mut updates = 0usize;
    let mut errors = 0usize;
    for row in &rows {
        // Auto-allow prereleases when the existing pin is one — otherwise
        // a user on `5.13.0-M3` would never get bumped to `5.13.0-M4`.
        let allow = args.allow_prereleases || registry::is_prerelease(&row.current);
        match registry::latest_version(&row.group, &row.artifact, allow) {
            Ok(Some(latest)) => {
                if registry::is_newer(&row.current, &latest) {
                    println!(
                        "  {key} : {current} → {latest}  [{table}]",
                        key = row.key,
                        current = row.current,
                        latest = latest,
                        table = row.table,
                    );
                    updates += 1;
                }
            }
            Ok(None) => {
                eprintln!("  warning: {} not found on Maven Central", row.key);
            }
            Err(e) => {
                eprintln!("  warning: lookup failed for {}: {e}", row.key);
                errors += 1;
            }
        }
    }

    if updates == 0 && errors == 0 {
        println!("All dependencies are up to date.");
    } else if updates > 0 {
        let s = if updates == 1 { "" } else { "s" };
        println!("\n{updates} update{s} available. Run `jet update` to apply.");
    }

    Ok(())
}

struct Row {
    key: String,
    group: String,
    artifact: String,
    current: String,
    table: &'static str,
}

fn collect(
    deps: &std::collections::BTreeMap<String, DepSpec>,
    table: &'static str,
    out: &mut Vec<Row>,
) {
    for (key, spec) in deps {
        if spec.path().is_some() || spec.inherits_workspace() {
            continue;
        }
        let version = spec.version();
        if version.is_empty() {
            continue;
        }
        let Some((group, artifact)) = key.split_once(':') else {
            continue;
        };
        out.push(Row {
            key: key.clone(),
            group: group.to_string(),
            artifact: artifact.to_string(),
            current: version.to_string(),
            table,
        });
    }
}
