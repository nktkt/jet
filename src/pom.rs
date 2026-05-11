//! Generate Maven POM XML from a jet manifest plus a resolved lockfile.

use crate::lockfile::{Lockfile, Package};
use crate::manifest::Manifest;

/// Render the POM XML for a publishable artifact.
///
/// Coordinates: `<groupId>` from `[package].group`, `<artifactId>` from
/// `[package].name`, `<version>` from `[package].version`. Always
/// `packaging=jar`.
///
/// Dependencies come from `lockfile.packages` (origin = "main", scope =
/// compile/runtime). Path-deps and dev-deps are intentionally excluded —
/// path-deps don't make sense for a published artifact, and consumers don't
/// need our test deps.
pub fn render_pom(manifest: &Manifest, lockfile: Option<&Lockfile>) -> anyhow::Result<String> {
    let pkg = manifest.pkg()?;
    let group = pkg
        .group
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("[package].group is required for `jet publish`"))?;
    let artifact = pkg.name.as_str();
    let version = pkg.version.as_str();
    let pub_cfg = manifest.publish.as_ref();

    let mut s = String::with_capacity(2048);
    s.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    s.push_str(
        "<project xmlns=\"http://maven.apache.org/POM/4.0.0\"\n         \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\n         \
         xsi:schemaLocation=\"http://maven.apache.org/POM/4.0.0 \
         https://maven.apache.org/xsd/maven-4.0.0.xsd\">\n",
    );
    s.push_str("  <modelVersion>4.0.0</modelVersion>\n");
    s.push_str(&tag("groupId", group, 1));
    s.push_str(&tag("artifactId", artifact, 1));
    s.push_str(&tag("version", version, 1));
    s.push_str(&tag("packaging", "jar", 1));
    s.push_str(&tag("name", artifact, 1));

    if let Some(desc) = pkg.description.as_deref() {
        s.push_str(&tag("description", desc, 1));
    }
    if let Some(home) = pub_cfg.and_then(|p| p.homepage.as_deref()) {
        s.push_str(&tag("url", home, 1));
    }

    // Licenses.
    if let Some(license) = pkg.license.as_deref() {
        let url = pub_cfg
            .and_then(|p| p.license_url.clone())
            .or_else(|| spdx_to_url(license).map(str::to_string));
        s.push_str("  <licenses>\n");
        s.push_str("    <license>\n");
        s.push_str(&tag("name", license, 3));
        if let Some(u) = url {
            s.push_str(&tag("url", &u, 3));
        }
        s.push_str(&tag("distribution", "repo", 3));
        s.push_str("    </license>\n");
        s.push_str("  </licenses>\n");
    }

    // Developers (from `[package].authors`).
    if !pkg.authors.is_empty() {
        s.push_str("  <developers>\n");
        for author in &pkg.authors {
            let (name, email) = parse_author(author);
            s.push_str("    <developer>\n");
            s.push_str(&tag("name", name, 3));
            if let Some(e) = email {
                s.push_str(&tag("email", e, 3));
            }
            s.push_str("    </developer>\n");
        }
        s.push_str("  </developers>\n");
    }

    // SCM.
    if let Some(p) = pub_cfg {
        if p.repository.is_some() || p.scm_connection.is_some() || p.scm_developer_connection.is_some()
        {
            s.push_str("  <scm>\n");
            if let Some(c) = p.scm_connection.as_deref() {
                s.push_str(&tag("connection", c, 2));
            }
            if let Some(c) = p.scm_developer_connection.as_deref() {
                s.push_str(&tag("developerConnection", c, 2));
            }
            if let Some(u) = p.repository.as_deref() {
                s.push_str(&tag("url", u, 2));
            }
            s.push_str("  </scm>\n");
        }
    }

    // Dependencies (from lockfile, scope=compile|runtime, origin=main).
    if let Some(lf) = lockfile {
        let mut deps: Vec<&Package> = lf
            .packages
            .iter()
            .filter(|p| p.origin == "main" && (p.scope == "compile" || p.scope == "runtime"))
            .collect();
        deps.sort_by(|a, b| (a.name.as_str(), a.version.as_str()).cmp(&(b.name.as_str(), b.version.as_str())));
        if !deps.is_empty() {
            s.push_str("  <dependencies>\n");
            for d in deps {
                let (g, a) = d.name.split_once(':').unwrap_or((&d.name, ""));
                s.push_str("    <dependency>\n");
                s.push_str(&tag("groupId", g, 3));
                s.push_str(&tag("artifactId", a, 3));
                s.push_str(&tag("version", &d.version, 3));
                if d.ty != "jar" {
                    s.push_str(&tag("type", &d.ty, 3));
                }
                if let Some(c) = d.classifier.as_deref() {
                    s.push_str(&tag("classifier", c, 3));
                }
                s.push_str(&tag("scope", &d.scope, 3));
                s.push_str("    </dependency>\n");
            }
            s.push_str("  </dependencies>\n");
        }
    }

    s.push_str("</project>\n");
    Ok(s)
}

