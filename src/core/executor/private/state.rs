use super::context::ExecutionContext;
use crate::core::build_deps;
use crate::core::invocation::Runtime;
use anyhow::Result;
use rhai::Engine;
use std::path::{Path, PathBuf};

/// Evaluate readiness, preserving discovered context and genuine errors.
///
/// If the check passes, returns the ctx value that the check returned.
pub(crate) fn check_phase_with_reason(
    scope: &mut Runtime<'_>,
    fn_name: &str,
    ctx: &rhai::Map,
) -> Result<(bool, rhai::Map, Option<String>)> {
    match scope.check(fn_name, ctx)? {
        crate::core::invocation::Readiness::Ready(ctx) => Ok((false, ctx, None)),
        crate::core::invocation::Readiness::NeedsWork(ctx, reason) => Ok((true, ctx, Some(reason))),
    }
}

/// Resolve dependency recipes, install tools, and set up PATH + env vars.
/// Returns an RAII guard that restores the environment on drop.
pub(crate) fn resolve_deps(
    engine: &Engine,
    build_dir: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    dep_names: &[String],
    autofix: Option<&crate::AutoFixConfig>,
    persist_enabled: bool,
    exec: &mut ExecutionContext<'_>,
) -> Result<EnvRestoreGuard> {
    let original_path = std::env::var("PATH").unwrap_or_default();
    let resolver =
        build_deps::BuildDepsResolver::new(engine, build_dir, search_path, defines, autofix);
    let tools_prefix = resolver.resolve_with_context(dep_names, persist_enabled, exec)?;

    // Safety: we're single-threaded during recipe execution
    unsafe {
        std::env::set_var(
            "PATH",
            format!(
                "{}:{}:{}:{}:{}",
                tools_prefix.join("usr/bin").display(),
                tools_prefix.join("usr/sbin").display(),
                tools_prefix.join("bin").display(),
                tools_prefix.join("sbin").display(),
                original_path
            ),
        );
    }

    // Save current env for restoration
    let env_keys = [
        "BISON_PKGDATADIR",
        "M4",
        "LIBRARY_PATH",
        "C_INCLUDE_PATH",
        "CPLUS_INCLUDE_PATH",
        "PKG_CONFIG_PATH",
    ];
    let mut saved_env: Vec<(String, Option<String>)> =
        vec![("PATH".to_string(), Some(original_path))];
    for key in &env_keys {
        saved_env.push((key.to_string(), std::env::var(key).ok()));
    }

    // Set data/lib paths so relocated RPM tools find their files
    let tools_usr = tools_prefix.join("usr");
    let env_fixups: &[(&str, PathBuf)] = &[
        ("BISON_PKGDATADIR", tools_usr.join("share/bison")),
        ("M4", tools_usr.join("bin/m4")),
        ("LIBRARY_PATH", tools_usr.join("lib64")),
        ("C_INCLUDE_PATH", tools_usr.join("include")),
        ("CPLUS_INCLUDE_PATH", tools_usr.join("include")),
        ("PKG_CONFIG_PATH", tools_usr.join("lib64/pkgconfig")),
    ];
    for (key, val) in env_fixups {
        if val.exists() {
            unsafe {
                std::env::set_var(key, val);
            }
        }
    }

    Ok(EnvRestoreGuard(saved_env))
}

/// RAII guard that restores environment variables on drop (even on early error return).
pub(crate) struct EnvRestoreGuard(Vec<(String, Option<String>)>);

impl Drop for EnvRestoreGuard {
    fn drop(&mut self) {
        // Safety: single-threaded recipe execution
        for (key, val) in &self.0 {
            unsafe {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}
