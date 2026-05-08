//! JAR (ZIP) writer with reproducible defaults and MANIFEST.MF formatting.
//!
//! Reproducibility:
//! - Entries written in lexicographic order (POSIX byte order on path), with
//!   `META-INF/` and `META-INF/MANIFEST.MF` hoisted to positions 0 and 1.
//! - All entries pinned to `SOURCE_DATE_EPOCH` if set (clamped to 1980-01-01),
//!   else 2024-01-01 00:00:00 UTC.
//! - Fixed Unix permissions: 0o644 files, 0o755 dirs.
//! - Deflate compression for non-empty entries.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result};
use zip::DateTime;
use zip::CompressionMethod;
use zip::write::{FileOptions, ZipWriter};

const DEFAULT_TIMESTAMP_EPOCH: i64 = 1_704_067_200; // 2024-01-01T00:00:00Z
const DOS_MIN_EPOCH: i64 = 315_532_800; // 1980-01-01T00:00:00Z

/// One in-memory entry to be written to the JAR.
#[derive(Clone)]
pub struct Entry {
    pub path: String,
    pub bytes: Vec<u8>,
    pub is_dir: bool,
}

impl Entry {
    pub fn file(path: impl Into<String>, bytes: Vec<u8>) -> Self {
        Self { path: path.into(), bytes, is_dir: false }
    }
    pub fn dir(path: impl Into<String>) -> Self {
        let mut p = path.into();
        if !p.ends_with('/') {
            p.push('/');
        }
        Self { path: p, bytes: Vec::new(), is_dir: true }
    }
}

pub struct JarBuilder {
    entries: BTreeMap<String, Entry>,
}

impl Default for JarBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl JarBuilder {
    pub fn new() -> Self {
        Self { entries: BTreeMap::new() }
    }

    /// Add (or replace) an entry.
    pub fn put(&mut self, entry: Entry) {
        self.entries.insert(entry.path.clone(), entry);
    }

    /// Add an entry only if no entry exists at that path. Returns `true` if
    /// inserted, `false` if a prior entry was kept (caller can use this for
    /// "first-wins" with conflict reporting).
    pub fn put_if_absent(&mut self, entry: Entry) -> bool {
        if self.entries.contains_key(&entry.path) {
            return false;
        }
        self.entries.insert(entry.path.clone(), entry);
        true
    }

    pub fn contains(&self, path: &str) -> bool {
        self.entries.contains_key(path)
    }

    pub fn get(&self, path: &str) -> Option<&Entry> {
        self.entries.get(path)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Write to disk. Caller is responsible for having added META-INF/MANIFEST.MF.
    pub fn write_to(&self, path: &Path) -> Result<()> {
        let file = File::create(path)
            .with_context(|| format!("creating {}", path.display()))?;
        let writer = BufWriter::new(file);
        let mut zip = ZipWriter::new(writer);

        let dt = reproducible_datetime()?;
        let file_opts: FileOptions<'_, ()> = FileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(dt)
            .unix_permissions(0o644);
        let dir_opts: FileOptions<'_, ()> = FileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .last_modified_time(dt)
            .unix_permissions(0o755);

        // Spec: META-INF/ first, then META-INF/MANIFEST.MF, then everything
        // else in lexicographic order. BTreeMap already sorts, but we want to
        // ensure the spec ordering for the two well-known prefixes.
        let mut written: std::collections::HashSet<&str> = Default::default();

        if self.entries.contains_key("META-INF/") {
            self.write_one(&mut zip, "META-INF/", &dir_opts, &file_opts)?;
            written.insert("META-INF/");
        }
        if self.entries.contains_key("META-INF/MANIFEST.MF") {
            self.write_one(&mut zip, "META-INF/MANIFEST.MF", &dir_opts, &file_opts)?;
            written.insert("META-INF/MANIFEST.MF");
        }
        for path in self.entries.keys() {
            if written.contains(path.as_str()) {
                continue;
            }
            self.write_one(&mut zip, path, &dir_opts, &file_opts)?;
        }
        zip.finish().context("finalizing JAR central directory")?;
        Ok(())
    }

    fn write_one<W: Write + std::io::Seek>(
        &self,
        zip: &mut ZipWriter<W>,
        path: &str,
        dir_opts: &FileOptions<'_, ()>,
        file_opts: &FileOptions<'_, ()>,
    ) -> Result<()> {
        let entry = self.entries.get(path).expect("path must exist");
        if entry.is_dir {
            zip.add_directory(&entry.path, dir_opts.clone())
                .with_context(|| format!("adding directory {}", entry.path))?;
        } else {
            zip.start_file(&entry.path, file_opts.clone())
                .with_context(|| format!("starting entry {}", entry.path))?;
            zip.write_all(&entry.bytes)
                .with_context(|| format!("writing entry {}", entry.path))?;
        }
        Ok(())
    }
}

/// Determine the JAR's mtime: SOURCE_DATE_EPOCH (clamped to 1980+) or default.
fn reproducible_datetime() -> Result<DateTime> {
    let epoch = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(DEFAULT_TIMESTAMP_EPOCH)
        .max(DOS_MIN_EPOCH);
    let secs = epoch.try_into().unwrap_or(DEFAULT_TIMESTAMP_EPOCH as u64);
    epoch_to_datetime(secs)
}

fn epoch_to_datetime(epoch_secs: u64) -> Result<DateTime> {
    // Naive UTC conversion (no leap seconds; ZIP DOS time has 2-second resolution).
    let days_per_month = |y: i64, m: u32| -> u32 {
        match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                    29
                } else {
                    28
                }
            }
            _ => 0,
        }
    };

    let mut secs = epoch_secs as i64;
    let day_seconds = 24 * 3600;
    let mut days = secs / day_seconds;
    secs -= days * day_seconds;
    let h = (secs / 3600) as u8;
    secs -= h as i64 * 3600;
    let m = (secs / 60) as u8;
    let s = (secs - m as i64 * 60) as u8;

    let mut y: i64 = 1970;
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let yd = if leap { 366 } else { 365 };
        if days < yd {
            break;
        }
        days -= yd;
        y += 1;
    }
    let mut mo: u32 = 1;
    while mo <= 12 {
        let dm = days_per_month(y, mo) as i64;
        if days < dm {
            break;
        }
        days -= dm;
        mo += 1;
    }
    let d = (days + 1) as u8;

    DateTime::from_date_and_time(y as u16, mo as u8, d, h, m, s)
        .map_err(|_| anyhow::anyhow!("invalid date for SOURCE_DATE_EPOCH={epoch_secs}"))
}

