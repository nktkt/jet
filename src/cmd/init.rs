use std::env;

use anyhow::{Context, Result, bail};

use super::scaffold::{Scaffold, ScaffoldOpts};

pub struct InitArgs {
    pub name: Option<String>,
    pub java: u32,
    pub vcs: bool,
}

pub fn cmd_init(args: InitArgs) -> Result<()> {
    let cwd = env::current_dir().context("getting current directory")?;

    if cwd.join("jet.toml").exists() {
        bail!("`jet init` cannot run in a directory that already has a jet.toml");
    }

    let dir_name = cwd
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("could not derive a project name from `{}`", cwd.display()))?
        .to_string();
    let name = args.name.unwrap_or(dir_name);

    Scaffold {
        root: cwd,
        opts: ScaffoldOpts {
            name,
            java: args.java,
            vcs: args.vcs,
        },
        in_existing_dir: true,
    }
    .run()
}
