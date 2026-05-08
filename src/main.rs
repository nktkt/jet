use anyhow::Result;
use clap::Parser;

mod cli;
mod cmd;
mod template;
mod validate;

use cli::{Cli, Command};
use cmd::init::{InitArgs, cmd_init};
use cmd::new::{NewArgs, cmd_new};

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::New { path, name, java, no_vcs } => cmd_new(NewArgs {
            path,
            name,
            java,
            vcs: !no_vcs,
        }),
        Command::Init { name, java, no_vcs } => cmd_init(InitArgs {
            name,
            java,
            vcs: !no_vcs,
        }),
        Command::Build { release } => todo!("compile project (release={release})"),
        Command::Run { args } => todo!("run main class with args: {args:?}"),
        Command::Test { filter } => todo!("run tests (filter={filter:?})"),
        Command::Add { coord } => todo!("add dependency: {coord}"),
        Command::Clean => todo!("remove target directory"),
        Command::Package => todo!("build distributable jar"),
        Command::Publish => todo!("publish to Maven repository"),
    }
}
