//! `jet completions <shell>` — emit a shell-completion script to stdout.
//!
//! Uses `clap_complete::generate` against the live `Cli` parser, so the
//! script always reflects the subcommands and flags this build of jet
//! actually supports (no second source of truth to drift). Install with
//! e.g. `jet completions zsh > "$fpath[1]/_jet"` for zsh.

use anyhow::Result;
use clap::CommandFactory as _;
use clap_complete::Shell;

use crate::cli::Cli;

pub struct CompletionsArgs {
    pub shell: Shell,
}

pub fn cmd_completions(args: CompletionsArgs) -> Result<()> {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    clap_complete::generate(args.shell, &mut cmd, name, &mut std::io::stdout());
    Ok(())
}
