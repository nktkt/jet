//! HTTP fetcher with on-disk cache for POMs and JARs.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::coord::Coord;

/// Default Maven repositories tried in order.
pub fn default_repos() -> Vec<String> {
    vec!["https://repo1.maven.org/maven2".to_string()]
}

pub struct Fetcher {
    repos: Vec<String>,
    cache_dir: PathBuf,
    agent: ureq::Agent,
}

impl Fetcher {
    pub fn new(repos: Vec<String>) -> Result<Self> {
        let cache_dir = jet_cache_dir()?;
        fs::create_dir_all(&cache_dir)
            .with_context(|| format!("creating {}", cache_dir.display()))?;
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout_read(Duration::from_secs(60))
            .user_agent(concat!("jet/", env!("CARGO_PKG_VERSION")))
            .build();
        Ok(Self { repos, cache_dir, agent })
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Fetch a POM by coordinate. Returns the raw XML bytes. Cached on disk.
    pub fn fetch_pom(&self, coord: &Coord) -> Result<Vec<u8>> {
        let rel = coord.pom_path();
        let cached = self.cache_dir.join(&rel);
        if let Ok(bytes) = fs::read(&cached) {
            return Ok(bytes);
        }
        let bytes = self.fetch_path(&rel)?;
        atomic_write(&cached, &bytes)?;
        Ok(bytes)
    }

    /// Fetch the artifact JAR (or other type). Returns the local path within
    /// the cache. Verifies sha256 if `expected_sha256` is `Some`.
    pub fn fetch_artifact(
        &self,
        coord: &Coord,
        expected_sha256: Option<&str>,
    ) -> Result<PathBuf> {
        let rel = coord.artifact_path();
        let cached = self.cache_dir.join(&rel);
        if cached.is_file() {
            if let Some(expected) = expected_sha256 {
                let actual = sha256_of_file(&cached)?;
                if actual == expected {
                    return Ok(cached);
                }
                // Hash mismatch — refetch.
                let _ = fs::remove_file(&cached);
            } else {
                return Ok(cached);
            }
        }
        let bytes = self.fetch_path(&rel)?;
        if let Some(expected) = expected_sha256 {
            let actual = sha256_of_bytes(&bytes);
            if actual != expected {
                bail!(
                    "sha256 mismatch for {coord}: expected {expected}, got {actual}"
                );
            }
        }
        atomic_write(&cached, &bytes)?;
        Ok(cached)
    }

    /// Fetch by relative path against each repo in turn. Returns body bytes.
    fn fetch_path(&self, rel: &str) -> Result<Vec<u8>> {
        let mut last_err: Option<anyhow::Error> = None;
        for base in &self.repos {
            let url = format!("{}/{}", base.trim_end_matches('/'), rel.trim_start_matches('/'));
            match self.get_url(&url) {
                Ok(b) => return Ok(b),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no repositories configured")))
    }

    fn get_url(&self, url: &str) -> Result<Vec<u8>> {
        match self.agent.get(url).call() {
            Ok(resp) => {
                let mut buf = Vec::new();
                resp.into_reader()
                    .read_to_end(&mut buf)
                    .with_context(|| format!("reading body from {url}"))?;
                Ok(buf)
            }
            Err(ureq::Error::Status(code, _)) => {
                bail!("HTTP {code} from {url}");
            }
            Err(e) => bail!("HTTP error fetching {url}: {e}"),
        }
    }
}

fn jet_cache_dir() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("JET_CACHE_DIR") {
        return Ok(PathBuf::from(p));
    }
    let base = dirs::cache_dir()
        .or_else(dirs::home_dir)
        .ok_or_else(|| anyhow::anyhow!("could not locate a cache directory"))?;
    Ok(base.join("jet"))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension("part");
    fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} → {}", tmp.display(), path.display()))?;
    Ok(())
}

pub fn sha256_of_bytes(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    hex::encode(h.finalize())
}

pub fn sha256_of_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path)
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(sha256_of_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_value() {
        assert_eq!(
            sha256_of_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_of_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
