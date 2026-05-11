//! Maven Central registry helpers.
//!
//! Used by `jet outdated` / `jet update` to discover the newest published
//! version of a `group:artifact` without fetching its POM. Hits the public
//! Solr endpoint at `https://search.maven.org/solrsearch/select` and reads
//! `response.docs[0].latestVersion`.

use std::io::Read as _;
use std::time::Duration;

use anyhow::{Context, Result, bail};

const SEARCH_URL: &str = "https://search.maven.org/solrsearch/select";

/// Return the highest published version of `group:artifact` from Maven Central
/// that satisfies the prerelease policy:
///   - `allow_prereleases = true`: any version qualifies (including `-M*`,
///     `-RC*`, `-alpha*`, `-beta*`, `-SNAPSHOT`, `-pre*`, `-dev*`, `-PR*`).
///   - `allow_prereleases = false`: only stable versions qualify.
///
/// Returns `None` when the coord isn't on Central, or when every published
/// version was filtered out by the policy. Caller decides how to handle that.
///
/// Uses the Solr `gav` core which returns the full version list (vs. the
/// default core's rolled-up `latestVersion` field — that pre-filtered single
/// value gives us no way to skip prereleases).
pub fn latest_version(
    group: &str,
    artifact: &str,
    allow_prereleases: bool,
) -> Result<Option<String>> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(15))
        .user_agent(concat!("jet/", env!("CARGO_PKG_VERSION")))
        .build();

    // `core=gav` enumerates every version doc. Maven Central returns them
    // newest-first, but we sort defensively in case the order ever shifts.
    // `rows=20` is enough headroom to skip a handful of prereleases on top
    // and still find a stable; very few coords go more than 20 prereleases
    // deep without a stable in between.
    let q = format!("g:{group} AND a:{artifact}");
    let url = format!(
        "{SEARCH_URL}?q={}&core=gav&rows=20&wt=json",
        urlencode(&q),
    );
    let resp = match agent.get(&url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => {
            bail!("HTTP {code} from {url}");
        }
        Err(e) => bail!("HTTP error {e} for {url}"),
    };
    let mut body = String::new();
    resp.into_reader().read_to_string(&mut body)
        .with_context(|| format!("reading response from {url}"))?;
    let parsed: SolrGavResponse = serde_json::from_str(&body)
        .with_context(|| format!("parsing search.maven.org response for {group}:{artifact}"))?;

    let mut versions: Vec<String> = parsed
        .response
        .docs
        .into_iter()
        .map(|d| d.v)
        .filter(|v| allow_prereleases || !is_prerelease(v))
        .collect();
    // Descending by our comparator: a > b iff is_newer(b, a).
    versions.sort_by(|a, b| {
        if is_newer(b, a) {
            std::cmp::Ordering::Less
        } else if is_newer(a, b) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    Ok(versions.into_iter().next())
}

#[derive(serde::Deserialize)]
struct SolrGavResponse {
    response: SolrGavInner,
}

#[derive(serde::Deserialize)]
struct SolrGavInner {
    docs: Vec<SolrGavDoc>,
}

#[derive(serde::Deserialize)]
struct SolrGavDoc {
    v: String,
}

/// Recognized pre-release qualifiers, case-insensitive. A version is
/// "pre-release" iff one of its `-`/`.`-separated segments matches any of
/// these prefixes. We match prefixes so `5.13.0-M3`, `7.0-alpha`,
/// `1.0.0-beta2`, `2.0-rc1`, etc. all flag as prerelease.
const PRERELEASE_QUALIFIERS: &[&str] = &[
    "m", "rc", "alpha", "beta", "snapshot", "pre", "dev", "pr", "preview",
];

/// True if `v` looks like a pre-release (milestone, RC, alpha/beta, snapshot,
/// or any vendor-y `-dev`/`-pre`/`-PR*` qualifier). The check is conservative:
/// segments that start with a digit or are pure numeric are never prerelease,
/// so `33.4.8-jre` stays stable (`jre` is a classifier suffix, not a status).
pub fn is_prerelease(v: &str) -> bool {
    for segment in v.split(['.', '-', '+', '_']) {
        if segment.is_empty() {
            continue;
        }
        // Skip leading-digit segments — those are normal version numbers.
        if segment.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        let lc = segment.to_ascii_lowercase();
        // The `jre` / `android` / `jdk8` classifier-style suffixes on Guava
        // and similar coords are not prereleases. Whitelist the known
        // non-prerelease qualifier prefixes by exclusion instead of
        // enumeration: we only flag prerelease when the segment *matches*
        // one of our known prerelease prefixes.
        for q in PRERELEASE_QUALIFIERS {
            // Prefix match: `m3`, `rc1`, `alpha`, `beta-2` all match.
            if lc.starts_with(q) {
                let rest = &lc[q.len()..];
                // Avoid false positives: `m` alone matches our `m` prefix
                // but `media` would also match. Require the rest to be empty,
                // a digit, or a separator-tail.
                if rest.is_empty() || rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    return true;
                }
            }
        }
    }
    false
}

/// One row in the result of a Maven Central free-text search.
pub struct SearchHit {
    pub group: String,
    pub artifact: String,
    pub latest_version: String,
}

#[derive(serde::Deserialize)]
struct SolrSearchResponse {
    response: SolrSearchInner,
}

