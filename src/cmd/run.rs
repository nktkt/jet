use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use walkdir::WalkDir;

use super::build::{BuildArgs, do_build};
use crate::javac::{find_java, join_classpath};

pub struct RunArgs {
    pub args: Vec<String>,
}

pub fn cmd_run(args: RunArgs) -> Result<()> {
    let outputs = do_build(BuildArgs { release: false, force_resolve: false })?;

    let main_class = match outputs.manifest.package.main.clone() {
        Some(m) => m,
        None => detect_main_class(&outputs.classes_dir)?,
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

/// Scan `target/classes` for class files containing `public static void main`.
/// This is a coarse heuristic — we look for the literal byte pattern of the
/// `main([Ljava/lang/String;)V` UTF-8 entry plus the class name. Cheap and
/// good enough for `jet run` UX; users can always set `[package].main`.
fn detect_main_class(classes_dir: &Path) -> Result<String> {
    if !classes_dir.is_dir() {
        bail!(
            "classes directory `{}` does not exist (did the build succeed?)",
            classes_dir.display()
        );
    }
    let mut candidates: Vec<String> = Vec::new();
    for entry in WalkDir::new(classes_dir).into_iter().filter_map(|e| e.ok()) {
        let p = entry.path();
        if !p.is_file() || p.extension().and_then(|s| s.to_str()) != Some("class") {
            continue;
        }
        if let Ok(bytes) = std::fs::read(p) {
            if has_main_method_signature(&bytes) {
                if let Some(class_name) = class_name_from_path(classes_dir, p) {
                    candidates.push(class_name);
                }
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    match candidates.len() {
        0 => bail!(
            "no `public static void main(String[])` found under `{}`. \
             Set `[package].main = \"com.example.Main\"` in jet.toml.",
            classes_dir.display()
        ),
        1 => Ok(candidates.remove(0)),
        _ => bail!(
            "multiple main classes found: [{}]. Set `[package].main` in jet.toml \
             to disambiguate.",
            candidates.join(", ")
        ),
    }
}

fn has_main_method_signature(class_bytes: &[u8]) -> bool {
    // Look for the descriptor + method name in the constant pool. This is a
    // string match in the .class file, which is correct in practice because
    // both strings appear verbatim as Modified-UTF8 entries.
    let needle = b"([Ljava/lang/String;)V";
    bytes_contain(class_bytes, needle) && bytes_contain(class_bytes, b"main")
}

fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle)
}

fn class_name_from_path(classes_dir: &Path, class_file: &Path) -> Option<String> {
    let rel = class_file.strip_prefix(classes_dir).ok()?;
    let stem = rel.with_extension("");
    let parts: Vec<String> = stem
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("."))
}

#[allow(dead_code)]
fn dummy_to_quiet_unused(_: &PathBuf) {}
