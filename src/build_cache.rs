//! Content-addressed build cache.
//!
//! Keyed by the existing `do_build_at` fingerprint (a sha256 over sources +
//! dep paths + java version + flags). On a hit we skip `javac` and copy the
//! cached classes back into `target/classes/<member>/`. On a miss we run
//! `javac` as usual and snapshot the result into the cache for next time.
//!
//! Cache layout:
//!
//! ```text
//! <cache_dir>/build/
//!   <hash>/        # ready entry (tree of .class files, mirroring classes_dir)
//!   <hash>.part/   # in-flight entry; promoted via rename on success
//! ```
//!
//! `<cache_dir>` is `~/Library/Caches/jet/` on macOS, `~/.cache/jet/` on
//! Linux, and `%LOCALAPPDATA%\jet\` on Windows (via the `dirs` crate). The
//! `JET_CACHE_DIR` env var overrides for tests.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use walkdir::WalkDir;

pub struct ContentCache {
    root: PathBuf,
}

impl ContentCache {
    /// Resolve the cache root, creating it if missing. Returns `Ok(None)` if
    /// the host has no usable cache directory (rare; we just disable caching).
    pub fn open() -> Result<Option<Self>> {
        let base = if let Ok(p) = std::env::var("JET_CACHE_DIR") {
            PathBuf::from(p)
        } else {
            match dirs::cache_dir() {
                Some(p) => p.join("jet"),
                None => return Ok(None),
            }
        };
        let root = base.join("build");
        fs::create_dir_all(&root)
            .with_context(|| format!("creating {}", root.display()))?;
        Ok(Some(Self { root }))
    }

    /// On hit, replace the contents of `dest` with the cache entry and return
    /// `true`. On miss, return `false` without touching `dest`.
    ///
    /// Atomically validates the entry by checking for a `.ready` marker —
    /// guards against partially-populated entries from a previous crash.
    pub fn try_restore(&self, key: &str, dest: &Path) -> Result<bool> {
        let entry = self.root.join(key);
        if !entry.is_dir() || !entry.join(".ready").is_file() {
            return Ok(false);
        }
        // Wipe destination so stale class files don't leak in.
        if dest.exists() {
            fs::remove_dir_all(dest)
                .with_context(|| format!("clearing {}", dest.display()))?;
        }
        fs::create_dir_all(dest)
            .with_context(|| format!("creating {}", dest.display()))?;
        copy_tree(&entry, dest, |name| name == ".ready")?;
        Ok(true)
    }

    /// Snapshot `src` into the cache under `key`. Atomic: writes into a
    /// `<key>.part/` directory first, then renames to `<key>/`.
    pub fn store(&self, key: &str, src: &Path) -> Result<()> {
        if !src.is_dir() {
            return Ok(());
        }
        let final_dir = self.root.join(key);
        if final_dir.is_dir() && final_dir.join(".ready").is_file() {
            // Already cached. Skip — avoids re-copying on every up-to-date build.
            return Ok(());
        }
        let tmp = self.root.join(format!("{key}.part"));
        if tmp.exists() {
            // Leftover from a crashed earlier run.
            let _ = fs::remove_dir_all(&tmp);
        }
        fs::create_dir_all(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        copy_tree(src, &tmp, |_| false)?;
        // Mark ready (sentinel file) before promotion.
        fs::write(tmp.join(".ready"), "")
            .with_context(|| format!("writing readiness marker at {}", tmp.display()))?;
        // Promote: if final_dir already exists (race), just clean up.
        if final_dir.exists() {
            let _ = fs::remove_dir_all(&tmp);
            return Ok(());
        }
        fs::rename(&tmp, &final_dir).with_context(|| {
            format!("renaming {} → {}", tmp.display(), final_dir.display())
        })?;
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn copy_tree(src: &Path, dst: &Path, skip: impl Fn(&str) -> bool) -> Result<()> {
    for entry in WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let rel = p.strip_prefix(src).unwrap();
        if let Some(name) = rel.file_name().and_then(|s| s.to_str()) {
            if skip(name) {
                continue;
            }
        }
        let target = dst.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::copy(p, &target)
            .with_context(|| format!("copying {} → {}", p.display(), target.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolated_cache() -> (tempfile::TempDir, ContentCache) {
        let dir = tempfile::tempdir().unwrap();
        // Set JET_CACHE_DIR for this process (NB: tests in same crate share env;
        // each test uses a unique dir so collisions are harmless).
        unsafe {
            std::env::set_var("JET_CACHE_DIR", dir.path());
        }
        let cache = ContentCache::open().unwrap().unwrap();
        (dir, cache)
    }

    #[test]
    fn miss_then_store_then_hit() {
        let (_tmp, cache) = isolated_cache();
        let work = tempfile::tempdir().unwrap();
        let src = work.path().join("classes");
        fs::create_dir_all(src.join("com/example")).unwrap();
        fs::write(src.join("com/example/Foo.class"), b"AAA").unwrap();
        fs::write(src.join("com/example/Bar.class"), b"BBB").unwrap();

        let key = "deadbeef";
        let dest = work.path().join("restored");

        // Miss.
        assert!(!cache.try_restore(key, &dest).unwrap());
        assert!(!dest.exists());

        // Store.
        cache.store(key, &src).unwrap();

        // Hit: dest is populated with a copy.
        assert!(cache.try_restore(key, &dest).unwrap());
        let foo = fs::read(dest.join("com/example/Foo.class")).unwrap();
        assert_eq!(foo, b"AAA");
        assert!(dest.join("com/example/Bar.class").is_file());
        // Internal sentinel must NOT leak into restored output.
        assert!(!dest.join(".ready").exists());
    }

    #[test]
    fn restore_clears_stale_files() {
        let (_tmp, cache) = isolated_cache();
        let work = tempfile::tempdir().unwrap();
        let src = work.path().join("classes");
        fs::create_dir_all(src.join("p")).unwrap();
        fs::write(src.join("p/A.class"), b"a").unwrap();

        let key = "feedface";
        cache.store(key, &src).unwrap();

        let dest = work.path().join("dest");
        fs::create_dir_all(dest.join("p")).unwrap();
        // Stale class that should be wiped:
        fs::write(dest.join("p/Stale.class"), b"old").unwrap();

        assert!(cache.try_restore(key, &dest).unwrap());
        assert!(dest.join("p/A.class").is_file());
        assert!(!dest.join("p/Stale.class").exists(), "stale file must be removed on restore");
    }

    #[test]
    fn second_store_is_noop() {
        let (_tmp, cache) = isolated_cache();
        let work = tempfile::tempdir().unwrap();
        let src = work.path().join("c");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("Foo.class"), b"x").unwrap();
        let key = "k1";
        cache.store(key, &src).unwrap();
        // Mutate src then call store again — cached entry should be unchanged.
        fs::write(src.join("Foo.class"), b"y").unwrap();
        cache.store(key, &src).unwrap();
        let dest = work.path().join("dest");
        assert!(cache.try_restore(key, &dest).unwrap());
        assert_eq!(fs::read(dest.join("Foo.class")).unwrap(), b"x");
    }
}
