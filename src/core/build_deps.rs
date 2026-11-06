//! Dependency recipes run through the same lifecycle as their parent.
//!
//! All recipes of one invocation share the tools prefix. Each dependency gets
//! its own work directory under the main recipe's, and cycles are detected by
//! canonical recipe path before evaluation.

use super::executor::{EngineError, ExecutionContext, Load, install as install_recipe};
use super::output;
use crate::core::events::HookStatus;
use anyhow::{Context, Result, anyhow};
use rhai::Engine;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Install the named dependencies, then put the tools prefix on the child
/// environment. The environment lasts as long as the returned guard; `None`
/// when there is nothing to install.
pub(crate) fn install(
    engine: &Engine,
    exec: &mut ExecutionContext<'_>,
    load: &Load<'_>,
    names: &[String],
) -> Result<Option<crate::process::EnvironmentGuard>> {
    if names.is_empty() {
        return Ok(None);
    }
    for name in names {
        let recipe = locate(name, exec.current_recipe(), load.search_path)?;
        let build_dir = work_dir(exec.work_root(), &recipe);
        std::fs::create_dir_all(&build_dir)
            .with_context(|| format!("creating dependency work directory for '{name}'"))?;
        output::hook_event(
            name,
            "dependency.prepare",
            HookStatus::Requested,
            "executing shared recipe lifecycle",
        );
        let dependency = Load {
            build_dir: &build_dir,
            ..*load
        };
        // A dependency's phase failure is the parent's dependency failure;
        // autofix patches only the main recipe.
        install_recipe(engine, exec, &recipe, &dependency)
            .map_err(|(EngineError::Recipe(error) | EngineError::Phase { error, .. })| error)
            .with_context(|| format!("preparing dependency recipe '{name}'"))?;
        output::hook_event(name, "dependency.ready", HookStatus::Success, "tool dependency prepared");
    }
    Ok(Some(tools_environment()?))
}

/// The dependency recipe file, by the same rule as bases and modules.
fn locate(name: &str, parent: Option<&Path>, search_path: Option<&Path>) -> Result<PathBuf> {
    let file = crate::recipes::dependency_file(name).map_err(anyhow::Error::msg)?;
    let found = parent
        .and_then(|parent| crate::recipes::find_related(&file, parent, search_path))
        .ok_or_else(|| {
            anyhow!("Dependency recipe '{file}' not found (parent: {parent:?}, search_path: {search_path:?})")
        })?;
    found
        .canonicalize()
        .with_context(|| format!("resolving dependency '{name}'"))
}

/// `<work root>/.deps/<stem>-<digest>`: the path digest separates equal
/// recipe names in different directories.
fn work_dir(work_root: &Path, recipe: &Path) -> PathBuf {
    let digest = format!("{:x}", Sha256::digest(recipe.as_os_str().as_encoded_bytes()));
    let stem = recipe.file_stem().unwrap_or_default().to_string_lossy();
    work_root.join(".deps").join(format!("{stem}-{}", &digest[..16]))
}

/// PATH gains the tool bin dirs (once), plus the variables that the tools
/// LevitateOS recipes build into the prefix need to find their data.
fn tools_environment() -> Result<crate::process::EnvironmentGuard> {
    let current = crate::process::environment("PATH").unwrap_or_default();
    let current: Vec<PathBuf> = std::env::split_paths(&current).collect();
    let mut path: Vec<PathBuf> = crate::domain::tool_bin_dirs()
        .map(PathBuf::from)
        .filter(|dir| !current.contains(dir))
        .collect();
    path.extend(current);
    let mut values = vec![(std::ffi::OsString::from("PATH"), std::env::join_paths(path)?)];
    let usr = Path::new(crate::domain::TOOLS).join("usr");
    for (key, value) in [
        ("BISON_PKGDATADIR", usr.join("share/bison")),
        ("M4", usr.join("bin/m4")),
        ("LIBRARY_PATH", usr.join("lib64")),
        ("C_INCLUDE_PATH", usr.join("include")),
        ("CPLUS_INCLUDE_PATH", usr.join("include")),
        ("PKG_CONFIG_PATH", usr.join("lib64/pkgconfig")),
    ] {
        if value.exists() {
            values.push((key.into(), value.into_os_string()));
        }
    }
    Ok(crate::process::push_environment(values))
}
