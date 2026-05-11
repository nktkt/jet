//! `jet publish [--dry-run] [--no-sign]` — generate POM, sources JAR,
//! checksums, optional GPG signatures, and either upload to a Maven
//! repository or write the bundle to `target/publish/`.
//!
//! Maven repository layout (used both for HTTP PUT paths and for the dry-run
//! tree):
//!
//! ```text
//! <group>/<group>/<artifact>/<version>/<artifact>-<version>.{jar,pom}
//! <group>/<group>/<artifact>/<version>/<artifact>-<version>-sources.jar
//! plus .md5, .sha1, .asc (when signed) for each
//! ```

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::build::{BuildArgs, do_build};
use crate::jar::{Entry, JarBuilder, render_manifest};
use crate::lockfile::{LOCKFILE_NAME, Lockfile};
use crate::manifest::Manifest;
use crate::pom::render_pom;

pub struct PublishArgs {
    /// Stage everything under `target/publish/` instead of uploading.
    pub dry_run: bool,
    /// Skip GPG signing (overrides `[publish].sign`).
    pub no_sign: bool,
}

pub fn cmd_publish(args: PublishArgs) -> Result<()> {
    let outputs = do_build(BuildArgs {
        release: false,
        force_resolve: false,
        package: None,
        jobs: None,
        no_cache: false,
        check_only: false,
    })?;
    let root = outputs.project_root.clone();
    let manifest = outputs.manifest;

    let pkg = manifest.pkg()?;
    let group = pkg
        .group
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("[package].group is required for `jet publish`"))?
        .to_string();
    let artifact = pkg.name.clone();
    let version = pkg.version.clone();

    let publish = manifest.publish.clone().unwrap_or_default();

    // 1. Build the project's main JAR (we reuse the package logic by
    //    walking target/classes + src/main/resources directly to avoid the
    //    coupling with cmd/package.rs).
    let main_jar = build_main_jar(&root, &outputs.classes_dir, &manifest, &outputs.target_dir)?;

    // 2. Build the sources JAR.
    let sources_jar = build_sources_jar(&root, &manifest, &outputs.target_dir)?;

    // 3. Generate the POM.
    let lock_path = root.join(LOCKFILE_NAME);
    let lockfile = if lock_path.is_file() {
        Lockfile::load(&root).ok()
    } else {
        None
    };
    let pom_xml = render_pom(&manifest, lockfile.as_ref())?;
    let pom_path = outputs.target_dir.join(format!("{artifact}-{version}.pom"));
    fs::write(&pom_path, &pom_xml)
        .with_context(|| format!("writing {}", pom_path.display()))?;

    // 4. Decide signing policy.
    let sign = publish.sign && !args.no_sign;
    if sign && which::which("gpg").is_err() {
        bail!(
            "GPG signing requested but `gpg` was not found on PATH.\n\
             Install GnuPG, or pass `--no-sign` to publish without signatures."
        );
    }

    // 5. Sign + checksum each artifact.
    let mut artifacts: Vec<Artifact> = Vec::new();
    artifacts.push(Artifact { local: main_jar, remote_name: format!("{artifact}-{version}.jar") });
    artifacts.push(Artifact {
        local: sources_jar,
        remote_name: format!("{artifact}-{version}-sources.jar"),
    });
    artifacts.push(Artifact { local: pom_path, remote_name: format!("{artifact}-{version}.pom") });

    let mut staged: Vec<StagedFile> = Vec::new();
    for a in &artifacts {
        // Primary file.
        let bytes = fs::read(&a.local)?;
        let md5 = hex::encode(md5_simple(&bytes));
        let sha1 = hex::encode({
            let mut h = Sha1::new();
            h.update(&bytes);
            h.finalize()
        });
        staged.push(StagedFile { remote: a.remote_name.clone(), data: Data::Bytes(bytes.clone()) });
        staged.push(StagedFile {
            remote: format!("{}.md5", a.remote_name),
            data: Data::Bytes(md5.into_bytes()),
        });
        staged.push(StagedFile {
            remote: format!("{}.sha1", a.remote_name),
            data: Data::Bytes(sha1.into_bytes()),
        });
        if sign {
            let asc = gpg_sign(&a.local, publish.gpg_key.as_deref())?;
            staged.push(StagedFile { remote: format!("{}.asc", a.remote_name), data: Data::Bytes(asc) });
        }
    }

    // 6. Compute the remote group path.
    let group_path = group.replace('.', "/");
    let remote_prefix = format!("{group_path}/{artifact}/{version}");

    if args.dry_run {
        let stage_root = outputs.target_dir.join("publish");
        if stage_root.exists() {
            fs::remove_dir_all(&stage_root).ok();
        }
        let dest_dir = stage_root.join(&remote_prefix);
        fs::create_dir_all(&dest_dir)?;
        for f in &staged {
            let path = dest_dir.join(&f.remote);
            match &f.data {
                Data::Bytes(b) => fs::write(&path, b)?,
            }
        }
        println!(
            "  Staged {} files at {}",
            staged.len(),
            dest_dir.display()
        );
        return Ok(());
    }

    // 7. Upload via HTTP PUT.
    let url = std::env::var("JET_PUBLISH_URL")
        .ok()
        .or_else(|| publish.url.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no publish URL configured.\n\
                 set [publish].url in jet.toml or `JET_PUBLISH_URL` env var,\n\
                 or run with `--dry-run` to stage locally."
            )
        })?;
    let user = std::env::var("JET_PUBLISH_USER").ok();
    let token = std::env::var("JET_PUBLISH_TOKEN").ok();
    let auth = match (user.as_deref(), token.as_deref()) {
        (_, Some(tok)) if !tok.is_empty() && user.is_none() => Auth::Bearer(tok.to_string()),
        (Some(u), Some(t)) if !u.is_empty() && !t.is_empty() => {
            Auth::Basic(u.to_string(), t.to_string())
        }
        _ => {
            bail!(
                "publish credentials missing.\n\
                 set `JET_PUBLISH_TOKEN` (bearer) or `JET_PUBLISH_USER` + \
                 `JET_PUBLISH_TOKEN` (basic auth)."
            );
        }
    };

    let base = url.trim_end_matches('/').to_string();
    println!(
        "  Publishing {} files to {base}/{remote_prefix}",
        staged.len()
    );
    for f in &staged {
        let target = format!("{base}/{remote_prefix}/{}", f.remote);
        match &f.data {
            Data::Bytes(b) => http_put(&target, b, &auth)?,
        }
        println!("    PUT {}", target);
    }
    println!("  Published `{group}:{artifact}:{version}`.");
    Ok(())
}

