//! `jet search <query>` — free-text search across Maven Central, à la
//! `cargo search` / `bun pm search`.
//!
//! Hands the query straight to the Solr endpoint (see `registry::search`),
//! so callers can write either plain words (`guava`) or field-scoped
//! expressions (`g:com.google.guava`, `g:io.netty AND a:netty-handler`).
//! Prints up to `--limit N` rows in `group:artifact = "version"` form so the
//! output can be pasted into `[dependencies]` directly.

use anyhow::{Result, bail};

use crate::registry;

pub struct SearchArgs {
    pub query: String,
    pub limit: usize,
}

pub fn cmd_search(args: SearchArgs) -> Result<()> {
    if args.query.trim().is_empty() {
        bail!("search query is empty");
    }
    let hits = registry::search(&args.query, args.limit)?;
    if hits.is_empty() {
        println!("No matches on Maven Central for `{}`.", args.query);
        return Ok(());
    }
    let key_width = hits
        .iter()
        .map(|h| h.group.len() + h.artifact.len() + 1)
        .max()
        .unwrap_or(0);
    for h in &hits {
        let key = format!("{}:{}", h.group, h.artifact);
        println!(
            "  \"{key}\"{pad} = \"{ver}\"",
            pad = " ".repeat(key_width.saturating_sub(key.len())),
            ver = h.latest_version,
        );
    }
    Ok(())
}
