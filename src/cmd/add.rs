use anyhow::{Context, Result, bail};

use crate::coord::Coord;
use crate::manifest::{MANIFEST_FILENAME, Manifest};

pub struct AddArgs {
    pub coord: String,
    /// Skip remote existence verification.
    pub no_verify: bool,
    /// Add to [dev-dependencies] instead of [dependencies].
    pub dev: bool,
}

pub fn cmd_add(args: AddArgs) -> Result<()> {
    let coord: Coord = args
        .coord
        .parse()
        .with_context(|| format!("parsing coordinate `{}`", args.coord))?;

    let cwd = std::env::current_dir()?;
    let root = Manifest::find_root(&cwd)?;
    let manifest = Manifest::load(&root)?;

    let key = format!("{}:{}", coord.group, coord.artifact);
    let table = if args.dev { "dev-dependencies" } else { "dependencies" };
    let already_present = if args.dev {
        manifest.dev_dependencies.contains_key(&key)
    } else {
        manifest.dependencies.contains_key(&key)
    };
    if already_present {
        bail!(
            "dependency `{key}` is already present in [{table}] \
             (use `jet remove {key}` first if you want to change the version)"
        );
    }

    if !args.no_verify {
        verify_exists(&coord)?;
    }

    let manifest_path = root.join(MANIFEST_FILENAME);
    Manifest::add_dep_to_table(&manifest_path, table, &key, &coord.version)?;
    println!("  Added `{key} = \"{}\"` to [{table}]", coord.version);

    // Re-resolve to refresh jet.lock.
    use crate::cmd::build::{BuildArgs, do_build};
    println!("  Resolving dependencies…");
    do_build(BuildArgs { release: false, force_resolve: true, package: None })?;

    Ok(())
}

fn verify_exists(coord: &Coord) -> Result<()> {
    let url = coord.pom_url("https://repo1.maven.org/maven2");
    match ureq::head(&url).call() {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(404, _)) => bail!(
            "`{coord}` not found on Maven Central (404 at {url}). \
             Check the coordinate or pass --no-verify."
        ),
        Err(e) => {
            eprintln!("  warning: could not verify {coord} ({e}); proceeding");
            Ok(())
        }
    }
}
