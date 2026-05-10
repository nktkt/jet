//! JDK toolchain management. Auto-downloads from the Adoptium API into a
//! per-user store at `~/.jet/jdks/<vendor>-<version>/`, mirroring the layout
//! of `rustup` toolchains.
//!
//! The API endpoint:
//!
//! ```text
//! https://api.adoptium.net/v3/binary/latest/<version>/ga/<os>/<arch>/jdk/hotspot/normal/<vendor>
//! ```
//!
//! returns a redirect to the canonical tar.gz, which we stream into memory,
//! gunzip + untar, and atomically rename into place. Subsequent builds skip
//! the download.

use std::env;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::manifest::ToolchainConfig;

/// Resolved JDK paths after a `resolve_or_install` call.
pub struct JdkPaths {
    pub home: PathBuf,
    pub javac: PathBuf,
    pub java: PathBuf,
}

/// Look up an installed JDK matching `tc`, or download + install it if absent.
pub fn resolve_or_install(tc: &ToolchainConfig) -> Result<JdkPaths> {
    let dir = jdk_dir(&tc.vendor, tc.version)?;
    if !dir.is_dir() {
        install(tc)?;
    }
    locate_binaries(&dir)
}

/// Force a fresh install at `<store>/<vendor>-<version>/`. Intended for the
/// `jet jdk install` subcommand.
pub fn install(tc: &ToolchainConfig) -> Result<JdkPaths> {
    let dest = jdk_dir(&tc.vendor, tc.version)?;
    if dest.is_dir() {
        println!(
            "  JDK {} ({}) already installed at {}",
            tc.version,
            tc.vendor,
            dest.display()
        );
        return locate_binaries(&dest);
    }

    let (os, arch) = current_os_arch()?;
    // Adoptium's API uses the upstream project name (`eclipse` for Eclipse
    // Temurin) in the URL, not the consumer-facing brand. Translate friendly
    // vendor names users actually type into what the API expects.
    let api_vendor = match tc.vendor.as_str() {
        "temurin" | "eclipse" => "eclipse",
        other => other,
    };
    let url = format!(
        "https://api.adoptium.net/v3/binary/latest/{ver}/ga/{os}/{arch}/jdk/hotspot/normal/{vendor}",
        ver = tc.version,
        vendor = api_vendor,
    );
    println!(
        "  Downloading JDK {} ({}/{}, {}) — this may take a minute…",
        tc.version, os, arch, tc.vendor
    );
    let bytes = http_get_with_redirects(&url, 5).with_context(|| format!("GET {url}"))?;
    println!("  Got {} MB; extracting…", bytes.len() / 1024 / 1024);

    let staging = dest.with_extension("part");
    if staging.exists() {
        fs::remove_dir_all(&staging).ok();
    }
    fs::create_dir_all(&staging)
        .with_context(|| format!("creating {}", staging.display()))?;

    extract_tar_gz(&bytes, &staging)
        .with_context(|| format!("extracting JDK archive into {}", staging.display()))?;

    // Adoptium archives unpack to a single top-level directory like
    // `jdk-21.0.2+13/` (Linux) or `jdk-21.0.2+13.jdk/` (macOS). Find that
    // single child and lift it up.
    let top = single_child(&staging)
        .with_context(|| format!("locating JDK root in {}", staging.display()))?;
    if dest.exists() {
        let _ = fs::remove_dir_all(&dest);
    }
    fs::rename(&top, &dest)
        .with_context(|| format!("renaming {} → {}", top.display(), dest.display()))?;
    fs::remove_dir_all(&staging).ok();

    let paths = locate_binaries(&dest)?;
    println!(
        "  Installed JDK {} ({}) at {}",
        tc.version,
        tc.vendor,
        dest.display()
    );
    Ok(paths)
}

/// Walk `~/.jet/jdks/` and report each installed JDK as
/// `(vendor, version, path, version_string)`. Best-effort — entries that
/// don't have a usable `bin/javac` are skipped silently.
pub fn list_installed() -> Result<Vec<InstalledJdk>> {
    let store = jdks_store_dir()?;
    if !store.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&store)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let stem = match path.file_name().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        let (vendor, version) = match stem.split_once('-') {
            Some((v, n)) => (v.to_string(), n.parse::<u32>().unwrap_or(0)),
            None => (String::from("unknown"), 0),
        };
        if let Ok(paths) = locate_binaries(&path) {
            out.push(InstalledJdk {
                vendor,
                version,
                home: paths.home,
            });
        }
    }
    out.sort_by(|a, b| (a.version, a.vendor.clone()).cmp(&(b.version, b.vendor.clone())));
    Ok(out)
}

