//! Maven coordinates and Maven Central URL construction.

use std::fmt;
use std::str::FromStr;

use anyhow::{Result, bail};

/// `group:artifact:version` (with optional classifier and type).
///
/// String form: `group:artifact:version[:classifier][@type]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Coord {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub classifier: Option<String>,
    pub ty: String,
}

impl Coord {
    pub fn new(group: &str, artifact: &str, version: &str) -> Self {
        Self {
            group: group.to_string(),
            artifact: artifact.to_string(),
            version: version.to_string(),
            classifier: None,
            ty: "jar".to_string(),
        }
    }

    /// Identity used for conflict resolution: `group:artifact[:classifier][@type]`.
    /// (Version is intentionally excluded.)
    pub fn ga(&self) -> String {
        let mut s = format!("{}:{}", self.group, self.artifact);
        if let Some(c) = &self.classifier {
            s.push(':');
            s.push_str(c);
        }
        if self.ty != "jar" {
            s.push('@');
            s.push_str(&self.ty);
        }
        s
    }

    /// `group/path/artifact/version/artifact-version[-classifier].type`
    pub fn artifact_path(&self) -> String {
        let group_path = self.group.replace('.', "/");
        let mut filename = format!("{}-{}", self.artifact, self.version);
        if let Some(c) = &self.classifier {
            filename.push('-');
            filename.push_str(c);
        }
        filename.push('.');
        filename.push_str(&self.ty);
        format!(
            "{group_path}/{}/{}/{filename}",
            self.artifact, self.version
        )
    }

    /// POM path is always `.pom` regardless of artifact `type`.
    pub fn pom_path(&self) -> String {
        let group_path = self.group.replace('.', "/");
        format!(
            "{group_path}/{}/{}/{}-{}.pom",
            self.artifact, self.version, self.artifact, self.version
        )
    }

    /// Full URL to fetch the artifact from a base repository URL.
    pub fn artifact_url(&self, base: &str) -> String {
        format!("{}{}", strip_trailing_slash(base), prefix_slash(&self.artifact_path()))
    }

    pub fn pom_url(&self, base: &str) -> String {
        format!("{}{}", strip_trailing_slash(base), prefix_slash(&self.pom_path()))
    }
}

fn strip_trailing_slash(s: &str) -> &str {
    s.strip_suffix('/').unwrap_or(s)
}
fn prefix_slash(s: &str) -> String {
    if s.starts_with('/') { s.to_string() } else { format!("/{s}") }
}

impl fmt::Display for Coord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.group, self.artifact, self.version)?;
        if let Some(c) = &self.classifier {
            write!(f, ":{c}")?;
        }
        if self.ty != "jar" {
            write!(f, "@{}", self.ty)?;
        }
        Ok(())
    }
}

impl FromStr for Coord {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        let (head, ty) = match s.split_once('@') {
            Some((h, t)) => (h, Some(t)),
            None => (s, None),
        };
        let parts: Vec<&str> = head.split(':').collect();
        let (group, artifact, version, classifier) = match parts.as_slice() {
            [g, a, v] => (*g, *a, *v, None),
            [g, a, v, c] => (*g, *a, *v, Some((*c).to_string())),
            _ => bail!(
                "coordinate `{s}` must be `group:artifact:version[:classifier][@type]`"
            ),
        };
        for (label, value) in [("group", group), ("artifact", artifact), ("version", version)] {
            if value.is_empty() {
                bail!("coordinate `{s}` has empty {label}");
            }
        }
        if version.starts_with('[') || version.starts_with('(') || version.contains(',') {
            bail!(
                "coordinate `{s}` looks like a Maven version range; ranges are not \
                 supported in jet 0.2 — pin to an exact version"
            );
        }
        Ok(Coord {
            group: group.to_string(),
            artifact: artifact.to_string(),
            version: version.to_string(),
            classifier,
            ty: ty.unwrap_or("jar").to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let c: Coord = "org.slf4j:slf4j-api:2.0.13".parse().unwrap();
        assert_eq!(c.group, "org.slf4j");
        assert_eq!(c.artifact, "slf4j-api");
        assert_eq!(c.version, "2.0.13");
        assert_eq!(c.classifier, None);
        assert_eq!(c.ty, "jar");
    }

    #[test]
    fn parse_with_classifier_and_type() {
        let c: Coord = "io.netty:netty:4.1.0:linux-x86_64@jar".parse().unwrap();
        assert_eq!(c.classifier.as_deref(), Some("linux-x86_64"));
        assert_eq!(c.ty, "jar");
        let c: Coord = "org.springframework.boot:spring-boot-dependencies:3.2.0@pom".parse().unwrap();
        assert_eq!(c.ty, "pom");
    }

    #[test]
    fn rejects_bad_input() {
        assert!("nope".parse::<Coord>().is_err());
        assert!("a:b".parse::<Coord>().is_err());
        assert!(":b:c".parse::<Coord>().is_err());
        assert!("a:b:[1.0,2.0)".parse::<Coord>().is_err());
    }

    #[test]
    fn artifact_url_construction() {
        let c: Coord = "org.apache.commons:commons-lang3:3.14.0".parse().unwrap();
        let url = c.artifact_url("https://repo1.maven.org/maven2/");
        assert_eq!(
            url,
            "https://repo1.maven.org/maven2/org/apache/commons/commons-lang3/3.14.0/commons-lang3-3.14.0.jar"
        );
        let pom = c.pom_url("https://repo1.maven.org/maven2");
        assert_eq!(
            pom,
            "https://repo1.maven.org/maven2/org/apache/commons/commons-lang3/3.14.0/commons-lang3-3.14.0.pom"
        );
    }

    #[test]
    fn ga_for_conflict_resolution() {
        let a: Coord = "g:a:1.0".parse().unwrap();
        let b: Coord = "g:a:2.0".parse().unwrap();
        assert_eq!(a.ga(), b.ga());
    }

    #[test]
    fn display_round_trip() {
        for s in [
            "org.slf4j:slf4j-api:2.0.13",
            "io.netty:netty:4.1.0:linux-x86_64",
        ] {
            let c: Coord = s.parse().unwrap();
            assert_eq!(c.to_string(), s);
        }
    }
}
