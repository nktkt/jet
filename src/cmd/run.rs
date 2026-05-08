use std::process::Command;

use anyhow::{Context, Result, bail};

use super::build::{BuildArgs, do_build};
use crate::classes::detect_main_classes;
use crate::javac::{find_java, join_classpath};

pub struct RunArgs {
    pub args: Vec<String>,
}

pub fn cmd_run(args: RunArgs) -> Result<()> {
    let outputs = do_build(BuildArgs { release: false, force_resolve: false, package: None, jobs: None })?;

    let main_class = match outputs.manifest.pkg()?.main.clone() {
        Some(m) => m,
        None => {
            let candidates = detect_main_classes(&outputs.classes_dir)?;
            match candidates.len() {
                0 => bail!(
                    "no `public static void main(String[])` found under `{}`. \
                     Set `[package].main = \"com.example.Main\"` in jet.toml.",
                    outputs.classes_dir.display()
                ),
                1 => candidates.into_iter().next().unwrap(),
                _ => bail!(
                    "multiple main classes found: [{}]. Set `[package].main` in jet.toml \
                     to disambiguate.",
                    candidates.join(", ")
                ),
            }
        }
    };

    let java = find_java()?;
    let cp = join_classpath(&outputs.classpath);

    println!("  Running `{main_class}`");
    let mut cmd = Command::new(java);
    cmd.arg("-cp").arg(cp).arg(&main_class).args(&args.args);
    let status = cmd.status().context("spawning java")?;
    if !status.success() {
        bail!(
            "`{main_class}` exited with {}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into())
        );
    }
    Ok(())
}