struct Artifact {
    local: PathBuf,
    remote_name: String,
}

struct StagedFile {
    remote: String,
    data: Data,
}

enum Data {
    Bytes(Vec<u8>),
}

enum Auth {
    Basic(String, String),
    Bearer(String),
}

fn gpg_sign(file: &Path, key: Option<&str>) -> Result<Vec<u8>> {
    let mut cmd = Command::new("gpg");
    cmd.arg("--batch")
        .arg("--yes")
        .arg("--detach-sign")
        .arg("--armor");
    if let Some(k) = key {
        cmd.arg("-u").arg(k);
    }
    cmd.arg("--output").arg("-").arg(file);
    let output = cmd.output().with_context(|| "running gpg")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "gpg failed signing {}: {}",
            file.display(),
            stderr.trim()
        );
    }
    Ok(output.stdout)
}

fn http_put(url: &str, body: &[u8], auth: &Auth) -> Result<()> {
    let mut req = ureq::put(url);
    match auth {
        Auth::Bearer(t) => req = req.set("Authorization", &format!("Bearer {t}")),
        Auth::Basic(u, p) => {
            use base64_simple as b64;
            let creds = format!("{u}:{p}");
            req = req.set("Authorization", &format!("Basic {}", b64::encode(creds.as_bytes())));
        }
    }
    let resp = req
        .send_bytes(body)
        .with_context(|| format!("PUT {url}"))?;
    let code = resp.status();
    if !(200..=299).contains(&code) {
        bail!("PUT {url} returned HTTP {code}");
    }
    Ok(())
}

/// Build the project JAR (analogous to `jet package` thin mode but inlined to
/// keep publish self-contained).
fn build_main_jar(
    root: &Path,
    classes_dir: &Path,
    manifest: &Manifest,
    target_dir: &Path,
) -> Result<PathBuf> {
    let pkg = manifest.pkg()?;
    let mut builder = JarBuilder::new();
    builder.put(Entry::dir("META-INF"));
    let headers: Vec<(&str, String)> = vec![
        ("Manifest-Version", "1.0".into()),
        ("Created-By", format!("jet {}", env!("CARGO_PKG_VERSION"))),
        ("Build-Jdk-Spec", pkg.java.to_string()),
        ("Implementation-Title", pkg.name.clone()),
        ("Implementation-Version", pkg.version.clone()),
    ];
    builder.put(Entry::file("META-INF/MANIFEST.MF", render_manifest(&headers)));

    add_dir_to_jar(&mut builder, classes_dir, "")?;
    let resources_dir = root.join("src/main/resources");
    if resources_dir.is_dir() {
        add_dir_to_jar(&mut builder, &resources_dir, "")?;
    }

    fs::create_dir_all(target_dir)?;
    let jar_path = target_dir.join(format!("{}-{}.jar", pkg.name, pkg.version));
    builder.write_to(&jar_path)?;
    Ok(jar_path)
}

