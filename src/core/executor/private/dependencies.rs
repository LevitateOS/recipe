//! Dependencies have already completed in separate coordinator-owned instances.
//! This worker only verifies their immutable result bindings and selects tools.
use anyhow::{Result, ensure};
use std::path::PathBuf;

/// Check that every named dependency was prepared, then put their tools on PATH.
pub(crate) fn select(names: &[String]) -> Result<Option<crate::process::EnvironmentGuard>> {
    if names.is_empty() {
        return Ok(None);
    }
    for name in names {
        crate::recipes::dependency_file(name).map_err(anyhow::Error::msg)?;
        let path = crate::domain::local(crate::domain::INPUTS)
            .join(crate::domain::DEPENDENCY_INPUT)
            .join(name);
        ensure!(
            std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()),
            "dependency {name} has no separately prepared result; same-domain dependency execution is forbidden"
        );
    }
    Ok(Some(tools_environment()?))
}

/// PATH gains the tool bin dirs (once), plus the variables that the tools
/// LevitateOS recipes build into the prefix need to find their data.
fn tools_environment() -> Result<crate::process::EnvironmentGuard> {
    let current = crate::process::environment("PATH").unwrap_or_default();
    let current: Vec<PathBuf> = std::env::split_paths(&current).collect();
    let mut path: Vec<PathBuf> = crate::domain::tool_bin_dirs()
        .map(|dir| crate::domain::local(&dir))
        .filter(|dir| !current.contains(dir))
        .collect();
    path.extend(current);
    let mut values = vec![(
        std::ffi::OsString::from("PATH"),
        std::env::join_paths(path)?,
    )];
    let usr = crate::domain::local(crate::domain::TOOLS).join("usr");
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
