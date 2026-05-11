use std::ffi::OsString;

use clap::{Parser, Subcommand};
use clap_complete::Shell;

#[derive(Subcommand, Clone)]
pub enum WatchAction {
    /// Re-run `jet build` on every change (default)
    Build,
    /// Re-run `jet check` (typecheck only) on every change — fastest feedback
    Check,
    /// Re-run `jet test` on every change
    Test,
    /// Spawn the main class on each successful build; kill + respawn on change
    Run {
        /// Arguments forwarded to the Java program
        #[arg(last = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum JdkCommand {
    /// List jet-managed JDKs installed under ~/.jet/jdks/
    List,
    /// Download and install a JDK (default vendor: temurin)
    Install {
        /// Java major version (e.g. 21, 25)
        version: u32,
        /// Distribution vendor; defaults to `temurin`
        #[arg(long, default_value = "temurin")]
        vendor: String,
    },
}

#[derive(Parser)]
#[command(
    name = "jet",
    version,
    about = "A fast, modern Java build tool",
    long_about = "jet is a Cargo/Bun-inspired build tool for the JVM. \
                  It aims to replace Maven/Gradle with a simpler config (jet.toml), \
                  faster builds, and a friendlier CLI.",
    allow_external_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create a new jet project in a new directory
    New {
        /// Project directory to create
        path: String,
        /// Override the package name (defaults to the dir name)
        #[arg(long)]
        name: Option<String>,
        /// Java version to target (default: 21)
        #[arg(long, default_value_t = 21)]
        java: u32,
        /// Do not run `git init` in the new project
        #[arg(long)]
        no_vcs: bool,
    },
    /// Initialize a jet project in the current directory
    Init {
        /// Override the package name (defaults to the cwd name)
        #[arg(long)]
        name: Option<String>,
        /// Java version to target (default: 21)
        #[arg(long, default_value_t = 21)]
        java: u32,
        /// Do not run `git init` (also skipped if .git/ already exists)
        #[arg(long)]
        no_vcs: bool,
    },
    /// Typecheck the current project without producing class outputs in `target/classes/`
    Check {
        /// Limit to a specific workspace member (and its path-dep ancestors)
        #[arg(short = 'p', long)]
        package: Option<String>,
        /// Number of parallel build jobs (default: available_parallelism)
        #[arg(short = 'j', long)]
        jobs: Option<usize>,
    },
    /// Compile the current project
    Build {
        /// Build in release mode
        #[arg(long)]
        release: bool,
        /// Re-resolve dependencies, ignoring jet.lock
        #[arg(long)]
        resolve: bool,
        /// Limit to a specific workspace member (and its path-dep ancestors)
        #[arg(short = 'p', long)]
        package: Option<String>,
        /// Number of parallel build jobs (default: available_parallelism)
        #[arg(short = 'j', long)]
        jobs: Option<usize>,
        /// Skip the content-addressed build cache
        #[arg(long)]
        no_cache: bool,
    },
    /// Compile and run the project's main class
    Run {
        /// Arguments forwarded to the Java program
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Run the project's tests
    Test {
        /// Filter: ClassName, ClassName::method, com.pkg.*, or substring
        filter: Option<String>,
    },
    /// Add a dependency to jet.toml
    Add {
        /// Coordinate in `group:artifact:version` form
        coord: String,
        /// Skip remote existence check
        #[arg(long)]
        no_verify: bool,
        /// Add to [dev-dependencies] instead of [dependencies]
        #[arg(long)]
        dev: bool,
    },
    /// Remove build artifacts
    Clean,
    /// Diagnose project + environment configuration
    Doctor,
    /// Import a Maven `pom.xml` into a new `jet.toml`
    Import {
        /// Overwrite an existing jet.toml
        #[arg(long)]
        force: bool,
    },
    /// Watch source files and re-run build (default) or test on change
    Watch {
        #[command(subcommand)]
        action: Option<WatchAction>,
    },
    /// Print the resolved dependency tree
    Tree {
        /// Restrict to a single scope (compile, runtime, test, dev)
        #[arg(long)]
        scope: Option<String>,
    },
    /// Explain why a coord ended up in the resolved graph
    Why {
        /// `group:artifact` or `group:artifact:version`
        coord: String,
    },
    /// List jet plugins (jet-* binaries) discoverable on PATH
    Plugins,
    /// Build a distributable JAR (thin by default; --uber for self-contained)
    Package {
        /// Build a self-contained uber JAR (bundles all main dependencies)
        #[arg(long)]
        uber: bool,
        /// Also produce a GraalVM native image (implies --uber)
        #[arg(long)]
        native: bool,
    },
    /// Manage jet-installed JDK toolchains under ~/.jet/jdks/
    Jdk {
        #[command(subcommand)]
        action: JdkCommand,
    },
    /// Dispatched to `jet-<name>` on PATH (git-style plugin)
    #[command(external_subcommand)]
    External(Vec<OsString>),
    /// Publish the project to a Maven-compatible repository
    Publish {
        /// Stage everything under `target/publish/` instead of uploading
        #[arg(long)]
        dry_run: bool,
        /// Skip GPG signing (overrides `[publish].sign`)
        #[arg(long)]
        no_sign: bool,
    },
    /// Check Maven Central for newer dependency versions
    Outdated {
        /// Consider pre-release versions (-M*, -RC*, -alpha*, -beta*,
        /// -SNAPSHOT, -pre, -dev). Off by default — stable pins stay
        /// on stable. Deps whose current pin is itself a prerelease
        /// always allow prereleases regardless of this flag.
        #[arg(long)]
        allow_prereleases: bool,
    },
    /// Bump dependencies to their latest Maven Central version
    Update {
        /// `group:artifact` (or `group:artifact:version`) to restrict the
        /// update to a single dep. Omit to update every dep in jet.toml.
        coord: Option<String>,
        /// Same semantics as `jet outdated --allow-prereleases`.
        #[arg(long)]
        allow_prereleases: bool,
    },
    /// Remove a dependency from jet.toml and refresh jet.lock
    Remove {
        /// Coordinate in `group:artifact` (or `group:artifact:version`) form
        coord: String,
        /// Remove from [dev-dependencies] instead of [dependencies]
        #[arg(long)]
        dev: bool,
    },
    /// Search Maven Central for a coordinate
    Search {
        /// Free-text query, or a Solr field expression (e.g. `g:io.netty`)
        query: String,
        /// Max number of rows to print
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Print a shell-completion script for the given shell
    Completions {
        /// Target shell: bash, zsh, fish, elvish, powershell
        shell: Shell,
    },
    /// Show Maven Central metadata for a coordinate
    Info {
        /// `group:artifact` (latest stable) or `group:artifact:version`
        coord: String,
    },
    /// Format Java source via google-java-format
    Fmt {
        /// Verify formatting without writing — non-zero exit if any file would change
        #[arg(long)]
        check: bool,
    },
}
