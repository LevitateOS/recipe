use crate::core::executor::CompiledRecipe;
use crate::core::{build_deps, ctx, output};
use anyhow::{Context, Result, anyhow};
use rhai::{AST, Engine, Scope};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Check whether a step is needed (true when the check throws).
///
/// If the check passes, returns the ctx value that the check returned.
pub(crate) fn check_phase_with_reason(
    engine: &Engine,
    ast: &AST,
    scope: &Scope,
    fn_name: &str,
    ctx: &rhai::Map,
) -> (bool, rhai::Map, Option<String>) {
    if !crate::core::runner::has_fn(ast, fn_name) {
        return (
            true,
            ctx.clone(),
            Some("check function is missing (default: needs work)".to_string()),
        );
    }

    match engine.call_fn::<rhai::Map>(&mut scope.clone(), ast, fn_name, (ctx.clone(),)) {
        Ok(new_ctx) => (false, new_ctx, None),
        Err(e) => {
            let msg = format!("{e}");
            (true, ctx.clone(), Some(msg))
        }
    }
}

/// Run recipe cleanup hook and return the updated ctx.
pub(crate) fn maybe_cleanup(
    engine: &Engine,
    ast: &AST,
    scope: &mut Scope,
    ctx: rhai::Map,
    reason: &str,
    best_effort: bool,
    require_defined: bool,
) -> Result<rhai::Map> {
    if !crate::core::runner::has_fn(ast, "cleanup") {
        if require_defined {
            return Err(anyhow!("Recipe has no cleanup function"));
        }
        return Ok(ctx);
    }

    if !crate::core::runner::has_fn_arity(ast, "cleanup", 2) {
        return Err(anyhow!(
            "cleanup hook must be cleanup(ctx, reason) (found non-2-arg cleanup)"
        ));
    }

    let result =
        engine.call_fn::<rhai::Map>(scope, ast, "cleanup", (ctx.clone(), reason.to_string()));

    match result {
        Ok(ctx) => Ok(ctx),
        Err(e) if best_effort => {
            output::warning(&format!("cleanup hook failed (reason={reason}): {e}"));
            Ok(ctx)
        }
        Err(e) => Err(anyhow!("cleanup failed: {}", e)),
    }
}

pub(crate) fn persist_ctx(
    compiled: &mut CompiledRecipe,
    ctx_map: &rhai::Map,
    err_ctx: &'static str,
) -> Result<()> {
    // Prefer persisting to the main recipe. If it doesn't declare ctx (common when
    // using `//! extends:` for shared logic), fall back to the base recipe.
    let (path, source): (&Path, &mut String) =
        if ctx::find_ctx_block(&compiled.recipe_source).is_some() {
            (&compiled.recipe_path, &mut compiled.recipe_source)
        } else if let (Some(base_path), Some(base_source)) =
            (&compiled.base_path, &mut compiled.base_source)
        {
            if ctx::find_ctx_block(base_source).is_some() {
                (base_path, base_source)
            } else {
                return Err(anyhow!(
                    "ctx block not found in recipe {} or base {:?}",
                    compiled.recipe_path.display(),
                    base_path.display()
                ))
                .with_context(|| err_ctx);
            }
        } else {
            return Err(anyhow!(
                "ctx block not found in recipe {} (no base recipe)",
                compiled.recipe_path.display()
            ))
            .with_context(|| err_ctx);
        };

    *source = ctx::persist(source, ctx_map).with_context(|| err_ctx)?;

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("Recipe path has no parent directory: {}", path.display()))
        .with_context(|| err_ctx)?;

    // Preserve existing file permissions where possible.
    let existing_perms = fs::metadata(path).map(|m| m.permissions()).ok();

    // Write to a sibling temp file so the final rename is atomic on Unix.
    let mut tmp = tempfile::Builder::new()
        .prefix(".recipe-ctx.")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| err_ctx)?;

    tmp.as_file_mut()
        .write_all(source.as_bytes())
        .with_context(|| err_ctx)?;
    tmp.as_file().sync_all().with_context(|| err_ctx)?;

    if let Some(perms) = existing_perms {
        // Best-effort; failure should still abort to avoid surprising permission changes.
        fs::set_permissions(tmp.path(), perms).with_context(|| err_ctx)?;
    }

    // Keep temp file on drop so we can rename it into place. If the rename fails,
    // explicitly remove it to avoid leaving junk behind.
    let (_f, tmp_path) = tmp.keep().with_context(|| err_ctx)?;
    drop(_f);

    if let Err(e) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e).with_context(|| err_ctx);
    }

    // Ensure the directory entry update is durable on Unix.
    #[cfg(unix)]
    {
        use std::fs::File;
        File::open(parent)
            .and_then(|d| d.sync_all())
            .with_context(|| err_ctx)?;
    }
    Ok(())
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
) -> Result<EnvRestoreGuard> {
    let original_path = std::env::var("PATH").unwrap_or_default();
    let mut resolver =
        build_deps::BuildDepsResolver::new(engine, build_dir, search_path, defines, autofix);
    let tools_prefix = resolver.resolve_and_install(dep_names)?;

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
