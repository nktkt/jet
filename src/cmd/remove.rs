//! `jet remove <coord>` — drop a dependency from `jet.toml` and refresh `jet.lock`.
//!
//! Accepts `group:artifact` or `group:artifact:version` (version is ignored:
//! the manifest key has only group + artifact). `--dev` targets
//! `[dev-dependencies]`. If the dep isn't present in the chosen table, bail
//! with a clear error rather than silently no-op.

use anyhow::{Context, Result, bail};

use crate::cmd::build::{BuildArgs, do_build};
use crate::manifest::{MANIFEST_FILENAME, Manifest};

pub struct RemoveArgs {
    pub coord: String,
    /// Remove from `[dev-dependencies]` instead of `[dependencies]`.
    pub dev: bool,
}

pub fn cmd_remove(args: RemoveArgs) -> Result<()> {
    let key = parse_key(&args.coord)?;

    let cwd = std::env::current_dir()?;
    let root = Manifest::find_root(&cwd)?;
    let manifest = Manifest::load(&root)?;
    let manifest_path = root.join(MANIFEST_FILENAME);

    let table = if args.dev { "dev-dependencies" } else { "dependencies" };
    let present = if args.dev {
        manifest.dev_dependencies.contains_key(&key)
    } else {
        manifest.dependencies.contains_key(&key)
    };
    if !present {
        // Surface the helpful hint if it's actually in the other table.
        let other_table = if args.dev { "dependencies" } else { "dev-dependencies" };
        let in_other = if args.dev {
            manifest.dependencies.contains_key(&key)
        } else {
            manifest.dev_dependencies.contains_key(&key)
        };
        if in_other {
            bail!(
                "dependency `{key}` is in [{other_table}], not [{table}] \
                 (try `jet remove {}{key}`)",
                if args.dev { "" } else { "--dev " }
            );
        }
        bail!("dependency `{key}` is not in [{table}]");
    }

    let removed = Manifest::remove_dep_from_table(&manifest_path, table, &key)
        .with_context(|| format!("removing {key} from [{table}]"))?;
    if !removed {
        // Should be unreachable — we just checked `present` — but keep the
        // error explicit in case the manifest is mutated underneath us.
        bail!("dependency `{key}` vanished from [{table}] before remove could run");
    }
    println!("  Removed `{key}` from [{table}]");

    println!("  Resolving dependencies…");
    do_build(BuildArgs {
        release: false,
        force_resolve: true,
        package: None,
        jobs: None,
        no_cache: false,
        check_only: false,
    })?;

    Ok(())
}

fn parse_key(s: &str) -> Result<String> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.as_slice() {
        [g, a] | [g, a, _] => {
            if g.is_empty() || a.is_empty() {
                bail!("coord `{s}` has an empty group or artifact");
            }
            Ok(format!("{g}:{a}"))
        }
        _ => bail!("coord `{s}` must be `group:artifact` or `group:artifact:version`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_key_two_or_three_segments() {
        assert_eq!(parse_key("a.b:c").unwrap(), "a.b:c");
        assert_eq!(parse_key("a.b:c:1.0").unwrap(), "a.b:c");
    }

    #[test]
    fn parse_key_rejects_bad() {
        assert!(parse_key("nope").is_err());
        assert!(parse_key(":").is_err());
        assert!(parse_key("a:").is_err());
        assert!(parse_key(":b").is_err());
    }
}
