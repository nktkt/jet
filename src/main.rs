use anyhow::Result;
use clap::Parser;

mod build_cache;
mod classes;
mod cli;
mod cmd;
mod coord;
mod jar;
mod javac;
mod lockfile;
mod manifest;
mod pom;
mod registry;
mod resolver;
mod template;
mod toolchain;
mod tools;
mod validate;
mod workspace;

use cli::{Cli, Command, JdkCommand, WatchAction};
use cmd::add::{AddArgs, cmd_add};
use cmd::build::{BuildArgs, cmd_build};
use cmd::clean::cmd_clean;
use cmd::init::{InitArgs, cmd_init};
use cmd::new::{NewArgs, cmd_new};
use cmd::package::{PackageArgs, cmd_package};
use cmd::publish::{PublishArgs, cmd_publish};
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
        Command::Build { release, resolve, package, jobs, no_cache } => cmd_build(BuildArgs {
            release,
            force_resolve: resolve,
            package,
            jobs,
            no_cache,
            check_only: false,
        }),
        Command::Check { package, jobs } => {
            cmd::check::cmd_check(cmd::check::CheckArgs { package, jobs })
        }
        Command::Run { args } => cmd_run(RunArgs { args }),
        Command::Test { filter } => cmd_test(TestArgs { filter }),
        Command::Add { coord, no_verify, dev } => {
            cmd_add(AddArgs { coord, no_verify, dev })
        }
        Command::Clean => cmd_clean(),
        Command::Jdk { action } => match action {
            JdkCommand::List => cmd::jdk::cmd_list(),
            JdkCommand::Install { version, vendor } => cmd::jdk::cmd_install(version, vendor),
        },
        Command::Package { uber, native } => cmd_package(PackageArgs { uber, native }),
        Command::Publish { dry_run, no_sign } => {
            cmd_publish(PublishArgs { dry_run, no_sign })
        }
        Command::Outdated { allow_prereleases } => {
            cmd::outdated::cmd_outdated(cmd::outdated::OutdatedArgs {
                allow_prereleases,
            })
        }
        Command::Update { coord, allow_prereleases } => {
            cmd::update::cmd_update(cmd::update::UpdateArgs {
                coord,
                allow_prereleases,
            })
        }
        Command::Remove { coord, dev } => {
            cmd::remove::cmd_remove(cmd::remove::RemoveArgs { coord, dev })
        }
        Command::Search { query, limit } => {
            cmd::search::cmd_search(cmd::search::SearchArgs { query, limit })
        }
        Command::Completions { shell } => {
            cmd::completions::cmd_completions(cmd::completions::CompletionsArgs { shell })
        }
        Command::Info { coord } => {
            cmd::info::cmd_info(cmd::info::InfoArgs { coord })
        }
        Command::Fmt { check } => {
            cmd::fmt::cmd_fmt(cmd::fmt::FmtArgs { check })
        }
        Command::Audit { ignore } => {
            cmd::audit::cmd_audit(cmd::audit::AuditArgs { ignore })
        }
        Command::Licenses { detail, scope } => {
            cmd::licenses::cmd_licenses(cmd::licenses::LicensesArgs { detail, scope })
        }
        Command::Tree { scope } => {
            cmd::tree::cmd_tree(cmd::tree::TreeArgs { scope })
        }
        Command::Why { coord } => cmd::why::cmd_why(cmd::why::WhyArgs { coord }),
        Command::Plugins => cmd::plugin::cmd_list(),
        Command::Doctor => cmd::doctor::cmd_doctor(),
        Command::Watch { action } => {
            let action = match action.unwrap_or(WatchAction::Build) {
                WatchAction::Build => cmd::watch::WatchAction::Build,
                WatchAction::Check => cmd::watch::WatchAction::Check,
                WatchAction::Test => cmd::watch::WatchAction::Test,
                WatchAction::Run { args } => cmd::watch::WatchAction::Run { args },
            };
            cmd::watch::cmd_watch(cmd::watch::WatchArgs { action })
        }
        Command::Import { force } => {
            cmd::import::cmd_import(cmd::import::ImportArgs { force })
        }
        Command::External(args) => {
            let mut iter = args.into_iter();
            let name = iter
                .next()
                .map(|s| s.to_string_lossy().into_owned())
                .ok_or_else(|| anyhow::anyhow!("empty external command"))?;
            cmd::plugin::dispatch_external(&name, iter.collect())
        }
    }
}
