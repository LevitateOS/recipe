use super::context::ExecutionContext;
use crate::core::build_deps;
use crate::core::invocation::Runtime;
use anyhow::Result;
use rhai::Engine;
use std::path::Path;

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
/// Returns a guard for invocation-owned child environment settings.
pub(crate) fn resolve_deps(
    engine: &Engine,
    build_dir: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    dep_names: &[String],
    persist_enabled: bool,
    exec: &mut ExecutionContext<'_>,
) -> Result<crate::helpers::internal::cmd::EnvironmentGuard> {
    let original_path = crate::helpers::internal::cmd::environment("PATH").unwrap_or_default();
    let resolver = build_deps::BuildDepsResolver::new(engine, build_dir, search_path, defines);
    let tools_prefix = resolver.resolve_with_context(dep_names, persist_enabled, exec)?;
    let mut paths = vec![
        tools_prefix.join("usr/bin"),
        tools_prefix.join("usr/sbin"),
        tools_prefix.join("bin"),
        tools_prefix.join("sbin"),
    ];
    paths.extend(std::env::split_paths(&original_path));
    let path = std::env::join_paths(paths)?;
    let mut values = vec![(std::ffi::OsString::from("PATH"), path)];
    let tools_usr = tools_prefix.join("usr");
    for (key, value) in [
        ("BISON_PKGDATADIR", tools_usr.join("share/bison")),
        ("M4", tools_usr.join("bin/m4")),
        ("LIBRARY_PATH", tools_usr.join("lib64")),
        ("C_INCLUDE_PATH", tools_usr.join("include")),
        ("CPLUS_INCLUDE_PATH", tools_usr.join("include")),
        ("PKG_CONFIG_PATH", tools_usr.join("lib64/pkgconfig")),
    ] {
        if value.exists() {
            values.push((key.into(), value.into_os_string()));
        }
    }
    Ok(crate::helpers::internal::cmd::push_environment(values))
}
