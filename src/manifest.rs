//! `jet.toml` parser. Edit-preserving via `toml_edit`, typed via serde.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use toml_edit::DocumentMut;

use crate::validate::validate_project_name;

pub const MANIFEST_FILENAME: &str = "jet.toml";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Optional in workspace-root manifests that have only a `[workspace]` table
    /// (a "virtual manifest" — pure aggregator with no package of its own).
    #[serde(default)]
    pub package: Option<PackageMeta>,
    #[serde(default)]
    pub workspace: Option<WorkspaceTable>,
    #[serde(default)]
    pub dependencies: BTreeMap<String, DepSpec>,
    #[serde(default, rename = "dev-dependencies")]
    pub dev_dependencies: BTreeMap<String, DepSpec>,
    #[serde(default)]
    pub repositories: BTreeMap<String, Repository>,
    #[serde(default)]
    pub build: BuildConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTable {
    /// Member directories, relative to the workspace root.
    pub members: Vec<String>,
    /// Paths to skip even if matched by `members`.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Subset built when no `-p` flag is given. Defaults to all members.
    #[serde(default, rename = "default-members")]
    pub default_members: Vec<String>,
    /// Shared dependency definitions inherited by members via
    /// `dep.workspace = true` (or `dep = { workspace = true, ... }`).
    /// Mirrors Cargo's `[workspace.dependencies]`.
    #[serde(default)]
    pub dependencies: BTreeMap<String, DepSpec>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageMeta {
    pub name: String,
    pub version: String,
    pub java: u32,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub main: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum DepSpec {
    Version(String),
    Detailed(DetailedDep),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DetailedDep {
    /// `version` is required for Maven Central deps; absent for path deps and
    /// for workspace-inherited deps.
    #[serde(default)]
    pub version: Option<String>,
    /// Path to a workspace member (relative to this manifest's directory).
    #[serde(default)]
    pub path: Option<String>,
    /// `workspace = true` inherits the dep from `[workspace.dependencies]`
    /// at the workspace root. Member can still override `scope`, `classifier`,
    /// `type`, `exclude`, and `optional`; setting `version` alongside is
    /// rejected at substitution time.
    #[serde(default)]
    pub workspace: bool,
    #[serde(default)]
    pub classifier: Option<String>,
    #[serde(default, rename = "type")]
    pub ty: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub optional: bool,
}

impl DepSpec {
    /// Maven version (empty string for path-only deps).
    pub fn version(&self) -> &str {
        match self {
            DepSpec::Version(v) => v,
            DepSpec::Detailed(d) => d.version.as_deref().unwrap_or(""),
        }
    }
    pub fn classifier(&self) -> Option<&str> {
        match self {
            DepSpec::Version(_) => None,
            DepSpec::Detailed(d) => d.classifier.as_deref(),
        }
    }
    pub fn ty(&self) -> &str {
        match self {
            DepSpec::Version(_) => "jar",
            DepSpec::Detailed(d) => d.ty.as_deref().unwrap_or("jar"),
        }
    }
    pub fn exclusions(&self) -> &[String] {
        match self {
            DepSpec::Version(_) => &[],
            DepSpec::Detailed(d) => &d.exclude,
        }
    }
    /// `Some(path)` for a path dependency (`{ path = "../foo" }`).
    pub fn path(&self) -> Option<&str> {
        match self {
            DepSpec::Version(_) => None,
            DepSpec::Detailed(d) => d.path.as_deref(),
        }
    }
    /// True if this dep declared `workspace = true` and needs to be substituted
    /// from `[workspace.dependencies]` at the workspace root.
    pub fn inherits_workspace(&self) -> bool {
        match self {
            DepSpec::Version(_) => false,
            DepSpec::Detailed(d) => d.workspace,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Repository {
    pub url: String,
    #[serde(default)]
    pub default: bool,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BuildConfig {
    #[serde(default, rename = "sourceDirs")]
    pub source_dirs: Option<Vec<String>>,
    #[serde(default, rename = "testDirs")]
    pub test_dirs: Option<Vec<String>>,
    #[serde(default, rename = "resourceDirs")]
    pub resource_dirs: Option<Vec<String>>,
    #[serde(default, rename = "outputDir")]
    pub output_dir: Option<String>,
    #[serde(default)]
    pub encoding: Option<String>,
    #[serde(default, rename = "javacArgs")]
    pub javac_args: Option<Vec<String>>,
}

impl Manifest {
    /// Walk up from `start` to find the nearest `jet.toml`. Returns the project
    /// root (the directory containing `jet.toml`).
    pub fn find_root(start: &Path) -> Result<PathBuf> {
        let mut cur = start.canonicalize().unwrap_or_else(|_| start.to_path_buf());
        loop {
            if cur.join(MANIFEST_FILENAME).is_file() {
                return Ok(cur);
            }
            match cur.parent() {
                Some(p) if p != cur => cur = p.to_path_buf(),
                _ => bail!(
                    "no `{MANIFEST_FILENAME}` found in `{}` or any parent directory",
                    start.display()
                ),
            }
        }
    }

    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(MANIFEST_FILENAME);
        let text = fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Self::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn from_str(text: &str) -> Result<Self> {
        let m: Manifest = toml_edit::de::from_str(text)
            .context("invalid jet.toml")?;
        m.check()?;
        Ok(m)
    }

    /// The `[package]` table. Bails on a virtual workspace manifest (which
    /// has only `[workspace]` and no package of its own).
    pub fn pkg(&self) -> Result<&PackageMeta> {
        self.package.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "this jet.toml has no [package] table — it's a virtual workspace manifest"
            )
        })
    }

    /// True when this manifest is a workspace root (has `[workspace]`).
    pub fn is_workspace_root(&self) -> bool {
        self.workspace.is_some()
    }

    fn check(&self) -> Result<()> {
        // Workspace-root virtual manifest: only [workspace] is required; no
        // [package] is fine. Member/single-package manifests must have [package].
        if let Some(pkg) = &self.package {
            validate_project_name(&pkg.name)?;
            if pkg.version.trim().is_empty() {
                bail!("[package].version cannot be empty");
            }
            if !(8..=25).contains(&pkg.java) {
                bail!(
                    "[package].java = {} is out of range (supported: 8..=25)",
                    pkg.java
                );
            }
        } else if self.workspace.is_none() {
            bail!("jet.toml must contain either [package] or [workspace]");
        }
        for (key, spec) in self.dependencies.iter().chain(self.dev_dependencies.iter()) {
            if !key.contains(':') {
                bail!(
                    "dependency key `{key}` must be `group:artifact`, e.g. \
                     `org.slf4j:slf4j-api`"
                );
            }
            // workspace-inherited deps draw their version from the workspace
            // root; member must NOT also set version. Path deps don't need a
            // version either.
            if spec.inherits_workspace() {
                if let DepSpec::Detailed(d) = spec {
                    if d.version.is_some() {
                        bail!(
                            "dependency `{key}` has `workspace = true` but also \
                             specifies `version`; one or the other"
                        );
                    }
                }
                continue;
            }
            if spec.path().is_some() {
                continue;
            }
            let v = spec.version();
            if v.trim().is_empty() {
                bail!("dependency `{key}` has empty version");
            }
            if v.contains(',') || v.starts_with('[') || v.starts_with('(') {
                bail!(
                    "dependency `{key} = \"{v}\"` looks like a Maven version range; \
                     ranges are not supported — pin to an exact version"
                );
            }
        }
        Ok(())
    }

    /// Insert or replace a dependency in `jet.toml`, preserving comments and
    /// formatting. Used by `jet add`.
    pub fn add_dependency(
        manifest_path: &Path,
        key: &str,
        version: &str,
    ) -> Result<()> {
        Self::add_dep_to_table(manifest_path, "dependencies", key, version)
    }

    /// Insert or replace a dependency under a given top-level table, preserving
    /// comments. Used by `jet add` (table = "dependencies" or "dev-dependencies").
    pub fn add_dep_to_table(
        manifest_path: &Path,
        table: &str,
        key: &str,
        version: &str,
    ) -> Result<()> {
        let text = fs::read_to_string(manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let mut doc: DocumentMut = text
            .parse()
            .with_context(|| format!("parsing {}", manifest_path.display()))?;

        let deps = doc
            .entry(table)
            .or_insert(toml_edit::Item::Table(Default::default()))
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("[{table}] is not a table"))?;
        deps.insert(key, toml_edit::value(version));

        let tmp = manifest_path.with_extension("toml.tmp");
        fs::write(&tmp, doc.to_string())
            .with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, manifest_path)
            .with_context(|| format!("renaming {} → {}", tmp.display(), manifest_path.display()))?;
        Ok(())
    }

    /// Default Java package, e.g. `com.example.my_app`. Bails on virtual manifests.
    #[allow(dead_code)]
    pub fn java_package(&self) -> Result<String> {
        let pkg = self.pkg()?;
        if let Some(p) = &pkg.package {
            return Ok(p.clone());
        }
        let group = pkg.group.as_deref().unwrap_or("com.example");
        Ok(format!("{group}.{}", crate::validate::to_java_package_segment(&pkg.name)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal() {
        let s = r#"
[package]
name = "hello"
version = "0.1.0"
java = 21
"#;
        let m = Manifest::from_str(s).unwrap();
        assert_eq!(m.pkg().unwrap().name, "hello");
        assert_eq!(m.pkg().unwrap().java, 21);
        assert!(m.dependencies.is_empty());
    }

    #[test]
    fn parse_full() {
        let s = r#"
[package]
name = "demo"
version = "0.2.0"
java = 21
group = "io.example"
main = "io.example.demo.Main"

[dependencies]
"org.slf4j:slf4j-api" = "2.0.13"
"com.google.guava:guava" = { version = "33.0.0-jre", scope = "compile" }

[dev-dependencies]
"org.junit.jupiter:junit-jupiter" = "5.10.2"
"#;
        let m = Manifest::from_str(s).unwrap();
        assert_eq!(m.dependencies.len(), 2);
        assert_eq!(m.dev_dependencies.len(), 1);
        assert_eq!(m.dependencies["org.slf4j:slf4j-api"].version(), "2.0.13");
        assert_eq!(m.dependencies["com.google.guava:guava"].version(), "33.0.0-jre");
    }

    #[test]
    fn rejects_unknown_field() {
        let s = r#"
[package]
name = "x"
version = "0.1.0"
java = 21
unknown = "boom"
"#;
        assert!(Manifest::from_str(s).is_err());
    }

    #[test]
    fn rejects_bad_java_version() {
        let s = r#"
[package]
name = "x"
version = "0.1.0"
java = 99
"#;
        assert!(Manifest::from_str(s).is_err());
    }

    #[test]
    fn rejects_dep_without_colon() {
        let s = r#"
[package]
name = "x"
version = "0.1.0"
java = 21

[dependencies]
guava = "1.0"
"#;
        assert!(Manifest::from_str(s).is_err());
    }

    #[test]
    fn rejects_version_range() {
        let s = r#"
[package]
name = "x"
version = "0.1.0"
java = 21

[dependencies]
"a:b" = "[1.0,2.0)"
"#;
        let err = Manifest::from_str(s).err().expect("should reject ranges");
        assert!(format!("{err:#}").contains("range"));
    }

    #[test]
    fn add_dependency_preserves_existing_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jet.toml");
        fs::write(
            &path,
            r#"# my project
[package]
name    = "hello"
version = "0.1.0"
java    = 21

[dependencies]
"org.slf4j:slf4j-api" = "2.0.13"
"#,
        )
        .unwrap();

        Manifest::add_dependency(&path, "com.google.guava:guava", "33.0.0-jre").unwrap();

        let txt = fs::read_to_string(&path).unwrap();
        assert!(txt.contains("# my project"), "comment preserved");
        assert!(txt.contains("\"org.slf4j:slf4j-api\" = \"2.0.13\""));
        assert!(txt.contains("\"com.google.guava:guava\" = \"33.0.0-jre\""));
    }
}
