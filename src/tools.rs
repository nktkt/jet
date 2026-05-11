//! Cache of external tool binaries jet downloads on-demand. Lives at
//! `~/.jet/tools/`. Currently hosts only the `google-java-format` all-deps
//! JAR; future entries (Checkstyle, SpotBugs, JMH runner) can land here
//! with the same atomic download + sha-friendly layout.

use std::fs;
use std::io::Read as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};

/// Pinned `google-java-format` release. Bumped manually — each upgrade
/// involves visually diffing a sample of formatted output, since GJF
/// can change its mind about line breaks between minor versions.
pub const GJF_VERSION: &str = "1.35.0";

/// `~/.jet/tools/` (or the env override).
pub fn tools_dir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("JET_TOOLS_DIR") {
        return Ok(PathBuf::from(p));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home dir"))?;
    Ok(home.join(".jet").join("tools"))
}

pub fn gjf_jar_filename() -> String {
    format!("google-java-format-{GJF_VERSION}-all-deps.jar")
}

pub fn gjf_jar_path() -> Result<PathBuf> {
    Ok(tools_dir()?.join(gjf_jar_filename()))
}

/// Ensure the google-java-format JAR is on disk; download it from GitHub
/// releases if not. Returns the local path.
pub fn ensure_gjf() -> Result<PathBuf> {
    let path = gjf_jar_path()?;
    if path.is_file() {
        return Ok(path);
    }
    let dir = tools_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;

    let url = format!(
        "https://github.com/google/google-java-format/releases/download/v{GJF_VERSION}/{}",
        gjf_jar_filename(),
    );
    eprintln!("  Downloading google-java-format {GJF_VERSION}…");

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(120))
        .user_agent(concat!("jet/", env!("CARGO_PKG_VERSION")))
        .redirects(10)
        .build();

    let resp = agent.get(&url).call()
        .with_context(|| format!("GET {url}"))?;
    let mut bytes: Vec<u8> = Vec::with_capacity(8 << 20);
    resp.into_reader().read_to_end(&mut bytes)
        .with_context(|| format!("reading {url}"))?;

    if !is_zip(&bytes) {
        bail!(
            "downloaded {} is not a valid JAR/ZIP (got {} bytes, first 4: {:02X?}) — \
             the GitHub release may have changed; try again or report this.",
            url,
            bytes.len(),
            &bytes[..bytes.len().min(4)],
        );
    }

    let tmp = path.with_extension("jar.tmp");
    fs::write(&tmp, &bytes)
        .with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, &path)
        .with_context(|| format!("renaming {} → {}", tmp.display(), path.display()))?;
    Ok(path)
}

/// Cheap sanity check that a downloaded blob is at least a valid ZIP.
fn is_zip(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && &bytes[..4] == b"PK\x03\x04"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gjf_filename_matches_pin() {
        let f = gjf_jar_filename();
        assert!(f.contains(GJF_VERSION));
        assert!(f.starts_with("google-java-format-"));
        assert!(f.ends_with("-all-deps.jar"));
    }

    #[test]
    fn detects_non_zip_blob() {
        assert!(!is_zip(b""));
        assert!(!is_zip(b"<html>"));
        assert!(is_zip(b"PK\x03\x04\x00"));
    }
}