#[derive(serde::Deserialize)]
struct SolrSearchInner {
    docs: Vec<SolrSearchDoc>,
}

#[derive(serde::Deserialize)]
struct SolrSearchDoc {
    g: String,
    a: String,
    #[serde(rename = "latestVersion")]
    latest_version: String,
}

/// Free-text search across Maven Central. `query` is passed through as the
/// Solr `q` parameter; users can write plain words (`guava`), field-scoped
/// queries (`g:com.google.guava`), or full coords (`g:com.google.guava AND
/// a:guava`). Results are capped at `limit`.
pub fn search(query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(15))
        .user_agent(concat!("jet/", env!("CARGO_PKG_VERSION")))
        .build();

    let url = format!(
        "{SEARCH_URL}?q={}&rows={limit}&wt=json",
        urlencode(query),
    );
    let resp = match agent.get(&url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => bail!("HTTP {code} from {url}"),
        Err(e) => bail!("HTTP error {e} for {url}"),
    };
    let mut body = String::new();
    resp.into_reader().read_to_string(&mut body)
        .with_context(|| format!("reading response from {url}"))?;
    let parsed: SolrSearchResponse = serde_json::from_str(&body)
        .with_context(|| format!("parsing search.maven.org response for `{query}`"))?;
    Ok(parsed
        .response
        .docs
        .into_iter()
        .map(|d| SearchHit {
            group: d.g,
            artifact: d.a,
            latest_version: d.latest_version,
        })
        .collect())
}

/// Minimal URL-encoder for Solr query strings. The chars we actually pass in
/// (`a-z 0-9 . _ -` plus `:` and space inside the q value) need just a few
/// replacements; we don't need a full percent-encoder.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            ' ' => out.push_str("%20"),
            ':' => out.push_str("%3A"),
            '+' => out.push_str("%2B"),
            other if other.is_ascii_alphanumeric()
                || other == '-'
                || other == '_'
                || other == '.'
                || other == '~' =>
            {
                out.push(other);
            }
            other => {
                let mut buf = [0u8; 4];
                for b in other.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }
    out
}

/// Compare two Maven version strings just enough to detect "is `b` newer
/// than `a`". Numeric segments are compared numerically, others
/// lexicographically; trailing qualifiers (`-jre`, `-SNAPSHOT`, `-RC1`) are
/// compared after the numeric prefix. Good enough for `jet outdated`'s
/// "is there something to suggest?" decision; not a full Maven version
/// comparator.
pub fn is_newer(a: &str, b: &str) -> bool {
    let aa = split_version(a);
    let bb = split_version(b);
    let mut ai = aa.iter();
    let mut bi = bb.iter();
    loop {
        match (ai.next(), bi.next()) {
            (None, None) => return false,
            (None, Some(_)) => return true, // 1.0 < 1.0.1
            (Some(_), None) => return false,
            (Some(x), Some(y)) => {
                let cmp = match (x.parse::<u64>().ok(), y.parse::<u64>().ok()) {
                    (Some(xn), Some(yn)) => xn.cmp(&yn),
                    _ => x.as_str().cmp(y.as_str()),
                };
                match cmp {
                    std::cmp::Ordering::Less => return true,
                    std::cmp::Ordering::Greater => return false,
                    std::cmp::Ordering::Equal => {}
                }
            }
        }
    }
}

fn split_version(s: &str) -> Vec<String> {
    s.split(['.', '-', '+', '_'])
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_simple() {
        assert!(is_newer("1.0.0", "1.0.1"));
        assert!(is_newer("1.0.0", "2.0.0"));
        assert!(is_newer("1.0", "1.0.1"));
        assert!(!is_newer("2.0.0", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.0"));
    }

    #[test]
    fn newer_with_qualifier() {
        assert!(is_newer("33.0.0-jre", "33.4.0-jre"));
        assert!(is_newer("5.10.0", "5.10.2"));
        assert!(!is_newer("33.4.0-jre", "33.0.0-jre"));
    }

    #[test]
    fn prerelease_detection() {
        assert!(is_prerelease("5.13.0-M3"));
        assert!(is_prerelease("5.13.0-m3"));
        assert!(is_prerelease("7.0-alpha"));
        assert!(is_prerelease("1.0.0-beta2"));
        assert!(is_prerelease("2.0-RC1"));
        assert!(is_prerelease("2.0-rc1"));
        assert!(is_prerelease("1.0-SNAPSHOT"));
        assert!(is_prerelease("0.9-snapshot"));
        assert!(is_prerelease("3.0.0-pre1"));
        assert!(is_prerelease("4.0-dev"));
    }

    #[test]
    fn prerelease_excludes_classifier_suffix() {
        // These are stable. `jre`, `android`, `jdk8` are classifier-style
        // suffixes, not prerelease qualifiers.
        assert!(!is_prerelease("33.4.8-jre"));
        assert!(!is_prerelease("33.0.0-android"));
        assert!(!is_prerelease("1.0.0"));
        assert!(!is_prerelease("5.10.2"));
        assert!(!is_prerelease("2.0.13"));
    }

    #[test]
    fn urlencode_solr_q() {
        assert_eq!(
            urlencode("g:com.google.guava AND a:guava"),
            "g%3Acom.google.guava%20AND%20a%3Aguava"
        );
    }
}
