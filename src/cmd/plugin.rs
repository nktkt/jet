//! Plugin discovery and dispatch (git-style).
//!
//! When the user runs `jet <name>` and `<name>` is not a built-in subcommand,
//! we look for `jet-<name>` on `PATH` and exec it with the remaining
//! arguments. The plugin inherits stdio and the following env:
//!
//! - `JET_PROJECT_ROOT` — directory containing `jet.toml` (set if found by
//!   walking up from the cwd).
//! - `JET_VERSION`      — jet's version string.
//!
//! `jet plugins` lists every `jet-*` binary on PATH.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::manifest::Manifest;

/// Try to dispatch an unknown subcommand to a `jet-<name>` binary on PATH.
/// Exits the process with the plugin's exit code on success.
pub fn dispatch_external(name: &str, rest: Vec<OsString>) -> Result<()> {
    let bin_name = format!("jet-{name}");
    let bin = which::which(&bin_name).map_err(|_| {
        anyhow::anyhow!(
            "unknown subcommand `{name}` (and no `{bin_name}` on PATH)\n\
             help: built-ins: new, init, build, run, test, add, clean, package, \
             publish, jdk, tree, why, plugins"
        )
    })?;
    let mut cmd = Command::new(&bin);
    cmd.args(rest);
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(root) = Manifest::find_root(&cwd) {
            cmd.env("JET_PROJECT_ROOT", root);
        }
    }
    cmd.env("JET_VERSION", env!("CARGO_PKG_VERSION"));
    let status = cmd
        .status()
        .with_context(|| format!("spawning {}", bin.display()))?;
    if !status.success() {
        bail!(
            "`{bin_name}` exited with {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into())
        );
    }
    Ok(())
}

/// `jet plugins` — list every `jet-*` binary visible on PATH.
pub fn cmd_list() -> Result<()> {
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut seen: std::collections::HashSet<String> = Default::default();
    for entry in std::env::split_paths(&path) {
        let dir = match std::fs::read_dir(&entry) {
            Ok(d) => d,
            Err(_) => continue,
        };
        for f in dir.filter_map(|f| f.ok()) {
            let name = f.file_name().to_string_lossy().into_owned();
            // Strip platform suffix.
            let stem = name
                .strip_suffix(".exe")
                .map(str::to_string)
                .unwrap_or(name);
            let Some(plugin_name) = stem.strip_prefix("jet-") else {
                continue;
            };
            // Skip jet itself ("jet" with no plugin name) and obvious noise.
            if plugin_name.is_empty() {
                continue;
            }
            if !seen.insert(plugin_name.to_string()) {
                continue;
            }
            // Verify executable bit on Unix.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let meta = match f.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if meta.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            found.push((plugin_name.to_string(), f.path()));
        }
    }
    if found.is_empty() {
        println!(
            "  No jet plugins found on PATH.\n  \
             Drop a `jet-<name>` executable anywhere on PATH and `jet <name>` will dispatch to it."
        );
        return Ok(());
    }
    println!("  Plugins:");
    for (name, path) in &found {
        println!("    {name}  ({})", path.display());
    }
    Ok(())
}
