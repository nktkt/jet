use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "jet",
    version,
    about = "A fast, modern Java build tool",
    long_about = "jet is a Cargo/Bun-inspired build tool for the JVM. \
                  It aims to replace Maven/Gradle with a simpler config (jet.toml), \
                  faster builds, and a friendlier CLI."
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
    /// Compile the current project
    Build {
        /// Build in release mode
        #[arg(long)]
        release: bool,
        /// Re-resolve dependencies, ignoring jet.lock
        #[arg(long)]
        resolve: bool,
    },
    /// Compile and run the project's main class
    Run {
        /// Arguments forwarded to the Java program
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Run the project's tests
    Test {
        /// Test name filter
        filter: Option<String>,
    },
    /// Add a dependency to jet.toml
    Add {
        /// Coordinate in `group:artifact:version` form
        coord: String,
        /// Skip remote existence check
        #[arg(long)]
        no_verify: bool,
    },
    /// Remove build artifacts
    Clean,
    /// Build a distributable artifact (jar / uber-jar)
    Package,
    /// Publish the project to a Maven-compatible repository
    Publish,
}
