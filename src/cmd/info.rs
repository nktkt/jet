//! `jet info <coord>` — show the human-readable metadata Maven Central has
//! about a coordinate: latest version, display name, description, license,
//! project URL, SCM, and direct dependencies.
//!
//! Coord may be `group:artifact` (resolves to the latest stable) or
//! `group:artifact:version` (uses exactly that). Reuses the existing
//! `Fetcher` for the POM download (so the local Maven cache is shared
//! with builds) and the v1.5 `registry::latest_version` for version
//! resolution.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use quick_xml::Reader;
use quick_xml::events::Event;

use crate::coord::Coord;
use crate::registry;
use crate::resolver::{Fetcher, default_repos};

pub struct InfoArgs {
    /// `group:artifact` (latest stable) or `group:artifact:version`.
    pub coord: String,
}

pub fn cmd_info(args: InfoArgs) -> Result<()> {
    let (group, artifact, requested_version) = parse_coord(&args.coord)?;

    let version = match requested_version {
        Some(v) => v,
        None => match registry::latest_version(&group, &artifact, false)? {
            Some(v) => v,
            None => bail!(
                "`{group}:{artifact}` not found on Maven Central \
                 (or no stable version exists — try `{group}:{artifact}:<version>`)"
            ),
        },
    };

    let coord = Coord {
        group: group.clone(),
        artifact: artifact.clone(),
        version: version.clone(),
        classifier: None,
        ty: "jar".into(),
    };
    let fetcher = Fetcher::new(default_repos())?;
    let bytes = fetcher
        .fetch_pom(&coord)
        .with_context(|| format!("fetching POM for {coord}"))?;
    let info = parse_info(&bytes)
        .with_context(|| format!("parsing POM for {coord}"))?;

    print_info(&group, &artifact, &version, &info);
    Ok(())
}

#[derive(Default)]
struct Info {
    name: Option<String>,
    description: Option<String>,
    url: Option<String>,
    licenses: Vec<License>,
    scm_url: Option<String>,
    scm_connection: Option<String>,
    deps: Vec<DepRow>,
}

struct License {
    name: Option<String>,
    url: Option<String>,
}

struct DepRow {
    group: String,
    artifact: String,
    version: Option<String>,
    scope: Option<String>,
}

fn parse_coord(s: &str) -> Result<(String, String, Option<String>)> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.as_slice() {
        [g, a] => {
            if g.is_empty() || a.is_empty() {
                bail!("coord `{s}` has an empty group or artifact");
            }
            Ok((g.to_string(), a.to_string(), None))
        }
        [g, a, v] => {
            if g.is_empty() || a.is_empty() || v.is_empty() {
                bail!("coord `{s}` has an empty segment");
            }
            Ok((g.to_string(), a.to_string(), Some(v.to_string())))
        }
        _ => bail!("coord `{s}` must be `group:artifact` or `group:artifact:version`"),
    }
}

fn print_info(group: &str, artifact: &str, version: &str, info: &Info) {
    let header = info
        .name
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(artifact);
    println!("{header}  ({group}:{artifact}:{version})");
    if let Some(desc) = info.description.as_deref().filter(|s| !s.is_empty()) {
        let trimmed = desc.split_whitespace().collect::<Vec<_>>().join(" ");
        println!("  {trimmed}");
    }
    println!();

    if let Some(url) = info.url.as_deref().filter(|s| !s.is_empty()) {
        println!("  homepage:  {url}");
    }
    for lic in &info.licenses {
        let n = lic.name.as_deref().unwrap_or("(unnamed)");
        match lic.url.as_deref() {
            Some(u) if !u.is_empty() => println!("  license:   {n}  ({u})"),
            _ => println!("  license:   {n}"),
        }
    }
    if let Some(u) = info.scm_url.as_deref().filter(|s| !s.is_empty()) {
        println!("  scm:       {u}");
    }
    if let Some(c) = info
        .scm_connection
        .as_deref()
        .filter(|s| !s.is_empty())
        .filter(|c| Some(*c) != info.scm_url.as_deref())
    {
        println!("  scm-conn:  {c}");
    }
    if info.deps.is_empty() {
        println!("  deps:      (none declared)");
    } else {
        let compile_runtime: Vec<&DepRow> = info
            .deps
            .iter()
            .filter(|d| {
                let s = d.scope.as_deref().unwrap_or("compile");
                s == "compile" || s == "runtime"
            })
            .collect();
        println!("  deps:      {} declared ({} compile/runtime)",
            info.deps.len(),
            compile_runtime.len(),
        );
        // Keep output bounded — show the first ~15 to avoid drowning
        // the screen on POMs with sprawling dependency sections.
        const MAX_DEPS_TO_SHOW: usize = 15;
        for d in info.deps.iter().take(MAX_DEPS_TO_SHOW) {
            let v = d.version.as_deref().unwrap_or("(inherited)");
            let scope_tag = match d.scope.as_deref() {
                Some(s) if s != "compile" => format!("  [{s}]"),
                _ => String::new(),
            };
            println!("    {}:{}  {}{}", d.group, d.artifact, v, scope_tag);
        }
        if info.deps.len() > MAX_DEPS_TO_SHOW {
            println!("    … and {} more", info.deps.len() - MAX_DEPS_TO_SHOW);
        }
    }
    println!();
    println!("  add: jet add {group}:{artifact}:{version}");
}