/// Render a `MANIFEST.MF` body. Each header line is wrapped to 72 bytes (per
/// JAR spec); continuation lines are prefixed with a single space. Lines end
/// with CRLF. A blank line terminates the main section.
pub fn render_manifest(headers: &[(&str, String)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(256);
    for (key, value) in headers {
        let mut line = format!("{key}: {value}");
        // Wrap to 72 bytes per line. The first segment may be 72; continuation
        // lines start with a single space and contribute to the 72-byte budget.
        let mut first = true;
        while !line.is_empty() {
            if first {
                let take = byte_take(&line, 72);
                let (head, tail) = split_at_byte_safe(&line, take);
                out.extend_from_slice(head.as_bytes());
                out.extend_from_slice(b"\r\n");
                line = tail.to_string();
                first = false;
            } else {
                let take = byte_take(&line, 71);
                let (head, tail) = split_at_byte_safe(&line, take);
                out.push(b' ');
                out.extend_from_slice(head.as_bytes());
                out.extend_from_slice(b"\r\n");
                line = tail.to_string();
            }
        }
    }
    out.extend_from_slice(b"\r\n");
    out
}

fn byte_take(s: &str, n: usize) -> usize {
    s.len().min(n)
}

fn split_at_byte_safe(s: &str, n: usize) -> (&str, &str) {
    // We treat the manifest as ASCII for the headers we emit, so byte split is safe.
    if n >= s.len() {
        return (s, "");
    }
    let mut idx = n;
    while !s.is_char_boundary(idx) && idx > 0 {
        idx -= 1;
    }
    s.split_at(idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_basic() {
        let headers: Vec<(&str, String)> = vec![
            ("Manifest-Version", "1.0".into()),
            ("Main-Class", "com.example.Main".into()),
            ("Implementation-Version", "0.1.0".into()),
        ];
        let body = render_manifest(&headers);
        let s = String::from_utf8(body).unwrap();
        assert!(s.starts_with("Manifest-Version: 1.0\r\n"));
        assert!(s.contains("Main-Class: com.example.Main\r\n"));
        assert!(s.ends_with("\r\n\r\n"));
    }

    #[test]
    fn manifest_long_line_wraps_at_72_bytes() {
        let long = "a".repeat(200);
        let body = render_manifest(&[("Class-Path", long.clone())]);
        let s = String::from_utf8(body).unwrap();
        for line in s.split("\r\n") {
            // Each rendered line must be <= 72 bytes (continuation lines
            // include the leading space).
            assert!(line.len() <= 72, "line too long: {} bytes", line.len());
        }
        // Continuation lines must begin with a single space.
        for (i, line) in s.split("\r\n").enumerate() {
            if i > 0 && !line.is_empty() {
                assert!(line.starts_with(' '), "continuation must start with space");
            }
        }
        // Reconstructing must match the original.
        let mut joined = String::new();
        for (i, line) in s.trim_end().split("\r\n").enumerate() {
            if i == 0 {
                joined.push_str(line.trim_start_matches("Class-Path: "));
            } else {
                joined.push_str(line.trim_start_matches(' '));
            }
        }
        assert_eq!(joined, long);
    }

    #[test]
    fn epoch_to_datetime_known_values() {
        // 2024-01-01T00:00:00Z
        let dt = epoch_to_datetime(1_704_067_200).unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 1);

        // 1980-01-01T00:00:00Z
        let dt = epoch_to_datetime(315_532_800).unwrap();
        assert_eq!(dt.year(), 1980);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 1);
    }

    #[test]
    fn jar_builder_writes_sorted_with_meta_inf_first() {
        use std::io::Read as _;
        use zip::ZipArchive;

        let mut b = JarBuilder::new();
        b.put(Entry::file("zzz/last.txt", b"z".to_vec()));
        b.put(Entry::file("aaa/first.txt", b"a".to_vec()));
        b.put(Entry::dir("META-INF"));
        b.put(Entry::file(
            "META-INF/MANIFEST.MF",
            render_manifest(&[("Manifest-Version", "1.0".into())]),
        ));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jar");
        b.write_to(&path).unwrap();

        let mut z = ZipArchive::new(File::open(&path).unwrap()).unwrap();
        let names: Vec<String> =
            (0..z.len()).map(|i| z.by_index(i).unwrap().name().to_string()).collect();
        assert_eq!(names[0], "META-INF/");
        assert_eq!(names[1], "META-INF/MANIFEST.MF");
        // Remaining sorted lexicographically.
        let rest = &names[2..];
        let mut expected = rest.to_vec();
        expected.sort();
        assert_eq!(rest, &expected[..]);
        // Sanity: contents readable.
        let mut buf = String::new();
        z.by_name("aaa/first.txt").unwrap().read_to_string(&mut buf).unwrap();
        assert_eq!(buf, "a");
    }
}
