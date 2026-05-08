use anyhow::Result;
use clap::Parser;

mod classes;
mod cli;
mod cmd;
mod coord;
mod jar;
mod javac;
mod lockfile;
mod manifest;
mod resolver;
mod template;
mod validate;
mod workspace;

use cli::{Cli, Command};
use cmd::add::{AddArgs, cmd_add};
use cmd::build::{BuildArgs, cmd_build};
use cmd::clean::cmd_clean;
use cmd::init::{InitArgs, cmd_init};
use cmd::new::{NewArgs, cmd_new};
use cmd::package::{PackageArgs, cmd_package};
use cmd::run::{RunArgs, cmd_run};
use cmd::test::{TestArgs, cmd_test};

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
        Command::Build { release, resolve, package, jobs } => cmd_build(BuildArgs {
            release,
            force_resolve: resolve,
            package,
            jobs,
        }),
        Command::Run { args } => cmd_run(RunArgs { args }),
        Command::Test { filter } => cmd_test(TestArgs { filter }),
        Command::Add { coord, no_verify, dev } => {
            cmd_add(AddArgs { coord, no_verify, dev })
        }
        Command::Clean => cmd_clean(),
        Command::Package { uber } => cmd_package(PackageArgs { uber }),
        Command::Publish => todo!("publish to Maven repository"),
    }
}