#[derive(Debug)]
pub struct InstalledJdk {
    pub vendor: String,
    pub version: u32,
    pub home: PathBuf,
}

/// `<JET_JDKS_DIR | $HOME/.jet/jdks>/<vendor>-<version>/`.
fn jdk_dir(vendor: &str, version: u32) -> Result<PathBuf> {
    Ok(jdks_store_dir()?.join(format!("{vendor}-{version}")))
}

fn jdks_store_dir() -> Result<PathBuf> {
    if let Ok(p) = env::var("JET_JDKS_DIR") {
        return Ok(PathBuf::from(p));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no $HOME directory"))?;
    Ok(home.join(".jet/jdks"))
}

/// Look for `bin/javac` either at `<dir>/bin/javac` (Linux/Windows convention)
/// or `<dir>/Contents/Home/bin/javac` (macOS .jdk bundle).
fn locate_binaries(dir: &Path) -> Result<JdkPaths> {
    let exe = if cfg!(windows) { ".exe" } else { "" };
    let candidates = [
        dir.to_path_buf(),
        dir.join("Contents/Home"),
    ];
    for home in candidates {
        let javac = home.join("bin").join(format!("javac{exe}"));
        let java = home.join("bin").join(format!("java{exe}"));
        if javac.is_file() && java.is_file() {
            return Ok(JdkPaths { home, javac, java });
        }
    }
    bail!(
        "could not find bin/javac under {} (expected jdk-style or .jdk-bundle layout)",
        dir.display()
    );
}

fn current_os_arch() -> Result<(&'static str, &'static str)> {
    let os = if cfg!(target_os = "macos") {
        "mac"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        bail!("unsupported OS for Adoptium auto-download");
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        bail!("unsupported CPU architecture for Adoptium auto-download");
    };
    Ok((os, arch))
}

fn http_get_with_redirects(url: &str, max_redirects: usize) -> Result<Vec<u8>> {
    let agent = ureq::AgentBuilder::new()
        .redirects(max_redirects as u32)
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(180))
        .user_agent(concat!("jet/", env!("CARGO_PKG_VERSION")))
        .build();
    match agent.get(url).call() {
        Ok(resp) => {
            let mut buf = Vec::with_capacity(200 * 1024 * 1024);
            resp.into_reader().read_to_end(&mut buf)?;
            Ok(buf)
        }
        Err(ureq::Error::Status(code, _)) => {
            bail!("HTTP {code} from {url}")
        }
        Err(e) => bail!("HTTP error {e} for {url}"),
    }
}

fn extract_tar_gz(bytes: &[u8], dest: &Path) -> Result<()> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    archive.set_preserve_permissions(true);
    archive
        .unpack(dest)
        .with_context(|| format!("unpacking tar.gz into {}", dest.display()))?;
    Ok(())
}

fn single_child(dir: &Path) -> Result<PathBuf> {
    let entries: Vec<_> = fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();
    if entries.len() == 1 {
        return Ok(entries.into_iter().next().unwrap().path());
    }
    // Some Adoptium archives include a `_pax_global_header` extra. Skip it.
    let dirs: Vec<PathBuf> = entries
        .into_iter()
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    if dirs.len() == 1 {
        return Ok(dirs.into_iter().next().unwrap());
    }
    bail!(
        "expected exactly one top-level directory in archive, found {}",
        dirs.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_arch_resolves() {
        let (os, arch) = current_os_arch().unwrap();
        assert!(["mac", "linux", "windows"].contains(&os));
        assert!(["x64", "aarch64"].contains(&arch));
    }

    #[test]
    fn jdk_dir_layout() {
        unsafe {
            std::env::set_var("JET_JDKS_DIR", "/tmp/jet-jdks-test");
        }
        let p = jdk_dir("temurin", 21).unwrap();
        assert!(p.ends_with("temurin-21"));
        unsafe {
            std::env::remove_var("JET_JDKS_DIR");
        }
    }
}
