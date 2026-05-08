//! Static project templates. Embedded at compile time via `include_str!`.

use crate::validate::to_java_package_segment;

const MAIN_JAVA_TMPL: &str = include_str!("templates/Main.java.tmpl");
const GITIGNORE: &str = include_str!("templates/gitignore");

/// Group used for the default Java package when the user does not supply one.
const DEFAULT_GROUP: &str = "com.example";

/// Build the fully-qualified Java package for a project from its name,
/// e.g. `my-app` -> `com.example.my_app`.
pub fn default_java_package(name: &str) -> String {
    format!("{DEFAULT_GROUP}.{}", to_java_package_segment(name))
}

/// Render the `jet.toml` manifest for a freshly-scaffolded project.
pub fn render_manifest(name: &str, version: &str, java: u32) -> String {
    format!(
        "[package]\n\
         name    = \"{name}\"\n\
         version = \"{version}\"\n\
         java    = {java}\n\
         \n\
         [dependencies]\n"
    )
}

/// Render `Main.java` with the given fully-qualified package.
pub fn render_main_java(package: &str) -> String {
    MAIN_JAVA_TMPL.replace("{{package}}", package)
}

/// Return the canonical `.gitignore` content. No substitutions.
pub fn render_gitignore() -> &'static str {
    GITIGNORE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_uses_supplied_values() {
        let m = render_manifest("hello", "0.1.0", 21);
        assert!(m.contains("name    = \"hello\""));
        assert!(m.contains("version = \"0.1.0\""));
        assert!(m.contains("java    = 21"));
        assert!(m.contains("[dependencies]"));
    }

    #[test]
    fn main_java_substitutes_package() {
        let s = render_main_java("com.example.hello");
        assert!(s.contains("package com.example.hello;"));
        assert!(!s.contains("{{package}}"));
    }

    #[test]
    fn default_java_package_converts_hyphens() {
        assert_eq!(default_java_package("my-app"), "com.example.my_app");
        assert_eq!(default_java_package("hello"), "com.example.hello");
    }

    #[test]
    fn gitignore_includes_target() {
        let g = render_gitignore();
        assert!(g.contains("target/"));
        assert!(g.contains("!jet.lock"));
    }
}
