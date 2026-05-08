use std::fs;

use anyhow::{Context, Result};

use crate::manifest::Manifest;

pub fn cmd_clean() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = Manifest::find_root(&cwd)?;
    let target = root.join("target");
    if target.exists() {
        fs::remove_dir_all(&target)
            .with_context(|| format!("removing {}", target.display()))?;
        println!("  Removed {}", target.display());
    } else {
        println!("  Nothing to clean");
    }
    Ok(())
}
