//! `jet fmt [--check]` — format Java source via google-java-format.
//!
//! Walks `src/main/java`, `src/main/resources`, `src/test/java`, and
//! `src/test/resources` for `.java` files (resources dirs are walked too,
//! but only `.java` files match — Java sources very occasionally appear
//! under `resources/` for code-generation tests), then invokes the
//! google-java-format all-deps JAR cached under `~/.jet/tools/`.
//!
//! With `--check`, no files are modified; the process exits non-zero if
//! any file would have changed. Suitable for pre-commit hooks and CI.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use walkdir::WalkDir;

use crate::javac::find_java;
use crate::manifest::Manifest;
use crate::tools::{GJF_VERSION, ensure_gjf};

pub struct FmtArgs {
    pub check: bool,
}

pub fn cmd_fmt(args: FmtArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = Manifest::find_root(&cwd)?;

    let sources = collect_java_sources(&root)?;
    if sources.is_empty() {
        println!("No Java sources to format under {}.", root.display());
        return Ok(());
    }

    let gjf = ensure_gjf()
        .context("downloading google-java-format")?;
    let java = find_java()
        .context("no `java` on PATH (jet fmt needs a JRE to run google-java-format)")?;

    let mode_label = if args.check { "checking" } else { "formatting" };
    println!(
        "{} {} files with google-java-format {GJF_VERSION}…",
        mode_label,
        sources.len(),
    );

    // GJF needs JDK-internal compiler API access on JDK 16+.
    let exports = &[
        "--add-exports=jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED",
        "--add-exports=jdk.compiler/com.sun.tools.javac.code=ALL-UNNAMED",
        "--add-exports=jdk.compiler/com.sun.tools.javac.file=ALL-UNNAMED",
        "--add-exports=jdk.compiler/com.sun.tools.javac.parser=ALL-UNNAMED",
        "--add-exports=jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED",
        "--add-exports=jdk.compiler/com.sun.tools.javac.util=ALL-UNNAMED",
    ];

    let mut cmd = Command::new(&java);
    for e in exports {
        cmd.arg(e);
    }
    cmd.arg("-jar").arg(&gjf);
    if args.check {
        cmd.arg("--dry-run").arg("--set-exit-if-changed");
    } else {
        cmd.arg("--replace");
    }
    for s in &sources {
        cmd.arg(s);
    }

    let status = cmd
        .status()
        .with_context(|| format!("invoking {}", java.display()))?;
    if !status.success() {
        if args.check {
            // GJF prints the offending file list to stdout in --dry-run mode.
            bail!(
                "formatting check failed — run `jet fmt` to apply fixes (or commit the diff)"
            );
        }
        bail!(
            "google-java-format exited with status {} — see output above",
            status.code().map(|c| c.to_string()).unwrap_or_else(|| "<signal>".into()),
        );
    }

    if args.check {
        println!("All files are formatted correctly.");
    } else {
        println!("Reformatted {} files.", sources.len());
    }
    Ok(())
}

fn collect_java_sources(root: &std::path::Path) -> Result<Vec<PathBuf>> {
    let dirs = [
        root.join("src/main/java"),
        root.join("src/test/java"),
    ];
    let mut out: Vec<PathBuf> = Vec::new();
    for d in &dirs {
        if !d.is_dir() {
            continue;
        }
        for entry in WalkDir::new(d).into_iter().filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("java") {
                out.push(p.to_path_buf());
            }
        }
    }
    out.sort();
    Ok(out)
}
