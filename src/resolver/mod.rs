//! Maven dependency resolution: POM fetching, parsing, transitive resolution.

pub mod fetcher;
pub mod pom;
pub mod resolve;

pub use fetcher::{Fetcher, default_repos};
#[allow(unused_imports)]
pub use pom::Pom;
pub use resolve::{Resolution, Resolved, resolve};
