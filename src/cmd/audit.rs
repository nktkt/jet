//! `jet audit [--ignore <id,id,...>]` — security scan against OSV.dev.
//!
//! Walks `jet.lock`, builds a single `POST /v1/querybatch` against
//! `api.osv.dev` with every `Maven:<group:artifact>` coord + pinned version,
//! then fetches `/v1/vulns/<id>` for each unique advisory ID returned. Prints
//! findings sorted by severity (CRITICAL > HIGH > MODERATE > LOW > UNKNOWN),
//! and exits non-zero on any finding the user hasn't explicitly ignored.
//!
//! The batch endpoint is the right call here: 100+ deps in one round trip
//! beats a per-coord stream, and OSV happily accepts mixed coords. The
//! per-ID detail call (one per *unique* vulnerability, not per package) gets
//! us a human-readable summary and severity tag that the batch response
//! doesn't include — and it's typically only 1-3 calls because production
//! projects rarely have many distinct CVEs in flight.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::lockfile::{LOCKFILE_NAME, Lockfile};
use crate::manifest::Manifest;

const OSV_QUERYBATCH: &str = "https://api.osv.dev/v1/querybatch";
const OSV_VULN_DETAIL: &str = "https://api.osv.dev/v1/vulns/";

pub struct AuditArgs {
    /// Comma-separated list of advisory IDs (GHSA-*, CVE-*) to suppress.
    /// Useful for known-accepted findings where the project has reviewed
    /// the risk (e.g. test-only deps, unreachable code paths).
    pub ignore: Option<String>,
}

pub fn cmd_audit(args: AuditArgs) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let root = Manifest::find_root(&cwd)?;
    if !root.join(LOCKFILE_NAME).is_file() {
        bail!(
            "no jet.lock at {} — run `jet build` first so audit has resolved versions to check",
            root.join(LOCKFILE_NAME).display()
        );
    }
    let lock = Lockfile::load(&root)?;
    let ignore: BTreeSet<String> = args
        .ignore
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .user_agent(concat!("jet/", env!("CARGO_PKG_VERSION")))
        .build();

    let queries: Vec<BatchQuery> = lock
        .packages
        .iter()
        .map(|p| BatchQuery {
            package: BatchPackage {
                ecosystem: "Maven".into(),
                name: p.name.clone(),
            },
            version: p.version.clone(),
        })
        .collect();
    if queries.is_empty() {
        println!("No packages in jet.lock to audit.");
        return Ok(());
    }
    let total = queries.len();
    println!("Auditing {} packages against api.osv.dev…", total);

    let body = serde_json::to_string(&BatchRequest { queries: &queries })
        .context("serializing OSV querybatch request")?;
    let resp = agent
        .post(OSV_QUERYBATCH)
        .set("content-type", "application/json")
        .send_string(&body)
        .with_context(|| format!("POST {OSV_QUERYBATCH}"))?;
    let mut resp_body = String::new();
    resp.into_reader().read_to_string(&mut resp_body)
        .context("reading OSV querybatch response")?;
    let parsed: BatchResponse = serde_json::from_str(&resp_body)
        .context("parsing OSV querybatch response")?;

    // Stitch the results back to packages by index (OSV guarantees order).
    // Build a map of advisory_id → (severity?, summary?, fixed?, affected packages).
    let mut by_id: BTreeMap<String, Finding> = BTreeMap::new();
    let mut total_findings = 0usize;
    for (i, result) in parsed.results.iter().enumerate() {
        let Some(vulns) = result.vulns.as_ref() else {
            continue;
        };
        for v in vulns {
            total_findings += 1;
            let pkg = &lock.packages[i];
            by_id
                .entry(v.id.clone())
                .or_insert_with(|| Finding {
                    id: v.id.clone(),
                    severity: None,
                    summary: None,
                    advisory_url: None,
                    affected: Vec::new(),
                })
                .affected
                .push(format!("{} = \"{}\"  [{}]", pkg.name, pkg.version, pkg.scope));
        }
    }

    if by_id.is_empty() {
        println!("✓ No known vulnerabilities found.");
        return Ok(());
    }

    // Fetch details for each unique advisory.
    for finding in by_id.values_mut() {
        match agent
            .get(&format!("{OSV_VULN_DETAIL}{}", finding.id))
            .call()
        {
            Ok(r) => {
                let mut s = String::new();
                if r.into_reader().read_to_string(&mut s).is_ok() {
                    if let Ok(d) = serde_json::from_str::<VulnDetail>(&s) {
                        finding.summary = d.summary.or(d.details);
                        finding.severity = pick_severity(&d.severity);
                        finding.advisory_url = pick_advisory_url(&d.references);
                    }
                }
            }
            Err(e) => {
                eprintln!("  warning: detail lookup failed for {}: {e}", finding.id);
            }
        }
    }

    // Sort findings by severity (most severe first), then by id for stability.
    let mut sorted: Vec<&Finding> = by_id.values().collect();
    sorted.sort_by(|a, b| {
        severity_rank(b.severity.as_deref())
            .cmp(&severity_rank(a.severity.as_deref()))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut unignored = 0usize;
    for f in &sorted {
        if ignore.contains(&f.id) {
            continue;
        }
        unignored += 1;
        let sev = f.severity.as_deref().unwrap_or("UNKNOWN");
        println!();
        println!("✗ {} [{}]", f.id, sev);
        if let Some(summary) = f.summary.as_deref() {
            let one_line = summary.split_whitespace().collect::<Vec<_>>().join(" ");
            println!("  {one_line}");
        }
        if let Some(url) = f.advisory_url.as_deref() {
            println!("  advisory: {url}");
        }
        for pkg in &f.affected {
            println!("  affected: {pkg}");
        }
    }
    let ignored_count = sorted.len() - unignored;

    println!();
    if unignored == 0 {
        println!(
            "✓ {total_findings} findings ({ignored_count} ignored) — none unignored."
        );
        return Ok(());
    }
    println!(
        "{} {} vulnerabilities found across {} packages ({} ignored).",
        if unignored > 0 { "✗" } else { "✓" },
        unignored,
        total,
        ignored_count,
    );
    bail!("audit failed — {unignored} unignored vulnerabilities")
}

#[derive(serde::Serialize)]
struct BatchRequest<'a> {
    queries: &'a [BatchQuery],
}
#[derive(serde::Serialize)]
struct BatchQuery {
    package: BatchPackage,
    version: String,
}
#[derive(serde::Serialize)]
struct BatchPackage {
    ecosystem: String,
    name: String,
}
#[derive(serde::Deserialize)]
struct BatchResponse {
    results: Vec<BatchResult>,
}
#[derive(serde::Deserialize)]
struct BatchResult {
    vulns: Option<Vec<BatchVuln>>,
}
#[derive(serde::Deserialize)]
struct BatchVuln {
    id: String,
}