fn parse_info(xml: &[u8]) -> Result<Info> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let mut info = Info::default();
    let mut path: Vec<String> = Vec::with_capacity(8);
    let mut buf = Vec::new();

    let mut current_license: Option<License> = None;
    let mut current_dep: Option<DepRow> = None;
    // Capture text per-element so we can route it based on the path stack.
    let mut text_buf = String::new();
    // Project-level <properties> for cheap interpolation of `${...}` in
    // dependency versions (the resolver does heavyweight property/parent
    // resolution; for `jet info` we just want a readable display).
    let mut properties: HashMap<String, String> = HashMap::new();
    let mut current_prop_key: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = local_name(e.name().as_ref());
                path.push(name.clone());

                let n = path.len();
                let parent = if n >= 2 { path[n - 2].as_str() } else { "" };
                let grandparent = if n >= 3 { path[n - 3].as_str() } else { "" };

                if path == ["project", "licenses", "license"] {
                    current_license = Some(License { name: None, url: None });
                }
                if parent == "dependencies" && name == "dependency"
                    && grandparent == "project"
                {
                    current_dep = Some(DepRow {
                        group: String::new(),
                        artifact: String::new(),
                        version: None,
                        scope: None,
                    });
                }
                if path.len() == 3 && path[0] == "project" && path[1] == "properties" {
                    current_prop_key = Some(name.clone());
                }
                text_buf.clear();
            }
            Ok(Event::Text(t)) => {
                text_buf.push_str(&t.unescape().unwrap_or_default());
            }
            Ok(Event::CData(t)) => {
                text_buf.push_str(&String::from_utf8_lossy(t.as_ref()));
            }
            Ok(Event::End(_e)) => {
                let n = path.len();
                let name = path.last().cloned().unwrap_or_default();
                let parent = if n >= 2 { path[n - 2].as_str() } else { "" };
                let grandparent = if n >= 3 { path[n - 3].as_str() } else { "" };

                // Project-level scalar fields.
                if n == 2 && path[0] == "project" {
                    match name.as_str() {
                        "name" => info.name = Some(text_buf.trim().to_string()),
                        "description" => info.description = Some(text_buf.trim().to_string()),
                        "url" => info.url = Some(text_buf.trim().to_string()),
                        _ => {}
                    }
                }
                // Property kv.
                if let Some(key) = current_prop_key.take() {
                    properties.insert(key, text_buf.trim().to_string());
                }
                // SCM.
                if n == 3 && path[0] == "project" && parent == "scm" {
                    match name.as_str() {
                        "url" => info.scm_url = Some(text_buf.trim().to_string()),
                        "connection" => {
                            info.scm_connection = Some(text_buf.trim().to_string())
                        }
                        _ => {}
                    }
                }
                // License children.
                if path == ["project", "licenses", "license", "name"] {
                    if let Some(l) = current_license.as_mut() {
                        l.name = Some(text_buf.trim().to_string());
                    }
                }
                if path == ["project", "licenses", "license", "url"] {
                    if let Some(l) = current_license.as_mut() {
                        l.url = Some(text_buf.trim().to_string());
                    }
                }
                if path == ["project", "licenses", "license"] {
                    if let Some(l) = current_license.take() {
                        info.licenses.push(l);
                    }
                }
                // Dependency children — only project-level <dependencies>.
                if grandparent == "dependencies" && parent == "dependency" {
                    if let Some(d) = current_dep.as_mut() {
                        match name.as_str() {
                            "groupId" => d.group = text_buf.trim().to_string(),
                            "artifactId" => d.artifact = text_buf.trim().to_string(),
                            "version" => d.version = Some(text_buf.trim().to_string()),
                            "scope" => d.scope = Some(text_buf.trim().to_string()),
                            _ => {}
                        }
                    }
                }
                if parent == "dependencies" && name == "dependency" && grandparent == "project" {
                    if let Some(d) = current_dep.take() {
                        if !d.group.is_empty() && !d.artifact.is_empty() {
                            info.deps.push(d);
                        }
                    }
                }
                path.pop();
            }
            Ok(Event::Empty(e)) => {
                // self-closing tags don't contribute text we care about
                let _ = e;
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => bail!("xml parse error: {e}"),
        }
        buf.clear();
    }

    // Interpolate ${prop} in dependency versions for display.
    for d in info.deps.iter_mut() {
        if let Some(v) = d.version.as_ref() {
            if let Some(expanded) = expand_props(v, &properties) {
                d.version = Some(expanded);
            }
        }
    }

    Ok(info)
}

