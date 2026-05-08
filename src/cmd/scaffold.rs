use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::template::{default_java_package, render_gitignore, render_main_java, render_manifest};
use crate::validate::validate_project_name;

/// Common options for both `new` and `init`.
pub struct ScaffoldOpts {
    pub name: String,
    pub java: u32,
    pub vcs: bool,
}

/// What `Scaffold::run` should do when a target file already exists on disk.
#[derive(Clone, Copy)]
enum WriteMode {
    /// Always create. Error if the file already exists. Used by `jet new`.
    CreateNew,
    /// Skip silently if the file already exists. Used by `jet init`.
    SkipIfExists,
}

pub struct Scaffold {
    pub root: PathBuf,
    pub opts: ScaffoldOpts,
    /// `false` for `new` (target dir didn't exist before), `true` for `init`.
    pub in_existing_dir: bool,
}

impl Scaffold {
    pub fn run(&self) -> Result<()> {
        let started = Instant::now();
        validate_project_name(&self.opts.name)?;

        let mode = if self.in_existing_dir {
            WriteMode::SkipIfExists
        } else {
            WriteMode::CreateNew
        };

        let pkg = default_java_package(&self.opts.name);
        let pkg_path: PathBuf = pkg.split('.').collect();
        let main_java_dir = self.root.join("src/main/java").join(&pkg_path);
        let test_java_dir = self.root.join("src/test/java").join(&pkg_path);
        let resources_dir = self.root.join("src/main/resources");

        fs::create_dir_all(&main_java_dir)
            .with_context(|| format!("creating {}", main_java_dir.display()))?;
        fs::create_dir_all(&test_java_dir)
            .with_context(|| format!("creating {}", test_java_dir.display()))?;
        fs::create_dir_all(&resources_dir)
            .with_context(|| format!("creating {}", resources_dir.display()))?;

        write_file(
            &self.root.join("jet.toml"),
            &render_manifest(&self.opts.name, "0.1.0", self.opts.java),
            mode,
        )?;
        write_file(
            &self.root.join(".gitignore"),
            render_gitignore(),
            mode,
        )?;
        write_file(
            &main_java_dir.join("Main.java"),
            &render_main_java(&pkg),
            mode,
        )?;
        write_file(
            &resources_dir.join(".gitkeep"),
            "",
            mode,
        )?;

        if self.opts.vcs {
            try_git_init(&self.root)?;
        }

        let elapsed = started.elapsed();
        println!(
            "  Created jet project `{}` in {} ({:.0}ms)",
            self.opts.name,
            self.root.display(),
            elapsed.as_secs_f64() * 1000.0,
        );
        Ok(())
    }
}

fn write_file(path: &Path, contents: &str, mode: WriteMode) -> Result<()> {
    if path.exists() {
        match mode {
            WriteMode::CreateNew => {
                bail!("{} already exists", path.display());
            }
            WriteMode::SkipIfExists => {
                eprintln!("  note: skipping existing {}", path.display());
                return Ok(());
            }
        }
    }
    let mut f = fs::File::create(path)
        .with_context(|| format!("creating {}", path.display()))?;
    f.write_all(contents.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Run `git init --quiet` in `root`. Skipped if `.git` already exists.
/// Failures are warned, not fatal — a missing `git` binary should not break
/// `jet new`.
fn try_git_init(root: &Path) -> Result<()> {
    if root.join(".git").exists() {
        return Ok(());
    }
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => {
            eprintln!("  warning: `git init` exited with status {s}; skipping VCS setup");
            Ok(())
        }
        Err(e) => {
            eprintln!("  warning: failed to run `git`: {e}; skipping VCS setup");
            Ok(())
        }
    }
}
