//! Helpers for inspecting compiled `.class` outputs under `target/classes`.
//!
//! Used by `jet run` (which needs exactly one main class) and `jet package`
//! (which produces a library JAR if no main class is present).

use std::path::Path;

use anyhow::{Result, bail};
use walkdir::WalkDir;

/// Scan a classes directory for classes containing a `public static void
/// main(String[])` entry point.
///
/// Returns:
/// - `Err` if the directory does not exist.
/// - `Ok(vec![])` if no candidates are found.
/// - `Ok(vec![one])` for a single match.
/// - `Ok(vec![..])` for multiple matches (caller decides whether ambiguity is
///   fatal — `jet run` errors, `jet package` falls back to a library JAR).
pub fn detect_main_classes(classes_dir: &Path) -> Result<Vec<String>> {
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
    Ok(candidates)
}

/// Coarse heuristic: looks for the byte pattern of the `main([Ljava/lang/String;)V`
/// UTF-8 entry plus the class name in the constant pool. Cheap; correct in practice
/// because both strings appear verbatim as Modified-UTF-8 entries.
fn has_main_method_signature(class_bytes: &[u8]) -> bool {
    let needle = b"([Ljava/lang/String;)V";
    bytes_contain(class_bytes, needle) && bytes_contain(class_bytes, b"main")
}

fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// `target/classes/com/example/Main.class` -> `com.example.Main`.
pub fn class_name_from_path(classes_dir: &Path, class_file: &Path) -> Option<String> {
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