#[derive(serde::Deserialize)]
struct VulnDetail {
    summary: Option<String>,
    details: Option<String>,
    #[serde(default)]
    severity: Vec<SeverityEntry>,
    #[serde(default)]
    references: Vec<RefEntry>,
}
#[derive(serde::Deserialize)]
struct SeverityEntry {
    #[serde(rename = "type")]
    ty: Option<String>,
    score: Option<String>,
}
#[derive(serde::Deserialize)]
struct RefEntry {
    #[serde(rename = "type")]
    ty: Option<String>,
    url: Option<String>,
}

struct Finding {
    id: String,
    severity: Option<String>,
    summary: Option<String>,
    advisory_url: Option<String>,
    affected: Vec<String>,
}

/// Pick a single severity tag from an OSV detail. The OSV schema can carry
/// multiple entries (CVSS_V3, CVSS_V2, vendor-specific); we prefer V3, then
/// V2, then anything. The score string is the full CVSS vector; we crudely
/// extract a textual severity bucket from common patterns and otherwise
/// surface the raw vector.
fn pick_severity(entries: &[SeverityEntry]) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let pick = entries
        .iter()
        .find(|e| e.ty.as_deref() == Some("CVSS_V3"))
        .or_else(|| entries.iter().find(|e| e.ty.as_deref() == Some("CVSS_V2")))
        .or_else(|| entries.first())?;
    let score = pick.score.as_deref()?;
    // The CVSS vector includes a base score we can't easily extract without
    // an external parser; bucket by the textual marker GitHub Security
    // Advisories include in the OSV JSON when available, else expose the
    // raw vector.
    let lc = score.to_ascii_lowercase();
    if lc.contains("critical") {
        return Some("CRITICAL".into());
    }
    if lc.contains("high") {
        return Some("HIGH".into());
    }
    if lc.contains("moderate") || lc.contains("medium") {
        return Some("MODERATE".into());
    }
    if lc.contains("low") {
        return Some("LOW".into());
    }
    Some(score.to_string())
}

fn severity_rank(s: Option<&str>) -> u8 {
    match s.unwrap_or("UNKNOWN") {
        "CRITICAL" => 4,
        "HIGH" => 3,
        "MODERATE" => 2,
        "LOW" => 1,
        _ => 0,
    }
}

fn pick_advisory_url(refs: &[RefEntry]) -> Option<String> {
    // Prefer the human-readable ADVISORY entry; otherwise any URL.
    refs.iter()
        .find(|r| r.ty.as_deref() == Some("ADVISORY"))
        .and_then(|r| r.url.clone())
        .or_else(|| refs.iter().find_map(|r| r.url.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_rank_orders() {
        assert!(severity_rank(Some("CRITICAL")) > severity_rank(Some("HIGH")));
        assert!(severity_rank(Some("HIGH")) > severity_rank(Some("MODERATE")));
        assert!(severity_rank(Some("MODERATE")) > severity_rank(Some("LOW")));
        assert!(severity_rank(Some("LOW")) > severity_rank(None));
    }

    #[test]
    fn pick_severity_prefers_cvss_v3() {
        let entries = vec![
            SeverityEntry {
                ty: Some("CVSS_V2".into()),
                score: Some("AV:N/AC:M/Au:N/C:P/I:P/A:P".into()),
            },
            SeverityEntry {
                ty: Some("CVSS_V3".into()),
                score: Some("CVSS:3.1/AV:N HIGH".into()),
            },
        ];
        assert_eq!(pick_severity(&entries).as_deref(), Some("HIGH"));
    }

    #[test]
    fn pick_advisory_url_prefers_advisory_type() {
        let refs = vec![
            RefEntry { ty: Some("WEB".into()), url: Some("https://blog.example/post".into()) },
            RefEntry { ty: Some("ADVISORY".into()), url: Some("https://github.com/advisories/GHSA-xxx".into()) },
        ];
        assert_eq!(
            pick_advisory_url(&refs).as_deref(),
            Some("https://github.com/advisories/GHSA-xxx"),
        );
    }
}
