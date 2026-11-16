//! The steps Recipe does itself around a recipe's functions
//! (docs/recipe-format.md, sections 2.4 to 2.8): fetching and unpacking
//! `SOURCES`, the pins of `PACKAGES`, placing `FILES` and finishing a
//! `system` tree.
//!
//! C3: none of them is built yet. Each function below is the one place its
//! step goes; until C3 it does nothing when the recipe declares nothing for
//! it, and otherwise fails the run, naming what is not built yet, so a
//! recipe that needs it never looks done.

use anyhow::{Result, bail};
use rhai::Map;

use crate::declarations::Declared;

/// The outputs as the engine placed them: each declared name, its kind and
/// its path in the worker.
pub(crate) type Outputs = Vec<(String, crate::needs::PathKind, std::path::PathBuf)>;

/// The download step: every `SOURCES` entry for this system and the pins of
/// `PACKAGES` into the download store (section 2.4, 2.6 step 1).
///
/// C3: fetch them (offline cache first), check each SHA-256, unpack archives
/// (a single top folder left out unless `strip: false`), check out `git`
/// sources, and return `src`: each source name to its path, and with
/// `PACKAGES` and `build(src, out)` the list `src.packages`.
pub(crate) fn download(declared: &Declared, host_libc: &str) -> Result<Map> {
    let selected: Vec<&str> = declared
        .sources
        .iter()
        .filter(|(_, source)| source.libc.is_none_or(|libc| libc.as_str() == host_libc))
        .map(|(name, _)| name.as_str())
        .collect();
    if !selected.is_empty() || declared.packages.is_some() {
        bail!(
            "fetching SOURCES and the pins of PACKAGES is not built yet in C2 (this engine \
             runs only recipes without SOURCES and PACKAGES)"
        );
    }
    Ok(Map::new())
}

/// Steps 2 of section 2.6, for a recipe with `PACKAGES` and no `build`:
/// unpack every pinned package into the one output tree, in pin order, `var/`
/// handled as the pins say.
///
/// C3: unpack natively, without scripts (`target_extract_rpm`,
/// `target_extract_apk`).
pub(crate) fn unpack_packages(declared: &Declared, outputs: &Outputs) -> Result<()> {
    let _ = outputs;
    if declared.packages.is_some() && !declared.functions.contains(&"build") {
        bail!("unpacking the pinned packages into the tree is not built yet in C2");
    }
    Ok(())
}

/// `FILES` (section 2.5): move each named file or folder of the unpacked
/// sources into the output, with its mode.
///
/// C3: place them (copied for a source file named twice; modes of a `system`
/// output into its target metadata).
pub(crate) fn place_files(declared: &Declared, src: &Map, outputs: &Outputs) -> Result<()> {
    let _ = (src, outputs);
    if !declared.files.is_empty() {
        bail!("placing FILES is not built yet in C2");
    }
    Ok(())
}

/// The sources `configure(tree, src)` receives: the recipe's other `SOURCES`,
/// without the packages (section 2.6, answer 5).
///
/// C3: as `download` returns them for a recipe with `PACKAGES`.
pub(crate) fn configure_sources(src: &Map) -> Map {
    let mut other = src.clone();
    other.remove(crate::declarations::PACKAGES_SOURCE);
    other
}

/// Finish every `system` output (section 2.8): remove the folders a system
/// package does not own, then finalize the tree (owners resolved in the root
/// it is written into, `var/` as state seeds, `ACCOUNTS`).
///
/// C3: do it with `crate::target` and `crate::accounts`.
pub(crate) fn finish_system(declared: &Declared, outputs: &Outputs) -> Result<()> {
    let _ = declared;
    if outputs.iter().any(|(name, ..)| name == "system") {
        bail!(
            "finishing a system output (folders, target metadata, ACCOUNTS) is not built yet in C2"
        );
    }
    Ok(())
}
