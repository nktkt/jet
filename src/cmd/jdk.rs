//! `jet jdk list | install <version>` — manage jet-installed JDK toolchains.

use anyhow::Result;

use crate::manifest::ToolchainConfig;
use crate::toolchain;

pub fn cmd_list() -> Result<()> {
    let installed = toolchain::list_installed()?;
    if installed.is_empty() {
        println!("  No JDKs installed under ~/.jet/jdks/");
        println!("  Try: jet jdk install 21");
        return Ok(());
    }
    println!("  Installed JDKs:");
    for jdk in installed {
        println!("    {} {}  ({})", jdk.vendor, jdk.version, jdk.home.display());
    }
    Ok(())
}

pub fn cmd_install(version: u32, vendor: String) -> Result<()> {
    let tc = ToolchainConfig { version, vendor };
    toolchain::install(&tc)?;
    Ok(())
}
