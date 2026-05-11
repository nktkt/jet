//! `jet import` — convert a Maven `pom.xml` into a `jet.toml`.
//!
//! Reads the POM in the current directory using the existing
//! `crate::resolver::pom::Pom` parser, extracts the project's coordinates,
//! Java version, and dependencies, and emits a new `jet.toml` next to it.
//!
//! Scope mapping:
//! - `compile`, `runtime` → `[dependencies]`
//! - `test`               → `[dev-dependencies]`
//! - `provided`, `system` → skipped with a warning (these are JVM-level
//!   classpath contracts that don't translate to jet's resolver model)
//! - `import` (BOM in `<dependencyManagement>`) → skipped with a note
//!
//! Properties referenced by `${...}` are interpolated against the POM's
//! own `<properties>` block (no parent-chain resolution — keeps the
//! importer offline and self-contained).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::resolver::pom::Pom;

pub struct ImportArgs {
    /// Overwrite an existing `jet.toml` instead of erroring out.
    pub force: bool,
}

pub fn cmd_import(args: ImportArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let pom_path = cwd.join("pom.xml");
    if !pom_path.is_file() {
        bail!(
            "no `pom.xml` in `{}`. `jet import` expects a Maven project root.",
            cwd.display()
        );
    }
    let target = cwd.join("jet.toml");
    if target.is_file() && !args.force {
        bail!(
            "`{}` already exists. Re-run with `--force` to overwrite.",
            target.display()
        );
    }

    let xml = fs::read(&pom_path)
        .with_context(|| format!("reading {}", pom_path.display()))?;
    let pom = Pom::parse(&xml)
        .with_context(|| format!("parsing {}", pom_path.display()))?;

    let group = require(&pom.group_id, "groupId", &pom_path)?;
    let artifact = require(&pom.artifact_id, "artifactId", &pom_path)?;
    let version = require(&pom.version, "version", &pom_path)?;
    let java = detect_java_version(&pom);

    let mut main_deps: Vec<(String, String)> = Vec::new();
    let mut dev_deps: Vec<(String, String)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    let no_extra = HashMap::new();
    for dep in &pom.dependencies {
        if dep.optional {
            warnings.push(format!(
                "skipping optional dep `{}:{}` (jet has no optional model)",
                dep.group_id, dep.artifact_id
            ));
            continue;
        }
        let g = pom.interpolate(&dep.group_id, &no_extra);
        let a = pom.interpolate(&dep.artifact_id, &no_extra);
        let v = match dep.version.as_deref() {
            Some(v) => pom.interpolate(v, &no_extra),
            None => {
                warnings.push(format!(
                    "skipping `{g}:{a}` — no version (`<dependencyManagement>` \
                     inheritance is not followed by `jet import`; pin the version \
                     in pom.xml or edit jet.toml after import)"
                ));
                continue;
            }
        };
        if v.is_empty() || v.contains("${") {
            warnings.push(format!(
                "skipping `{g}:{a}` — version `{v}` could not be interpolated"
            ));
            continue;
        }
        let scope = dep.scope.as_deref().unwrap_or("compile");
        let key = format!("{g}:{a}");
        match scope {
            "compile" | "runtime" => main_deps.push((key, v)),
            "test" => dev_deps.push((key, v)),
            "provided" | "system" => warnings.push(format!(
                "skipping `{key}` (scope=`{scope}` is JVM-level; doesn't translate to jet)"
            )),
            "import" => warnings.push(format!(
                "skipping `{key}` (BOM import scope; not yet supported by `jet import` — \
                 expand to concrete versions manually after import)"
            )),
            other => warnings.push(format!(
                "skipping `{key}` with unknown scope `{other}`"
            )),
        }
    }

    main_deps.sort_by(|a, b| a.0.cmp(&b.0));
    main_deps.dedup_by(|a, b| a.0 == b.0);
    dev_deps.sort_by(|a, b| a.0.cmp(&b.0));
    dev_deps.dedup_by(|a, b| a.0 == b.0);

    let toml = render_jet_toml(&group, &artifact, &version, java, &main_deps, &dev_deps);
    fs::write(&target, toml)
        .with_context(|| format!("writing {}", target.display()))?;
    println!("  Wrote {}", target.display());
    println!(
        "    [package] {artifact}:{version} (group {group}, java {java})"
    );
    println!(
        "    {} main + {} dev deps imported.",
        main_deps.len(),
        dev_deps.len()
    );
    for w in &warnings {
        eprintln!("  note: {w}");
    }
    println!();
    println!("  Next: `jet build` to resolve transitives and write jet.lock.");
    Ok(())
}

