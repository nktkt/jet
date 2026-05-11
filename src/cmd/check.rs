//! `jet check` — typecheck without producing artifacts in `target/classes/`.
//!
//! Reuses the full `do_build` pipeline (dep resolution, JAR fetch, javac
//! invocation) but flips `check_only = true` so:
//!   - class files land under `target/check/classes/` (separate from
//!     `target/classes/` so a subsequent `jet build` doesn't see them as
//!     stale outputs to mistrust).
//!   - the content-addressed build cache is bypassed in BOTH directions —
//!     no lookup (we want the typecheck to actually run when fingerprints
//!     match a prior *build*; the user asked for `check`, give them
//!     fresh diagnostics) and no store (typecheck output is throwaway).
//!   - per-mode incremental state lives in `target/jet-info/check.json`
//!     so `jet build` and `jet check` keep independent up-to-date flags.
//!
//! When the project hasn't changed since the last `jet check`, the call
//! still short-circuits via the per-mode `check.json` — fast iteration
//! loops stay fast.

use anyhow::Result;

use crate::cmd::build::{BuildArgs, cmd_build};

pub struct CheckArgs {
    pub package: Option<String>,
    pub jobs: Option<usize>,
}

pub fn cmd_check(args: CheckArgs) -> Result<()> {
    cmd_build(BuildArgs {
        release: false,
        force_resolve: false,
        package: args.package,
        jobs: args.jobs,
        no_cache: false,
        check_only: true,
    })
}