/// Sources JAR: zip every file under `src/main/java` (and resources) at their
/// original paths.
fn build_sources_jar(
    root: &Path,
    manifest: &Manifest,
    target_dir: &Path,
) -> Result<PathBuf> {
    let pkg = manifest.pkg()?;
    let mut builder = JarBuilder::new();
    builder.put(Entry::dir("META-INF"));
    let headers: Vec<(&str, String)> = vec![
        ("Manifest-Version", "1.0".into()),
        ("Created-By", format!("jet {}", env!("CARGO_PKG_VERSION"))),
    ];
    builder.put(Entry::file("META-INF/MANIFEST.MF", render_manifest(&headers)));

    let src_dir = root.join("src/main/java");
    if src_dir.is_dir() {
        add_dir_to_jar(&mut builder, &src_dir, "")?;
    }
    let resources_dir = root.join("src/main/resources");
    if resources_dir.is_dir() {
        add_dir_to_jar(&mut builder, &resources_dir, "")?;
    }

    fs::create_dir_all(target_dir)?;
    let jar_path = target_dir.join(format!("{}-{}-sources.jar", pkg.name, pkg.version));
    builder.write_to(&jar_path)?;
    Ok(jar_path)
}

fn add_dir_to_jar(builder: &mut JarBuilder, dir: &Path, prefix: &str) -> Result<()> {
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if matches!(name, ".DS_Store" | "Thumbs.db") {
            continue;
        }
        let rel = p.strip_prefix(dir)?;
        let mut entry_path = String::new();
        if !prefix.is_empty() {
            entry_path.push_str(prefix);
            if !prefix.ends_with('/') {
                entry_path.push('/');
            }
        }
        entry_path.push_str(&rel.to_string_lossy().replace('\\', "/"));
        let bytes = fs::read(p)?;
        builder.put(Entry::file(entry_path, bytes));
    }
    Ok(())
}

/// Minimal MD5 implementation for checksums. Maven repos accept hex MD5.
fn md5_simple(data: &[u8]) -> [u8; 16] {
    use md5_compat::compute;
    compute(data)
}

mod md5_compat {
    /// Tiny MD5 (RFC 1321) — checksums for Maven Central are still expected as
    /// .md5, even though sha1/sha256 also exist. We don't pull the `md-5` crate
    /// just for this one use.
    pub fn compute(input: &[u8]) -> [u8; 16] {
        const S: [u32; 64] = [
            7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
            5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
            4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
            6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
        ];
        const K: [u32; 64] = [
            0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
            0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
            0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
            0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
            0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
            0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
            0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
            0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
        ];

        let mut a0: u32 = 0x67452301;
        let mut b0: u32 = 0xefcdab89;
        let mut c0: u32 = 0x98badcfe;
        let mut d0: u32 = 0x10325476;

        let bit_len = (input.len() as u64).wrapping_mul(8);
        let mut padded = input.to_vec();
        padded.push(0x80);
        while padded.len() % 64 != 56 {
            padded.push(0);
        }
        padded.extend_from_slice(&bit_len.to_le_bytes());

        for chunk in padded.chunks_exact(64) {
            let mut m = [0u32; 16];
            for (i, w) in chunk.chunks_exact(4).enumerate() {
                m[i] = u32::from_le_bytes(w.try_into().unwrap());
            }
            let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
            for i in 0..64 {
                let (f, g) = match i {
                    0..=15 => ((b & c) | ((!b) & d), i),
                    16..=31 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                    32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                    _ => (c ^ (b | !d), (7 * i) % 16),
                };
                let temp = d;
                d = c;
                c = b;
                b = b.wrapping_add(
                    a.wrapping_add(f)
                        .wrapping_add(K[i])
                        .wrapping_add(m[g])
                        .rotate_left(S[i]),
                );
                a = temp;
            }
            a0 = a0.wrapping_add(a);
            b0 = b0.wrapping_add(b);
            c0 = c0.wrapping_add(c);
            d0 = d0.wrapping_add(d);
        }
        let mut out = [0u8; 16];
        out[..4].copy_from_slice(&a0.to_le_bytes());
        out[4..8].copy_from_slice(&b0.to_le_bytes());
        out[8..12].copy_from_slice(&c0.to_le_bytes());
        out[12..].copy_from_slice(&d0.to_le_bytes());
        out
    }
}

mod base64_simple {
    /// Tiny base64 encoder for HTTP basic auth. URL-safe? No — standard.
    pub fn encode(input: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
        for chunk in input.chunks(3) {
            let b0 = chunk[0];
            let b1 = chunk.get(1).copied().unwrap_or(0);
            let b2 = chunk.get(2).copied().unwrap_or(0);
            out.push(ALPHABET[(b0 >> 2) as usize] as char);
            out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
            if chunk.len() > 1 {
                out.push(ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(ALPHABET[(b2 & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_known_values() {
        assert_eq!(hex::encode(md5_simple(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex::encode(md5_simple(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex::encode(md5_simple(b"The quick brown fox jumps over the lazy dog")),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn base64_known_values() {
        assert_eq!(base64_simple::encode(b""), "");
        assert_eq!(base64_simple::encode(b"f"), "Zg==");
        assert_eq!(base64_simple::encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_simple::encode(b"user:pass"), "dXNlcjpwYXNz");
    }

    #[test]
    fn unused_writer() {
        // Suppress unused import warning for `Write`.
        let mut buf: Vec<u8> = Vec::new();
        buf.write_all(b"x").unwrap();
        assert_eq!(buf, b"x");
    }
}