fn require(field: &str, label: &str, path: &Path) -> Result<String> {
    if field.is_empty() {
        bail!(
            "{} has no <{}> (parent POM inheritance is not followed by `jet import`)",
            path.display(),
            label
        );
    }
    Ok(field.to_string())
}

/// Detect the Java version from common POM places (in order):
/// 1. `<maven.compiler.release>` property
/// 2. `<maven.compiler.target>` property
/// 3. `<java.version>` property
/// 4. fall back to 21
fn detect_java_version(pom: &Pom) -> u32 {
    let candidates = [
        "maven.compiler.release",
        "maven.compiler.target",
        "java.version",
    ];
    for key in candidates {
        if let Some(v) = pom.properties.get(key) {
            // Strip the legacy `1.X` prefix that Maven sometimes uses.
            let trimmed = v.strip_prefix("1.").unwrap_or(v);
            if let Ok(n) = trimmed.trim().parse::<u32>() {
                if (8..=25).contains(&n) {
                    return n;
                }
            }
        }
    }
    21
}

fn render_jet_toml(
    group: &str,
    artifact: &str,
    version: &str,
    java: u32,
    main_deps: &[(String, String)],
    dev_deps: &[(String, String)],
) -> String {
    let mut s = String::with_capacity(512);
    s.push_str("# Generated by `jet import` from pom.xml.\n");
    s.push_str("# Review and edit before committing.\n\n");
    s.push_str("[package]\n");
    s.push_str(&format!("name    = \"{artifact}\"\n"));
    s.push_str(&format!("version = \"{version}\"\n"));
    s.push_str(&format!("java    = {java}\n"));
    s.push_str("edition = \"2026\"\n");
    s.push_str(&format!("group   = \"{group}\"\n"));
    s.push('\n');
    s.push_str("[dependencies]\n");
    for (key, version) in main_deps {
        s.push_str(&format!("\"{key}\" = \"{version}\"\n"));
    }
    s.push('\n');
    s.push_str("[dev-dependencies]\n");
    for (key, version) in dev_deps {
        s.push_str(&format!("\"{key}\" = \"{version}\"\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pom_with(props: &str, deps: &str) -> Pom {
        let xml = format!(
            r#"<?xml version="1.0"?>
<project>
  <groupId>io.example</groupId>
  <artifactId>thing</artifactId>
  <version>0.1.0</version>
  <properties>{props}</properties>
  <dependencies>{deps}</dependencies>
</project>"#
        );
        Pom::parse(xml.as_bytes()).unwrap()
    }

    #[test]
    fn detects_maven_compiler_release() {
        let pom = pom_with("<maven.compiler.release>21</maven.compiler.release>", "");
        assert_eq!(detect_java_version(&pom), 21);
    }

    #[test]
    fn falls_back_to_21() {
        let pom = pom_with("", "");
        assert_eq!(detect_java_version(&pom), 21);
    }

    #[test]
    fn strips_legacy_1x_prefix() {
        let pom = pom_with("<maven.compiler.target>1.8</maven.compiler.target>", "");
        assert_eq!(detect_java_version(&pom), 8);
    }

    #[test]
    fn renders_jet_toml_with_edition() {
        let s = render_jet_toml(
            "io.example",
            "thing",
            "0.1.0",
            21,
            &[("org.slf4j:slf4j-api".into(), "2.0.13".into())],
            &[("org.junit.jupiter:junit-jupiter".into(), "5.10.2".into())],
        );
        assert!(s.contains("edition = \"2026\""));
        assert!(s.contains("group   = \"io.example\""));
        assert!(s.contains("\"org.slf4j:slf4j-api\" = \"2.0.13\""));
        assert!(s.contains("[dev-dependencies]"));
        assert!(s.contains("\"org.junit.jupiter:junit-jupiter\" = \"5.10.2\""));
    }
}

// Allow `PathBuf` to remain imported when only used internally.
#[allow(dead_code)]
fn _phantom_use_pathbuf(p: PathBuf) -> PathBuf {
    p
}
