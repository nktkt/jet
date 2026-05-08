//! Minimal POM parser. Targets the subset jet 0.2 needs:
//! coordinates, `<parent>`, `<properties>`, `<dependencies>`, `<dependencyManagement>`,
//! per-dep `<scope>`, `<optional>`, `<classifier>`, `<type>`, `<exclusions>`.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::coord::Coord;

#[derive(Debug, Default, Clone)]
pub struct Pom {
    pub group_id: String,
    pub artifact_id: String,
    pub version: String,
    pub packaging: String,
    pub parent: Option<ParentRef>,
    pub properties: HashMap<String, String>,
    pub dependencies: Vec<DependencyDecl>,
    pub dependency_management: Vec<DependencyDecl>,
}

#[derive(Debug, Clone)]
pub struct ParentRef {
    pub group_id: String,
    pub artifact_id: String,
    pub version: String,
}

#[derive(Debug, Default, Clone)]
pub struct DependencyDecl {
    pub group_id: String,
    pub artifact_id: String,
    pub version: Option<String>,
    pub classifier: Option<String>,
    pub ty: Option<String>,
    pub scope: Option<String>,
    pub optional: bool,
    pub exclusions: Vec<Exclusion>,
}

#[derive(Debug, Clone)]
pub struct Exclusion {
    pub group_id: String,
    pub artifact_id: String,
}

impl Pom {
    pub fn coord(&self) -> Coord {
        Coord {
            group: self.group_id.clone(),
            artifact: self.artifact_id.clone(),
            version: self.version.clone(),
            classifier: None,
            ty: if self.packaging.is_empty() {
                "jar".into()
            } else {
                self.packaging.clone()
            },
        }
    }

