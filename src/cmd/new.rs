use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use super::scaffold::{Scaffold, ScaffoldOpts};

pub struct NewArgs {
    pub path: String,
    pub name: Option<String>,
    pub java: u32,
    pub vcs: bool,
}

pub fn cmd_new(args: NewArgs) -> Result<()> {
    let path = PathBuf::from(&args.path);

    if path.exists() {
        if path.is_file() {
            bail!(
                "destination `{}` is a file, not a directory",
                path.display()
            );
        }
        let is_empty = fs::read_dir(&path)
            .with_context(|| format!("reading {}", path.display()))?
            .next()
            .is_none();
        if !is_empty {
            bail!(
                "destination `{}` already exists and is not empty. \
                 Use `jet init` to initialize a project in an existing directory.",
                path.display()
            );
        }
    } else {
        fs::create_dir_all(&path)
            .with_context(|| format!("creating {}", path.display()))?;
    }

    let dir_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("could not derive a project name from `{}`", path.display()))?
        .to_string();
    let name = args.name.unwrap_or(dir_name);

    Scaffold {
        root: path,
        opts: ScaffoldOpts {
            name,
            java: args.java,
            vcs: args.vcs,
        },
        in_existing_dir: false,
    }
    .run()
}
