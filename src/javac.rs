//! JDK toolchain location and `javac`/`java` invocation.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

/// macOS `/usr/libexec/java_home` is the most reliable JDK locator on Mac.
#[cfg(target_os = "macos")]
const MAC_JAVA_HOME_TOOL: &str = "/usr/libexec/java_home";

#[cfg(windows)]
const EXE: &str = ".exe";
#[cfg(not(windows))]
const EXE: &str = "";

fn tool_name(name: &str) -> String {
    format!("{name}{EXE}")
}

/// Locate the `javac` binary, trying (in order) JAVA_HOME, then PATH, then
/// macOS java_home. Returns the absolute path on success.
pub fn find_javac() -> Result<PathBuf> {
    find_tool("javac")
}

/// Locate the `java` binary, same fallback chain as [`find_javac`].
pub fn find_java() -> Result<PathBuf> {
    find_tool("java")
}

fn find_tool(name: &str) -> Result<PathBuf> {
    if let Ok(home) = env::var("JAVA_HOME") {
        let candidate = Path::new(&home).join("bin").join(tool_name(name));
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    if let Ok(p) = which::which(name) {
        return Ok(p);
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = Command::new(MAC_JAVA_HOME_TOOL).output() {
            if out.status.success() {
                let home = String::from_utf8_lossy(&out.stdout).trim().to_string();
                let candidate = PathBuf::from(home).join("bin").join(tool_name(name));
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }

    let env_state = match env::var("JAVA_HOME") {
        Ok(v) => format!("JAVA_HOME={v}"),
        Err(_) => "JAVA_HOME=(unset)".to_string(),
    };
    bail!(
        "`{name}` not found.

jet needs a JDK (8+) to compile/run Java sources.

how to fix:
  - macOS/Linux:  install via SDKMAN  → https://sdkman.io/install
                  then: sdk install java 21-tem
  - any platform: download Adoptium    → https://adoptium.net/
  - already installed? ensure $JAVA_HOME/bin is on PATH:
                  export PATH=\"$JAVA_HOME/bin:$PATH\"

detected: {env_state}
          PATH contains no `{name}`"
    )
}

/// Invoke `javac` with the given source files, classpath, and output dir.
/// Returns Ok(()) on a clean compile; Err with a concise message otherwise
/// (javac's own diagnostics already streamed to the user's stderr via inherit).
pub struct CompileSpec<'a> {
    pub javac: &'a Path,
    pub release: u32,
    pub classpath: &'a [PathBuf],
    pub output_dir: &'a Path,
    pub sources: &'a [PathBuf],
    pub encoding: &'a str,
    pub extra_args: &'a [String],
}

pub fn compile(spec: CompileSpec<'_>) -> Result<()> {
    if spec.sources.is_empty() {
        bail!("no Java sources to compile");
    }
    std::fs::create_dir_all(spec.output_dir).with_context(|| {
        format!("creating {}", spec.output_dir.display())
    })?;

    let cp = join_classpath(spec.classpath);
    let mut cmd = Command::new(spec.javac);
    cmd.arg("--release").arg(spec.release.to_string());
    cmd.arg("-encoding").arg(spec.encoding);
    cmd.arg("-d").arg(spec.output_dir);
    if !cp.is_empty() {
        cmd.arg("-cp").arg(&cp);
    }
    for a in spec.extra_args {
        cmd.arg(a);
    }
    for s in spec.sources {
        cmd.arg(s);
    }

    let status = cmd
        .status()
        .with_context(|| format!("spawning `{}`", spec.javac.display()))?;
    if !status.success() {
        bail!(
            "compilation failed (javac exited with {})",
            status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into())
        );
    }
    Ok(())
}

/// Join classpath entries with the platform separator (`:` on Unix, `;` on Windows).
pub fn join_classpath(paths: &[PathBuf]) -> String {
    let sep = if cfg!(windows) { ";" } else { ":" };
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(sep)
}