fn tag(name: &str, value: &str, indent: usize) -> String {
    let pad = "  ".repeat(indent);
    format!("{pad}<{name}>{}</{name}>\n", xml_escape(value))
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// `Naoki <naoki@example.com>` → `("Naoki", Some("naoki@example.com"))`.
fn parse_author(s: &str) -> (&str, Option<&str>) {
    if let Some(open) = s.find('<') {
        if let Some(close) = s[open + 1..].find('>') {
            let name = s[..open].trim();
            let email = &s[open + 1..open + 1 + close];
            return (name, Some(email.trim()));
        }
    }
    (s.trim(), None)
}

/// Map common SPDX identifiers to their canonical license URL. Returns `None`
/// for unknown identifiers — POM still includes the SPDX name without URL.
fn spdx_to_url(license: &str) -> Option<&'static str> {
    let id = license.trim();
    Some(match id {
        "Apache-2.0" => "https://www.apache.org/licenses/LICENSE-2.0.txt",
        "MIT" => "https://opensource.org/licenses/MIT",
        "BSD-3-Clause" => "https://opensource.org/licenses/BSD-3-Clause",
        "BSD-2-Clause" => "https://opensource.org/licenses/BSD-2-Clause",
        "MPL-2.0" => "https://www.mozilla.org/MPL/2.0/",
        "GPL-2.0-only" | "GPL-2.0" => "https://www.gnu.org/licenses/gpl-2.0.txt",
        "GPL-3.0-only" | "GPL-3.0" => "https://www.gnu.org/licenses/gpl-3.0.txt",
        "LGPL-2.1-only" | "LGPL-2.1" => "https://www.gnu.org/licenses/lgpl-2.1.txt",
        "LGPL-3.0-only" | "LGPL-3.0" => "https://www.gnu.org/licenses/lgpl-3.0.txt",
        "AGPL-3.0-only" | "AGPL-3.0" => "https://www.gnu.org/licenses/agpl-3.0.txt",
        "ISC" => "https://opensource.org/licenses/ISC",
        "Unlicense" => "https://unlicense.org/",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lockfile::{LOCKFILE_VERSION, Package, RootPkg};
    use crate::manifest::{Manifest, PackageMeta, PublishConfig};

    fn empty_lockfile() -> Lockfile {
        Lockfile {
            version: LOCKFILE_VERSION,
            root: RootPkg { name: "x".into(), version: "0.1.0".into() },
            packages: vec![],
        }
    }

    fn manifest_with(group: &str, license: Option<&str>, authors: Vec<&str>) -> Manifest {
        Manifest {
            package: Some(PackageMeta {
                name: "demo".into(),
                version: "1.2.3".into(),
                java: 21,
                edition: None,
                group: Some(group.into()),
                package: None,
                main: None,
                license: license.map(str::to_string),
                authors: authors.into_iter().map(String::from).collect(),
                description: Some("A demo library".into()),
            }),
            workspace: None,
            dependencies: Default::default(),
            dev_dependencies: Default::default(),
            repositories: Default::default(),
            build: Default::default(),
            publish: Some(PublishConfig {
                homepage: Some("https://example.com/demo".into()),
                repository: Some("https://github.com/me/demo".into()),
                scm_connection: Some("scm:git:https://github.com/me/demo.git".into()),
                scm_developer_connection: Some("scm:git:ssh://git@github.com/me/demo.git".into()),
                ..Default::default()
            }),
            toolchain: None,
        }
    }

    #[test]
    fn renders_basic_pom() {
        let m = manifest_with("io.example", Some("Apache-2.0"), vec!["Ada <ada@example.org>"]);
        let lf = empty_lockfile();
        let xml = render_pom(&m, Some(&lf)).unwrap();
        assert!(xml.contains("<groupId>io.example</groupId>"));
        assert!(xml.contains("<artifactId>demo</artifactId>"));
        assert!(xml.contains("<version>1.2.3</version>"));
        assert!(xml.contains("<packaging>jar</packaging>"));
        assert!(xml.contains("<url>https://example.com/demo</url>"));
        assert!(xml.contains("<name>Apache-2.0</name>"));
        assert!(xml.contains("https://www.apache.org/licenses/LICENSE-2.0.txt"));
        assert!(xml.contains("<name>Ada</name>"));
        assert!(xml.contains("<email>ada@example.org</email>"));
        assert!(xml.contains("<connection>scm:git:https://github.com/me/demo.git</connection>"));
        assert!(xml.contains("<developerConnection>scm:git:ssh://"));
    }

    #[test]
    fn renders_dependencies_from_lockfile() {
        let m = manifest_with("io.example", Some("MIT"), vec![]);
        let lf = Lockfile {
            version: LOCKFILE_VERSION,
            root: RootPkg { name: "demo".into(), version: "1.2.3".into() },
            packages: vec![
                Package {
                    name: "org.slf4j:slf4j-api".into(),
                    version: "2.0.13".into(),
                    classifier: None,
                    ty: "jar".into(),
                    scope: "compile".into(),
                    origin: "main".into(),
                    url: "https://repo1.maven.org/maven2/org/slf4j/slf4j-api/2.0.13/slf4j-api-2.0.13.jar".into(),
                    sha256: None,
                },
                Package {
                    name: "org.junit.jupiter:junit-jupiter".into(),
                    version: "5.10.2".into(),
                    classifier: None,
                    ty: "jar".into(),
                    scope: "compile".into(),
                    origin: "dev".into(), // dev — must not appear
                    url: "...".into(),
                    sha256: None,
                },
            ],
        };
        let xml = render_pom(&m, Some(&lf)).unwrap();
        assert!(xml.contains("<artifactId>slf4j-api</artifactId>"));
        assert!(xml.contains("<version>2.0.13</version>"));
        assert!(!xml.contains("junit"), "dev-deps must not appear in published POM");
    }

    #[test]
    fn requires_group() {
        let mut m = manifest_with("ignored", Some("MIT"), vec![]);
        m.package.as_mut().unwrap().group = None;
        let err = render_pom(&m, None).err().expect("should fail without group");
        assert!(format!("{err:#}").contains("group"));
    }

    #[test]
    fn xml_escapes_special_chars() {
        let mut m = manifest_with("io.example", Some("MIT"), vec![]);
        m.package.as_mut().unwrap().description = Some("A & B <c> \"d\"".into());
        let xml = render_pom(&m, None).unwrap();
        assert!(xml.contains("A &amp; B &lt;c&gt; &quot;d&quot;"));
    }

    #[test]
    fn parses_author_with_email() {
        let (name, email) = parse_author("Ada Lovelace <ada@example.org>");
        assert_eq!(name, "Ada Lovelace");
        assert_eq!(email, Some("ada@example.org"));

        let (name, email) = parse_author("No Email Here");
        assert_eq!(name, "No Email Here");
        assert_eq!(email, None);
    }
}
