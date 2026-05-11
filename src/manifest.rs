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
    #[serde(default)]
    pub publish: Option<PublishConfig>,
    #[serde(default)]
    pub toolchain: Option<ToolchainConfig>,
}

/// `[toolchain]` table — pin the JDK distribution used to build this project.
/// When present, jet downloads the matching JDK from the Adoptium API on the
/// first build and reuses it via `~/.jet/jdks/` thereafter.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolchainConfig {
    /// Java major version (e.g. `21`, `25`).
    pub version: u32,
    /// Distribution vendor. Defaults to `temurin` (Eclipse Adoptium).
    /// Adoptium accepts: `temurin`, `dragonwell`, `liberica`, `openj9`,
    /// `corretto`, `zulu`, etc., but jet is tested against `temurin`.
    #[serde(default = "default_vendor")]
    pub vendor: String,
}

fn default_vendor() -> String {
    "temurin".into()
}

/// `[publish]` table — describes the target Maven repository and metadata
/// embedded in the generated POM.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublishConfig {
    /// Target repository URL (Maven layout). Overridable via `JET_PUBLISH_URL`.
    /// Examples:
    /// - `https://maven.pkg.github.com/<owner>/<repo>`
    /// - `https://oss.sonatype.org/service/local/staging/deploy/maven2/`
    #[serde(default)]
    pub url: Option<String>,
    /// `<url>` of the generated POM (homepage / project page).
    #[serde(default)]
    pub homepage: Option<String>,
    /// `<scm><url>` of the generated POM (source repo URL).
    #[serde(default)]
    pub repository: Option<String>,
    /// `<scm><connection>` (e.g. `scm:git:https://github.com/...`).
    #[serde(default, rename = "scm-connection")]
    pub scm_connection: Option<String>,
    /// `<scm><developerConnection>`.
    #[serde(default, rename = "scm-developer-connection")]
    pub scm_developer_connection: Option<String>,
    /// Run `gpg --detach-sign` on every artifact before upload. Defaults
    /// to `true`. Pass `--no-sign` on the CLI to skip for internal repos.
    #[serde(default = "default_sign")]
    pub sign: bool,
    /// Specific GPG key fingerprint or user-id (forwarded to `gpg -u`).
    #[serde(default, rename = "gpg-key")]
    pub gpg_key: Option<String>,
    /// Override the URL written into POM `<licenses><license><url>`.
    #[serde(default, rename = "license-url")]
    pub license_url: Option<String>,
}

fn default_sign() -> bool {
    true
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
    /// Shared `[package]` field defaults. Members opt in per field via
    /// `field.workspace = true` (or `field = { workspace = true }`).
    /// Mirrors Cargo's `[workspace.package]`.
    #[serde(default)]
    pub package: Option<WorkspacePackage>,
}

/// Shared package metadata for inheritance. Each field is optional — only
/// fields actually defined here can be inherited via `field.workspace = true`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePackage {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub java: Option<u32>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub authors: Option<Vec<String>>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageMeta {
    pub name: String,
    pub version: String,
    pub java: u32,
    /// Schema edition (`"2026"` for jet 1.0). Pinning the edition lets future
    /// jet releases evolve the manifest format without breaking projects on
    /// older schemas. Defaults to `"2026"` when absent — pre-1.0 jet.toml
    /// files keep parsing.
    #[serde(default)]
    pub edition: Option<String>,
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

/// Manifest editions jet knows how to parse. Future versions add new
/// strings here without removing old ones.
pub const KNOWN_EDITIONS: &[&str] = &["2026"];

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
            if let Some(ed) = pkg.edition.as_deref() {
                if !KNOWN_EDITIONS.contains(&ed) {
                    bail!(
                        "[package].edition = \"{ed}\" is not known to this jet ({}). \
                         Known editions: {}.\n\
                         help: upgrade jet, or pin one of the known editions.",
                        env!("CARGO_PKG_VERSION"),
                        KNOWN_EDITIONS.join(", "),
                    );
                }
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

    /// Drop a dependency from the given top-level table, preserving everything
    /// else. Returns `Ok(true)` when the key was present and removed,
    /// `Ok(false)` when the key (or the whole table) didn't exist —
    /// callers decide whether that's an error.
    pub fn remove_dep_from_table(
        manifest_path: &Path,
        table: &str,
        key: &str,
    ) -> Result<bool> {
        let text = fs::read_to_string(manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let mut doc: DocumentMut = text
            .parse()
            .with_context(|| format!("parsing {}", manifest_path.display()))?;

        let removed = match doc.get_mut(table).and_then(|i| i.as_table_mut()) {
            Some(deps) => deps.remove(key).is_some(),
            None => false,
        };
        if !removed {
            return Ok(false);
        }

        let tmp = manifest_path.with_extension("toml.tmp");
        fs::write(&tmp, doc.to_string())
            .with_context(|| format!("writing {}", tmp.display()))?;
        fs::rename(&tmp, manifest_path)
            .with_context(|| format!("renaming {} → {}", tmp.display(), manifest_path.display()))?;
        Ok(true)
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
    fn accepts_known_edition() {
        let s = r#"
[package]
name    = "x"
version = "0.1.0"
java    = 21
edition = "2026"
"#;
        let m = Manifest::from_str(s).unwrap();
        assert_eq!(m.pkg().unwrap().edition.as_deref(), Some("2026"));
    }

    #[test]
    fn rejects_unknown_edition() {
        let s = r#"
[package]
name    = "x"
version = "0.1.0"
java    = 21
edition = "2099"
"#;
        let err = Manifest::from_str(s).err().expect("should reject");
        let msg = format!("{err:#}");
        assert!(msg.contains("edition") && msg.contains("2099"), "got: {msg}");
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
    fn remove_dep_preserves_comments_and_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jet.toml");
        fs::write(
            &path,
            r#"# project header
[package]
name    = "hello"
version = "0.1.0"
java    = 21

[dependencies]
# slf4j is required for logging
"org.slf4j:slf4j-api"   = "2.0.13"
"com.google.guava:guava" = "33.0.0-jre"
"#,
        )
        .unwrap();

        let removed = Manifest::remove_dep_from_table(
            &path,
            "dependencies",
            "com.google.guava:guava",
        )
        .unwrap();
        assert!(removed);

        let txt = fs::read_to_string(&path).unwrap();
        assert!(txt.contains("# project header"));
        assert!(txt.contains("# slf4j is required for logging"));
        assert!(txt.contains("\"org.slf4j:slf4j-api\""));
        assert!(!txt.contains("guava"));

        // A second remove on the same key returns false.
        let again = Manifest::remove_dep_from_table(
            &path,
            "dependencies",
            "com.google.guava:guava",
        )
        .unwrap();
        assert!(!again);
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
