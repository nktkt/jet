//! `jet update [coord]` — bump dependencies to their latest Maven Central version.
//!
//! Without an argument: walks every entry in `[dependencies]` and
//! `[dev-dependencies]`, queries Maven Central, and rewrites `jet.toml` to
//! pin the latest published version. With `<group>:<artifact>` (or the full
//! `group:artifact:version`, version ignored), updates only that single key.
//!
//! After editing `jet.toml`, calls `do_build` with `force_resolve = true`
//! so `jet.lock` is regenerated against the new versions.
//!
//! Path-deps and workspace-inherited deps are skipped (we don't own their
//! version here). When no key changes, `jet.lock` regeneration is also
//! skipped to keep the no-op cheap.

use anyhow::{Context, Result, bail};

use crate::cmd::build::{BuildArgs, do_build};
use crate::manifest::{DepSpec, MANIFEST_FILENAME, Manifest};
use crate::registry;

pub struct UpdateArgs {
    /// Optional `group:artifact` (or `group:artifact:version`) restricting the
    /// update to one dep. `None` means "all deps".
    pub coord: Option<String>,
}

pub fn cmd_update(args: UpdateArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = Manifest::find_root(&cwd)?;
    let manifest = Manifest::load(&root)?;
    let manifest_path = root.join(MANIFEST_FILENAME);

    let target_key = args.coord.as_deref().map(parse_key).transpose()?;

    let mut planned: Vec<Plan> = Vec::new();
    plan(&manifest.dependencies, "dependencies", target_key.as_deref(), &mut planned);
    plan(&manifest.dev_dependencies, "dev-dependencies", target_key.as_deref(), &mut planned);

    if let Some(k) = &target_key {
        if planned.is_empty() {
            bail!(
                "`{k}` is not present in [dependencies] or [dev-dependencies] \
                 (or it's a path/workspace dep that jet update can't bump)"
            );
        }
    }

    let mut applied = 0usize;
    for p in &planned {
        match registry::latest_version(&p.group, &p.artifact) {
            Ok(Some(latest)) => {
                if registry::is_newer(&p.current, &latest) {
                    Manifest::add_dep_to_table(&manifest_path, p.table, &p.key, &latest)
                        .with_context(|| {
                            format!("rewriting {} in [{}]", p.key, p.table)
                        })?;
                    println!(
                        "  {key} : {current} → {latest}  [{table}]",
                        key = p.key,
                        current = p.current,
                        latest = latest,
                        table = p.table,
                    );
                    applied += 1;
                }
            }
            Ok(None) => {
                eprintln!("  warning: {} not found on Maven Central", p.key);
            }
            Err(e) => {
                eprintln!("  warning: lookup failed for {}: {e}", p.key);
            }
        }
    }

    if applied == 0 {
        println!("Nothing to update — all matched dependencies are at their latest version.");
        return Ok(());
    }

    println!("  Resolving dependencies…");
    do_build(BuildArgs {
        release: false,
        force_resolve: true,
        package: None,
        jobs: None,
        no_cache: false,
    })?;

    Ok(())
}

struct Plan {
    key: String,
    group: String,
    artifact: String,
    current: String,
    table: &'static str,
}

fn plan(
    deps: &std::collections::BTreeMap<String, DepSpec>,
    table: &'static str,
    only: Option<&str>,
    out: &mut Vec<Plan>,
) {
    for (key, spec) in deps {
        if spec.path().is_some() || spec.inherits_workspace() {
            continue;
        }
        if let Some(filter) = only {
            if key != filter {
                continue;
            }
        }
        let version = spec.version();
        if version.is_empty() {
            continue;
        }
        let Some((group, artifact)) = key.split_once(':') else {
            continue;
        };
        out.push(Plan {
            key: key.clone(),
            group: group.to_string(),
            artifact: artifact.to_string(),
            current: version.to_string(),
            table,
        });
    }
}

/// Accept `group:artifact` or `group:artifact:version`. The version segment
/// is ignored — `jet update <key>` always picks "latest". Returns the
/// `group:artifact` key used in `jet.toml`.
fn parse_key(s: &str) -> Result<String> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.as_slice() {
        [g, a] => {
            if g.is_empty() || a.is_empty() {
                bail!("coord `{s}` has an empty group or artifact");
            }
            Ok(format!("{g}:{a}"))
        }
        [g, a, _v] => {
            if g.is_empty() || a.is_empty() {
                bail!("coord `{s}` has an empty group or artifact");
            }
            Ok(format!("{g}:{a}"))
        }
        _ => bail!(
            "coord `{s}` must be `group:artifact` or `group:artifact:version`"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_two_segments() {
        assert_eq!(parse_key("com.google.guava:guava").unwrap(), "com.google.guava:guava");
    }

    #[test]
    fn parse_key_three_segments_drops_version() {
        assert_eq!(
            parse_key("com.google.guava:guava:33.0.0-jre").unwrap(),
            "com.google.guava:guava"
        );
    }

    #[test]
    fn parse_key_rejects_single_segment() {
        assert!(parse_key("guava").is_err());
    }
}
