use anyhow::Result;
use clap::Parser;

mod cli;
mod cmd;
mod coord;
mod javac;
mod lockfile;
mod manifest;
mod resolver;
mod template;
mod validate;

use cli::{Cli, Command};
use cmd::add::{AddArgs, cmd_add};
use cmd::build::{BuildArgs, cmd_build};
use cmd::clean::cmd_clean;
use cmd::init::{InitArgs, cmd_init};
use cmd::new::{NewArgs, cmd_new};
use cmd::run::{RunArgs, cmd_run};

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
        Command::Build { release, resolve } => cmd_build(BuildArgs {
            release,
            force_resolve: resolve,
        }),
        Command::Run { args } => cmd_run(RunArgs { args }),
        Command::Test { filter } => todo!("run tests (filter={filter:?})"),
        Command::Add { coord, no_verify } => cmd_add(AddArgs { coord, no_verify }),
        Command::Clean => cmd_clean(),
        Command::Package => todo!("build distributable jar"),
        Command::Publish => todo!("publish to Maven repository"),
    }
}
