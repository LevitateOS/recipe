//! The steps Recipe does itself around a recipe's functions
//! (docs/recipe-format.md, sections 2.4 to 2.8 and 6.1): fetching `SOURCES`
//! and the pins of `PACKAGES` into the download store ([`download`]),
//! unpacking the sources ([`unpack_sources`]), unpacking the pinned packages
//! into a group's tree ([`unpack_packages`]), placing `FILES`
//! ([`place_files`]) and finishing a `system` tree ([`finish_system`]).
//!
//! They work on the worker's own paths (`crate::domain::local`) through
//! `crate::helpers::confine`, as the helpers do: confined below the run's
//! directories in a host run, plain file operations in a Ginux domain.

mod fetch;
mod place;
mod system;

pub(crate) use fetch::{Fetched, download, src_map, unpack_sources};
pub(crate) use place::place_files;
pub(crate) use system::{finish_system, unpack_packages};

use rhai::{EvalAltResult, Map};
use std::path::Path;

/// The outputs as the engine placed them: each declared name, its kind and
/// its path in the worker.
pub(crate) type Outputs = Vec<(String, crate::needs::PathKind, std::path::PathBuf)>;

/// The sources `configure(tree, src)` receives: the recipe's other `SOURCES`,
/// without the packages (section 2.6, answer 5).
pub(crate) fn configure_sources(src: &Map) -> Map {
    let mut other = src.clone();
    other.remove(crate::declarations::PACKAGES_SOURCE);
    other
}

/// A helper's error as the engine's.
fn helper(error: Box<EvalAltResult>) -> anyhow::Error {
    anyhow::anyhow!("{error}")
}

/// A worker path as the text the helpers take.
fn text(path: &Path) -> anyhow::Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow::anyhow!("{} is not a UTF-8 path", path.display()))
}

/// Remove what `path` names (a file, a link or a folder), if anything.
fn remove_any(path: &Path) -> anyhow::Result<()> {
    use crate::helpers::confine;
    match confine::lstat(path) {
        Ok(metadata) if metadata.is_dir() => confine::remove_dir_all(path),
        Ok(_) => confine::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
    .map_err(|error| anyhow::anyhow!("cannot remove {}: {error}", path.display()))
}