fn expand_props(s: &str, props: &HashMap<String, String>) -> Option<String> {
    if !s.contains("${") {
        return None;
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        if let Some(end) = after.find('}') {
            let key = &after[..end];
            match props.get(key) {
                Some(v) => out.push_str(v),
                None => {
                    out.push_str("${");
                    out.push_str(key);
                    out.push('}');
                }
            }
            rest = &after[end + 1..];
        } else {
            out.push_str("${");
            out.push_str(after);
            break;
        }
    }
    out.push_str(rest);
    Some(out)
}

fn local_name(qname: &[u8]) -> String {
    let s = std::str::from_utf8(qname).unwrap_or("");
    match s.rfind(':') {
        Some(i) => s[i + 1..].to_string(),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_POM: &str = r#"<?xml version="1.0"?>
<project>
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.example</groupId>
  <artifactId>thing</artifactId>
  <version>1.2.3</version>
  <name>Thing</name>
  <description>
    A thing that
    does things.
  </description>
  <url>https://example.com/thing</url>
  <licenses>
    <license>
      <name>Apache-2.0</name>
      <url>https://www.apache.org/licenses/LICENSE-2.0</url>
    </license>
  </licenses>
  <scm>
    <url>https://github.com/example/thing</url>
    <connection>scm:git:https://github.com/example/thing.git</connection>
  </scm>
  <properties>
    <slf4j.version>2.0.13</slf4j.version>
  </properties>
  <dependencies>
    <dependency>
      <groupId>org.slf4j</groupId>
      <artifactId>slf4j-api</artifactId>
      <version>${slf4j.version}</version>
    </dependency>
    <dependency>
      <groupId>org.junit.jupiter</groupId>
      <artifactId>junit-jupiter</artifactId>
      <version>5.10.2</version>
      <scope>test</scope>
    </dependency>
  </dependencies>
</project>
"#;

    #[test]
    fn parses_full_pom() {
        let info = parse_info(SAMPLE_POM.as_bytes()).unwrap();
        assert_eq!(info.name.as_deref(), Some("Thing"));
        assert!(info.description.as_deref().unwrap().contains("A thing"));
        assert_eq!(info.url.as_deref(), Some("https://example.com/thing"));
        assert_eq!(info.licenses.len(), 1);
        assert_eq!(info.licenses[0].name.as_deref(), Some("Apache-2.0"));
        assert_eq!(info.scm_url.as_deref(), Some("https://github.com/example/thing"));
        assert_eq!(info.deps.len(), 2);
        // Property interpolation happened for display.
        assert_eq!(info.deps[0].version.as_deref(), Some("2.0.13"));
        assert_eq!(info.deps[1].scope.as_deref(), Some("test"));
    }

    #[test]
    fn parse_coord_variants() {
        assert_eq!(
            parse_coord("a.b:c").unwrap(),
            ("a.b".into(), "c".into(), None),
        );
        assert_eq!(
            parse_coord("a.b:c:1.0").unwrap(),
            ("a.b".into(), "c".into(), Some("1.0".into())),
        );
        assert!(parse_coord("a.b").is_err());
        assert!(parse_coord(":c:1").is_err());
    }
}
