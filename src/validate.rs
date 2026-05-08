use anyhow::{Result, bail};

const MAX_NAME_LEN: usize = 64;

/// Java reserved words and reserved literals (JLS §3.9, §3.10.3, §3.10.7).
/// Source: https://docs.oracle.com/javase/specs/jls/se21/html/jls-3.html
const JAVA_RESERVED: &[&str] = &[
    "abstract", "assert", "boolean", "break", "byte", "case", "catch", "char",
    "class", "const", "continue", "default", "do", "double", "else", "enum",
    "extends", "final", "finally", "float", "for", "goto", "if", "implements",
    "import", "instanceof", "int", "interface", "long", "native", "new",
    "package", "private", "protected", "public", "return", "short", "static",
    "strictfp", "super", "switch", "synchronized", "this", "throw", "throws",
    "transient", "try", "void", "volatile", "while",
    "true", "false", "null", "_",
];

/// Names that conflict with build-tool conventions or jet's own layout.
const RESERVED_DIRS: &[&str] = &[
    "target", "build", "out", "src", "test", "tests", "deps",
];

/// Windows reserved device names. Case-insensitive match.
const WINDOWS_RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul",
    "com0", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9",
    "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Validate a jet project name. The same rules apply to `jet new <path>` (where
/// the basename is the name) and to `--name` overrides.
///
/// Modelled on Cargo's `restricted_names::validate_package_name`, with Java
/// keyword rules added. See ROADMAP.md §"Open questions" for tradeoffs.
pub fn validate_project_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("project name cannot be empty");
    }
    if name.len() > MAX_NAME_LEN {
        bail!("project name `{name}` is too long (max {MAX_NAME_LEN} chars)");
    }

    let first = name.chars().next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        if first.is_ascii_digit() {
            bail!("project name `{name}` cannot start with a digit");
        }
        bail!(
            "project name `{name}` must start with an ASCII letter or `_` \
             (got `{first}`)"
        );
    }
    if name.starts_with('-') {
        bail!("project name `{name}` cannot start with `-`");
    }

    for c in name.chars() {
        if !(c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            bail!(
                "invalid character `{c}` in project name `{name}` \
                 (allowed: ASCII letters, digits, `_`, `-`)"
            );
        }
    }

    if name.contains("--") {
        bail!("project name `{name}` cannot contain `--`");
    }

    let lower = name.to_ascii_lowercase();
    if JAVA_RESERVED.contains(&lower.as_str()) {
        bail!("project name `{name}` is a Java reserved word");
    }
    if RESERVED_DIRS.contains(&lower.as_str()) {
        bail!(
            "project name `{name}` conflicts with a build-tool reserved name \
             (e.g. `target`, `src`, `build`)"
        );
    }
    if WINDOWS_RESERVED.contains(&lower.as_str()) {
        bail!("project name `{name}` is reserved on Windows");
    }

    Ok(())
}

/// Convert a project name (kebab-case allowed) into a valid Java package
/// segment. Hyphens become underscores; if the result starts with a digit or
/// is a Java keyword, a leading `_` is prepended.
pub fn to_java_package_segment(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();

    // Collapse runs of underscores so `my--app` -> `my_app` not `my__app`.
    while s.contains("__") {
        s = s.replace("__", "_");
    }
    let trimmed = s.trim_matches('_').to_string();
    let mut s = if trimmed.is_empty() { s } else { trimmed };

    if s.is_empty() {
        s.push('_');
    }
    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    if JAVA_RESERVED.contains(&s.as_str()) {
        s.push('_');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_names() {
        for name in ["hello", "my-app", "order_service", "_internal", "a", "App2"] {
            validate_project_name(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn rejects_empty_and_too_long() {
        assert!(validate_project_name("").is_err());
        let long = "a".repeat(65);
        assert!(validate_project_name(&long).is_err());
    }

    #[test]
    fn rejects_bad_first_char() {
        assert!(validate_project_name("123app").is_err());
        assert!(validate_project_name("-foo").is_err());
        assert!(validate_project_name(".foo").is_err());
    }

    #[test]
    fn rejects_bad_chars() {
        for name in ["hello world", "hello/world", "hello.world", "hello!", "héllo"] {
            assert!(
                validate_project_name(name).is_err(),
                "should reject `{name}`"
            );
        }
    }

    #[test]
    fn rejects_double_hyphen() {
        assert!(validate_project_name("foo--bar").is_err());
    }

    #[test]
    fn rejects_java_keywords() {
        for name in ["class", "int", "void", "if", "true", "null", "_"] {
            assert!(
                validate_project_name(name).is_err(),
                "should reject keyword `{name}`"
            );
        }
    }

    #[test]
    fn rejects_reserved_dirs() {
        for name in ["target", "src", "build", "out", "Test"] {
            assert!(
                validate_project_name(name).is_err(),
                "should reject reserved dir `{name}`"
            );
        }
    }

    #[test]
    fn rejects_windows_reserved() {
        for name in ["con", "CON", "nul", "com1", "LPT9"] {
            assert!(
                validate_project_name(name).is_err(),
                "should reject windows-reserved `{name}`"
            );
        }
    }

    #[test]
    fn java_package_segment_basic() {
        assert_eq!(to_java_package_segment("my-app"), "my_app");
        assert_eq!(to_java_package_segment("order_service"), "order_service");
        assert_eq!(to_java_package_segment("Hello"), "hello");
    }

    #[test]
    fn java_package_segment_collapses_underscores() {
        assert_eq!(to_java_package_segment("my--app"), "my_app");
        assert_eq!(to_java_package_segment("__foo__"), "foo");
    }

    #[test]
    fn java_package_segment_handles_digit_start() {
        assert_eq!(to_java_package_segment("123svc"), "_123svc");
    }

    #[test]
    fn java_package_segment_escapes_keyword() {
        assert_eq!(to_java_package_segment("class"), "class_");
    }
}
