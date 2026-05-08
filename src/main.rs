use anyhow::Result;
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
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new jet project in a new directory
    New {
        /// Project directory to create
        path: String,
    },
    /// Initialize a jet project in the current directory
    Init,
    /// Compile the current project
    Build {
        /// Build in release mode
        #[arg(long)]
        release: bool,
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
    },
    /// Remove build artifacts
    Clean,
    /// Build a distributable artifact (jar / uber-jar)
    Package,
    /// Publish the project to a Maven-compatible repository
    Publish,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::New { path } => todo!("scaffold a new project at {path}"),
        Command::Init => todo!("initialize jet.toml in the current directory"),
        Command::Build { release } => todo!("compile project (release={release})"),
        Command::Run { args } => todo!("run main class with args: {args:?}"),
        Command::Test { filter } => todo!("run tests (filter={filter:?})"),
        Command::Add { coord } => todo!("add dependency: {coord}"),
        Command::Clean => todo!("remove target directory"),
        Command::Package => todo!("build distributable jar"),
        Command::Publish => todo!("publish to Maven repository"),
    }
}