    /// Parse a POM from XML bytes. Tolerant of whitespace and unknown tags.
    pub fn parse(xml: &[u8]) -> Result<Self> {
        let mut reader = Reader::from_reader(xml);
        reader.config_mut().trim_text(true);

        let mut pom = Pom {
            packaging: "jar".into(),
            ..Default::default()
        };
        let mut path: Vec<String> = Vec::with_capacity(8);
        let mut buf = Vec::new();

        // Stacks for current nested struct under construction.
        let mut current_dep: Option<DependencyDecl> = None;
        let mut current_excl: Option<Exclusion> = None;
        let mut current_parent: Option<ParentRef> = None;
        let mut current_prop: Option<String> = None;

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) => {
                    let name = local_name(e.name().as_ref());
                    path.push(name.clone());

                    let n = path.len();
                    let parent = if n >= 1 { &path[..n - 1] } else { &path[..] };
                    if name == "dependency"
                        && (in_path(parent, &["dependencies"])
                            || in_path(parent, &["dependencyManagement", "dependencies"]))
                    {
                        current_dep = Some(DependencyDecl::default());
                    } else if name == "exclusion" && in_path(parent, &["exclusions"]) {
                        current_excl = Some(Exclusion {
                            group_id: String::new(),
                            artifact_id: String::new(),
                        });
                    } else if name == "parent" && parent == ["project"] {
                        current_parent = Some(ParentRef {
                            group_id: String::new(),
                            artifact_id: String::new(),
                            version: String::new(),
                        });
                    } else if parent.len() == 2
                        && parent[0] == "project"
                        && parent[1] == "properties"
                    {
                        current_prop = Some(name.clone());
                    }
                }
                Ok(Event::End(e)) => {
                    let name = local_name(e.name().as_ref());
                    let popped = path.pop();
                    debug_assert_eq!(popped.as_deref(), Some(name.as_str()));

                    match name.as_str() {
                        "dependency" => {
                            if let Some(d) = current_dep.take() {
                                if in_path(&path, &["dependencyManagement", "dependencies"]) {
                                    pom.dependency_management.push(d);
                                } else if in_path(&path, &["dependencies"]) {
                                    pom.dependencies.push(d);
                                }
                            }
                        }
                        "exclusion" => {
                            if let (Some(d), Some(e)) =
                                (current_dep.as_mut(), current_excl.take())
                            {
                                d.exclusions.push(e);
                            }
                        }
                        "parent" if path.as_slice() == ["project"] => {
                            if let Some(p) = current_parent.take() {
                                if !p.group_id.is_empty() && !p.artifact_id.is_empty() {
                                    pom.parent = Some(p);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Event::Text(t)) => {
                    let txt = t.unescape().unwrap_or_default().into_owned();
                    apply_text(
                        &path,
                        &txt,
                        &mut pom,
                        current_dep.as_mut(),
                        current_excl.as_mut(),
                        current_parent.as_mut(),
                        current_prop.as_deref(),
                    );
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(e) => bail!("xml error at offset {}: {e}", reader.buffer_position()),
            }
            buf.clear();
        }

        // Inherit groupId/version from parent if missing on the project.
        if pom.group_id.is_empty() {
            if let Some(p) = &pom.parent {
                pom.group_id = p.group_id.clone();
            }
        }
        if pom.version.is_empty() {
            if let Some(p) = &pom.parent {
                pom.version = p.version.clone();
            }
        }

        if pom.artifact_id.is_empty() {
            bail!("POM missing <artifactId>");
        }
        Ok(pom)
    }

    /// Interpolate `${...}` placeholders against this POM's properties +
    /// `project.*` model values + an optional ancestor map.
    pub fn interpolate(&self, s: &str, ancestors: &HashMap<String, String>) -> String {
        let mut out = String::with_capacity(s.len());
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if i + 1 < bytes.len() && bytes[i] == b'$' && bytes[i + 1] == b'{' {
                if let Some(end) = s[i + 2..].find('}') {
                    let key = &s[i + 2..i + 2 + end];
                    let value = self.lookup_prop(key, ancestors);
                    out.push_str(&value.unwrap_or_else(|| s[i..i + 2 + end + 1].to_string()));
                    i += 2 + end + 1;
                    continue;
                }
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    fn lookup_prop(&self, key: &str, ancestors: &HashMap<String, String>) -> Option<String> {
        match key {
            "project.groupId" | "pom.groupId" => Some(self.group_id.clone()),
            "project.artifactId" | "pom.artifactId" => Some(self.artifact_id.clone()),
            "project.version" | "pom.version" | "version" => Some(self.version.clone()),
            "project.parent.version" => {
                self.parent.as_ref().map(|p| p.version.clone())
            }
            "project.parent.groupId" => {
                self.parent.as_ref().map(|p| p.group_id.clone())
            }
            other => self
                .properties
                .get(other)
                .cloned()
                .or_else(|| ancestors.get(other).cloned()),
        }
    }
}

fn local_name(qn: &[u8]) -> String {
    let s = std::str::from_utf8(qn).unwrap_or("");
    match s.split_once(':') {
        Some((_, local)) => local.to_string(),
        None => s.to_string(),
    }
}

fn in_path(path: &[String], suffix: &[&str]) -> bool {
    if suffix.len() > path.len() {
        return false;
    }
    let start = path.len() - suffix.len();
    path[start..].iter().zip(suffix.iter()).all(|(a, b)| a == b)
}

fn apply_text(
    path: &[String],
    txt: &str,
    pom: &mut Pom,
    cur_dep: Option<&mut DependencyDecl>,
    cur_excl: Option<&mut Exclusion>,
    cur_parent: Option<&mut ParentRef>,
    cur_prop: Option<&str>,
) {
    if let Some(prop_name) = cur_prop {
        if path.last().map(String::as_str) == Some(prop_name) {
            pom.properties.insert(prop_name.to_string(), txt.to_string());
            return;
        }
    }
    if let Some(parent) = cur_parent {
        if path.starts_with(&["project".to_string(), "parent".to_string()]) {
            match path.last().map(String::as_str) {
                Some("groupId") => parent.group_id = txt.to_string(),
                Some("artifactId") => parent.artifact_id = txt.to_string(),
                Some("version") => parent.version = txt.to_string(),
                _ => {}
            }
            return;
        }
    }
    if let Some(excl) = cur_excl {
        match path.last().map(String::as_str) {
            Some("groupId") => excl.group_id = txt.to_string(),
            Some("artifactId") => excl.artifact_id = txt.to_string(),
            _ => {}
        }
        return;
    }
    if let Some(dep) = cur_dep {
        match path.last().map(String::as_str) {
            Some("groupId") => dep.group_id = txt.to_string(),
            Some("artifactId") => dep.artifact_id = txt.to_string(),
            Some("version") => dep.version = Some(txt.to_string()),
            Some("classifier") => dep.classifier = Some(txt.to_string()),
            Some("type") => dep.ty = Some(txt.to_string()),
            Some("scope") => dep.scope = Some(txt.to_string()),
            Some("optional") => dep.optional = txt.eq_ignore_ascii_case("true"),
            _ => {}
        }
        return;
    }
    // Top-level project metadata.
    if path.first().map(String::as_str) == Some("project") {
        match (path.len(), path.last().map(String::as_str)) {
            (2, Some("groupId")) => pom.group_id = txt.to_string(),
            (2, Some("artifactId")) => pom.artifact_id = txt.to_string(),
            (2, Some("version")) => pom.version = txt.to_string(),
            (2, Some("packaging")) => pom.packaging = txt.to_string(),
            _ => {}
        }
    }
}

pub fn fetch_and_parse(coord: &Coord, fetcher: &crate::resolver::Fetcher) -> Result<Pom> {
    let bytes = fetcher
        .fetch_pom(coord)
        .with_context(|| format!("fetching POM for {coord}"))?;
    Pom::parse(&bytes).with_context(|| format!("parsing POM for {coord}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUAVA_POM: &str = r#"<?xml version="1.0"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.google.guava</groupId>
  <artifactId>guava</artifactId>
  <version>33.0.0-jre</version>
  <packaging>bundle</packaging>
  <properties>
    <jsr305.version>3.0.2</jsr305.version>
  </properties>
  <dependencies>
    <dependency>
      <groupId>com.google.code.findbugs</groupId>
      <artifactId>jsr305</artifactId>
      <version>${jsr305.version}</version>
    </dependency>
    <dependency>
      <groupId>org.checkerframework</groupId>
      <artifactId>checker-qual</artifactId>
      <version>3.41.0</version>
      <scope>compile</scope>
    </dependency>
  </dependencies>
</project>
"#;

    #[test]
    fn parses_basic_pom() {
        let p = Pom::parse(GUAVA_POM.as_bytes()).unwrap();
        assert_eq!(p.group_id, "com.google.guava");
        assert_eq!(p.artifact_id, "guava");
        assert_eq!(p.version, "33.0.0-jre");
        assert_eq!(p.packaging, "bundle");
        assert_eq!(p.dependencies.len(), 2);
        assert_eq!(p.dependencies[0].group_id, "com.google.code.findbugs");
        assert_eq!(p.dependencies[0].version.as_deref(), Some("${jsr305.version}"));
        assert_eq!(p.dependencies[1].scope.as_deref(), Some("compile"));
    }

    #[test]
    fn property_interpolation() {
        let p = Pom::parse(GUAVA_POM.as_bytes()).unwrap();
        let raw = p.dependencies[0].version.clone().unwrap();
        let resolved = p.interpolate(&raw, &HashMap::new());
        assert_eq!(resolved, "3.0.2");
        assert_eq!(p.interpolate("${project.version}", &HashMap::new()), "33.0.0-jre");
    }

    #[test]
    fn parses_parent_and_dep_mgmt_with_exclusions() {
        let xml = r#"<project>
  <parent>
    <groupId>com.example</groupId>
    <artifactId>example-parent</artifactId>
    <version>1.0</version>
  </parent>
  <artifactId>child</artifactId>
  <dependencyManagement>
    <dependencies>
      <dependency>
        <groupId>g</groupId>
        <artifactId>a</artifactId>
        <version>2.0</version>
        <type>pom</type>
        <scope>import</scope>
      </dependency>
    </dependencies>
  </dependencyManagement>
  <dependencies>
    <dependency>
      <groupId>p</groupId>
      <artifactId>q</artifactId>
      <version>1.0</version>
      <exclusions>
        <exclusion>
          <groupId>x</groupId>
          <artifactId>y</artifactId>
        </exclusion>
      </exclusions>
    </dependency>
  </dependencies>
</project>"#;
        let p = Pom::parse(xml.as_bytes()).unwrap();
        // Inherit groupId+version from parent.
        assert_eq!(p.group_id, "com.example");
        assert_eq!(p.version, "1.0");
        assert_eq!(p.parent.as_ref().unwrap().group_id, "com.example");
        assert_eq!(p.dependency_management.len(), 1);
        assert_eq!(p.dependency_management[0].scope.as_deref(), Some("import"));
        assert_eq!(p.dependency_management[0].ty.as_deref(), Some("pom"));
        assert_eq!(p.dependencies.len(), 1);
        assert_eq!(p.dependencies[0].exclusions.len(), 1);
        assert_eq!(p.dependencies[0].exclusions[0].group_id, "x");
    }

    #[test]
    fn handles_namespaced_xml() {
        let xml = r#"<project xmlns="http://maven.apache.org/POM/4.0.0"
                              xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <groupId>g</groupId>
  <artifactId>a</artifactId>
  <version>1.0</version>
</project>"#;
        let p = Pom::parse(xml.as_bytes()).unwrap();
        assert_eq!(p.coord().to_string(), "g:a:1.0");
    }
}
