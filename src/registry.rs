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

/// Return the latest published version of `group:artifact` from Maven Central,
/// or `None` when the coordinate isn't on Central (likely an internal repo
/// dep — caller decides how to handle that).
pub fn latest_version(group: &str, artifact: &str) -> Result<Option<String>> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(15))
        .user_agent(concat!("jet/", env!("CARGO_PKG_VERSION")))
        .build();

    // Solr query — we ask for the rolled-up doc per artifact, which has the
    // `latestVersion` field; rows=1 keeps the response small.
    let q = format!("g:{group} AND a:{artifact}");
    let url = format!("{SEARCH_URL}?q={}&rows=1&wt=json", urlencode(&q));
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
    let parsed: SolrResponse = serde_json::from_str(&body)
        .with_context(|| format!("parsing search.maven.org response for {group}:{artifact}"))?;
    Ok(parsed.response.docs.into_iter().next().map(|d| d.latest_version))
}

#[derive(serde::Deserialize)]
struct SolrResponse {
    response: SolrResponseInner,
}

#[derive(serde::Deserialize)]
struct SolrResponseInner {
    docs: Vec<SolrDoc>,
}

#[derive(serde::Deserialize)]
struct SolrDoc {
    #[serde(rename = "latestVersion")]
    latest_version: String,
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
    fn urlencode_solr_q() {
        assert_eq!(
            urlencode("g:com.google.guava AND a:guava"),
            "g%3Acom.google.guava%20AND%20a%3Aguava"
        );
    }
}
