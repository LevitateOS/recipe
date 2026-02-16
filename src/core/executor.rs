//! Recipe executor - install, remove, cleanup operations
//!
//! Executes recipes using the ctx pattern where:
//! - `is_acquired(ctx)`, `is_built(ctx)`, `is_installed(ctx)` throw if phase needed
//! - `acquire(ctx)`, `build(ctx)`, `install(ctx)` return updated ctx
//! - ctx is persisted to the recipe file after each phase

use super::{build_deps, ctx, lock::acquire_recipe_lock, output};
use anyhow::{Context, Result, anyhow};
use rhai::{AST, Engine, Scope};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Parse `//! extends: <path>` from leading comments.
///
/// Only looks at comment lines at the top of the file. Stops at the first
/// non-comment, non-empty line.
fn parse_extends(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("//! extends:") {
            return Some(rest.trim().to_string());
        }
        if !trimmed.starts_with("//") {
            break;
        }
    }
    None
}

/// Resolve a base recipe path.
///
/// Tries relative to the child recipe's directory first, then the search path.
fn resolve_base_path(
    base_rel: &str,
    child_path: &Path,
    search_path: Option<&Path>,
) -> Result<PathBuf> {
    // Try relative to child
    if let Some(child_dir) = child_path.parent() {
        let candidate = child_dir.join(base_rel);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    // Try search path
    if let Some(sp) = search_path {
        let candidate = sp.join(base_rel);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    anyhow::bail!(
        "Base recipe '{}' not found (child: {}, search_path: {:?})",
        base_rel,
        child_path.display(),
        search_path
    )
}

#[derive(Debug)]
pub(crate) struct CompiledRecipe {
    pub ast: AST,
    /// The "main" recipe file (the one the user invoked).
    pub recipe_path: PathBuf,
    pub recipe_source: String,
    /// Optional base recipe from `//! extends:`.
    pub base_path: Option<PathBuf>,
    pub base_source: Option<String>,
    pub base_dir: Option<PathBuf>,
}

/// Compile a recipe with `//! extends:` resolution.
///
/// If the child recipe declares `//! extends: <base>`, the base is compiled first
/// and merged with the child AST. Child functions with the same name+arity replace
/// base functions. Top-level statements run base-first, then child.
///
/// Returns the merged AST plus the source texts/paths needed for ctx persistence.
pub(crate) fn compile_recipe(
    engine: &Engine,
    recipe_path: &Path,
    search_path: Option<&Path>,
) -> Result<CompiledRecipe> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let source = fs::read_to_string(&recipe_path)
        .with_context(|| format!("Failed to read recipe: {}", recipe_path.display()))?;

    let extends = parse_extends(&source);

    if let Some(base_rel) = extends {
        let base_path = resolve_base_path(&base_rel, &recipe_path, search_path)?;
        let base_path = base_path
            .canonicalize()
            .unwrap_or_else(|_| base_path.to_path_buf());
        let base_source = fs::read_to_string(&base_path)
            .with_context(|| format!("Failed to read base recipe: {}", base_path.display()))?;

        // Reject recursive extends
        if parse_extends(&base_source).is_some() {
            anyhow::bail!(
                "Recursive extends not supported: {} extends {} which also extends",
                recipe_path.display(),
                base_path.display()
            );
        }

        let mut base_ast = engine.compile(&base_source).map_err(|e| {
            anyhow!(
                "Failed to compile base recipe {}: {}",
                base_path.display(),
                e
            )
        })?;

        let child_ast = engine
            .compile(&source)
            .map_err(|e| anyhow!("Failed to compile recipe {}: {}", recipe_path.display(), e))?;

        // Merge: child overrides base functions, top-level runs base then child
        base_ast += child_ast;

        let base_dir = base_path.parent().map(|p| p.to_path_buf());
        Ok(CompiledRecipe {
            ast: base_ast,
            recipe_path,
            recipe_source: source,
            base_path: Some(base_path),
            base_source: Some(base_source),
            base_dir,
        })
    } else {
        let ast = engine
            .compile(&source)
            .map_err(|e| anyhow!("Failed to compile recipe: {}", e))?;
        Ok(CompiledRecipe {
            ast,
            recipe_path,
            recipe_source: source,
            base_path: None,
            base_source: None,
            base_dir: None,
        })
    }
}

/// Install a package by executing its recipe
///
/// Follows the lifecycle:
/// 1. Check is_installed(ctx) - skip if doesn't throw
/// 2. Check is_built(ctx) - skip build if doesn't throw
/// 3. Check is_acquired(ctx) - skip acquire if doesn't throw
/// 4. Execute needed phases (acquire, build, install)
/// 5. Persist ctx after each phase
///
/// Returns the final ctx map containing all recipe state.
pub fn install(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    defines: &[(String, String)],
    search_path: Option<&Path>,
) -> Result<rhai::Map> {
    install_with_autofix(
        engine,
        build_dir,
        recipe_path,
        defines,
        search_path,
        /* autofix */ None,
    )
}

#[derive(Debug)]
enum InstallAttemptError {
    Fatal(anyhow::Error),
    Phase {
        reason: &'static str,
        phase: &'static str,
        error: anyhow::Error,
    },
}

pub fn install_with_autofix(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    defines: &[(String, String)],
    search_path: Option<&Path>,
    autofix: Option<&crate::AutoFixConfig>,
) -> Result<rhai::Map> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let _lock = acquire_recipe_lock(&recipe_path)?;

    let max_attempts = autofix.map(|c| c.attempts as usize).unwrap_or(0);

    for attempt in 0..=max_attempts {
        match install_once(
            engine,
            build_dir,
            &recipe_path,
            defines,
            search_path,
            autofix,
        ) {
            Ok(ctx) => return Ok(ctx),
            Err(InstallAttemptError::Fatal(e)) => return Err(e),
            Err(InstallAttemptError::Phase {
                reason,
                phase,
                error,
            }) => {
                let Some(cfg) = autofix else {
                    return Err(error);
                };

                // Only try to autofix build/install failures (not acquire/network/etc).
                let eligible = matches!(reason, "auto.build.failure" | "auto.install.failure");
                if !eligible || attempt >= max_attempts {
                    return Err(error);
                }

                output::warning(&format!(
                    "[autofix] install failed ({reason} in {phase}); running LLM (attempt {}/{})",
                    attempt + 1,
                    max_attempts
                ));

                let failure = format!("{error:#}");
                super::autofix::run_and_apply(
                    cfg,
                    &recipe_path,
                    search_path,
                    defines,
                    reason,
                    &failure,
                )
                .with_context(|| format!("autofix failed ({reason} in {phase})"))?;
            }
        }
    }

    unreachable!("attempt loop returns on success or final failure");
}

fn install_once(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    defines: &[(String, String)],
    search_path: Option<&Path>,
    autofix: Option<&crate::AutoFixConfig>,
) -> std::result::Result<rhai::Map, InstallAttemptError> {
    let autofix_enabled = autofix.is_some();
    let mut compiled =
        compile_recipe(engine, recipe_path, search_path).map_err(InstallAttemptError::Fatal)?;
    let ast = compiled.ast.clone();

    // Derive RECIPE_DIR from the recipe file's parent directory
    let recipe_dir = recipe_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    // Set up scope with constants
    let mut scope = Scope::new();
    scope.push_constant("RECIPE_DIR", recipe_dir);
    if let Some(ref bd) = compiled.base_dir {
        scope.push_constant("BASE_RECIPE_DIR", bd.to_string_lossy().to_string());
    }
    scope.push_constant("BUILD_DIR", build_dir.to_string_lossy().to_string());
    scope.push_constant("ARCH", std::env::consts::ARCH);
    scope.push_constant("NPROC", num_cpus::get() as i64);
    scope.push_constant("RPM_PATH", std::env::var("RPM_PATH").unwrap_or_default());

    // Inject user-defined constants (from --define KEY=VALUE)
    for (key, value) in defines {
        scope.push_constant(key.as_str(), value.clone());
    }

    // Run script to populate scope (this sets up ctx)
    engine
        .run_ast_with_scope(&mut scope, &ast)
        .map_err(|e| InstallAttemptError::Fatal(anyhow!("Failed to run recipe: {}", e)))?;

    // Extract ctx from scope
    let mut ctx_map: rhai::Map = scope.get_value("ctx").ok_or_else(|| {
        InstallAttemptError::Fatal(anyhow!("Recipe missing 'let ctx = #{{...}}'"))
    })?;

    // Get package name for logging
    let name = ctx_map
        .get("name")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_else(|| {
            recipe_path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });

    // Check phases (reverse order) - throw means "needs this phase".
    //
    // IMPORTANT: `is_*` checks may also *return an updated ctx* (e.g. setting
    // `ctx.source_path` or `ctx.build_dir`). When the check passes, we must carry
    // the returned ctx forward even if the phase is skipped.
    let (needs_install, checked_ctx) = check_phase(engine, &ast, &scope, "is_installed", &ctx_map);
    ctx_map = checked_ctx;

    let mut needs_build = false;
    let mut needs_acquire = false;
    if needs_install {
        let (nb, checked_ctx) = check_phase(engine, &ast, &scope, "is_built", &ctx_map);
        needs_build = nb;
        ctx_map = checked_ctx;

        if needs_build {
            let (na, checked_ctx) = check_phase(engine, &ast, &scope, "is_acquired", &ctx_map);
            needs_acquire = na;
            ctx_map = checked_ctx;
        }
    }
    let cleanup_auto_supported = has_fn_arity(&ast, "cleanup", 2);

    if !needs_install {
        output::skip(&format!("{} already installed, skipping", name));
        return Ok(ctx_map);
    }

    // Cleanup is required in this repository: it provides the hygiene hooks needed
    // for consistent build dir behavior (especially on failure paths).
    if !cleanup_auto_supported {
        return Err(InstallAttemptError::Fatal(anyhow!(
            "{} missing required cleanup(ctx, reason) hook (found no 2-arg cleanup)",
            name
        )));
    }

    // Resolve dependencies declared in scope.
    // - `deps`: resolved before all phases (tools needed for acquire/build/install)
    // - `build_deps`: resolved only before build phase (compile-time tools)
    let deps: Vec<String> = scope
        .get_value::<rhai::Array>("deps")
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.clone().into_string().ok())
                .collect()
        })
        .unwrap_or_default();

    let build_deps: Vec<String> = scope
        .get_value::<rhai::Array>("build_deps")
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.clone().into_string().ok())
                .collect()
        })
        .unwrap_or_default();

    // Resolve `deps` immediately (needed for all phases)
    let _env_guard = if !deps.is_empty() {
        Some(
            resolve_deps(engine, build_dir, search_path, defines, &deps, autofix)
                .map_err(InstallAttemptError::Fatal)?,
        )
    } else {
        None
    };

    output::action(&format!("Installing {}", name));

    // Execute needed phases
    if needs_acquire {
        output::sub_action("acquire");
        let ctx_before = ctx_map.clone();
        match run_phase(engine, &ast, &mut scope, "acquire", ctx_map) {
            Ok(new_ctx) => {
                ctx_map = new_ctx;
                persist_ctx(
                    &mut compiled,
                    &ctx_map,
                    "Failed to persist ctx after acquire",
                )
                .map_err(InstallAttemptError::Fatal)?;

                // Hygiene: allow recipes to clean up intermediate acquire artifacts.
                if cleanup_auto_supported {
                    ctx_map = maybe_cleanup(
                        engine,
                        &ast,
                        &mut scope,
                        ctx_map,
                        "auto.acquire.success",
                        /* best_effort */ true,
                        /* require_defined */ false,
                    )
                    .map_err(InstallAttemptError::Fatal)?;
                    persist_ctx(
                        &mut compiled,
                        &ctx_map,
                        "Failed to persist ctx after cleanup",
                    )
                    .map_err(InstallAttemptError::Fatal)?;
                }
            }
            Err(e) => {
                // Best-effort failure hygiene: don't mask the original error.
                if cleanup_auto_supported {
                    let _ = maybe_cleanup(
                        engine,
                        &ast,
                        &mut scope,
                        ctx_before,
                        "auto.acquire.failure",
                        /* best_effort */ true,
                        /* require_defined */ false,
                    );
                }
                return Err(InstallAttemptError::Phase {
                    reason: "auto.acquire.failure",
                    phase: "acquire",
                    error: e,
                });
            }
        }
    }

    if needs_build && has_fn(&ast, "build") {
        // Re-check: does the recipe still need building after acquire ran?
        // (acquire may have updated ctx such that is_built now passes)
        let (still_needs_build, checked_ctx) =
            check_phase(engine, &ast, &scope, "is_built", &ctx_map);
        ctx_map = checked_ctx;

        // Resolve build_deps only when actually building
        let _build_env_guard = if still_needs_build && !build_deps.is_empty() {
            Some(
                resolve_deps(
                    engine,
                    build_dir,
                    search_path,
                    defines,
                    &build_deps,
                    autofix,
                )
                .map_err(InstallAttemptError::Fatal)?,
            )
        } else {
            None
        };

        if still_needs_build {
            output::sub_action("build");
            let ctx_before = ctx_map.clone();
            match run_phase(engine, &ast, &mut scope, "build", ctx_map) {
                Ok(new_ctx) => {
                    ctx_map = new_ctx;
                    persist_ctx(&mut compiled, &ctx_map, "Failed to persist ctx after build")
                        .map_err(InstallAttemptError::Fatal)?;

                    if cleanup_auto_supported {
                        ctx_map = maybe_cleanup(
                            engine,
                            &ast,
                            &mut scope,
                            ctx_map,
                            "auto.build.success",
                            /* best_effort */ true,
                            /* require_defined */ false,
                        )
                        .map_err(InstallAttemptError::Fatal)?;
                        persist_ctx(
                            &mut compiled,
                            &ctx_map,
                            "Failed to persist ctx after cleanup",
                        )
                        .map_err(InstallAttemptError::Fatal)?;
                    }
                }
                Err(e) => {
                    if cleanup_auto_supported {
                        let _ = maybe_cleanup(
                            engine,
                            &ast,
                            &mut scope,
                            ctx_before,
                            "auto.build.failure",
                            /* best_effort */ true,
                            /* require_defined */ false,
                        );
                    }
                    return Err(InstallAttemptError::Phase {
                        reason: "auto.build.failure",
                        phase: "build",
                        error: e,
                    });
                }
            }
        }
    }

    if needs_install {
        output::sub_action("install");
        let ctx_before = ctx_map.clone();
        match run_phase(engine, &ast, &mut scope, "install", ctx_map) {
            Ok(new_ctx) => {
                ctx_map = new_ctx;
                persist_ctx(
                    &mut compiled,
                    &ctx_map,
                    "Failed to persist ctx after install",
                )
                .map_err(InstallAttemptError::Fatal)?;

                if cleanup_auto_supported {
                    ctx_map = maybe_cleanup(
                        engine,
                        &ast,
                        &mut scope,
                        ctx_map,
                        "auto.install.success",
                        /* best_effort */ true,
                        /* require_defined */ false,
                    )
                    .map_err(InstallAttemptError::Fatal)?;
                    persist_ctx(
                        &mut compiled,
                        &ctx_map,
                        "Failed to persist ctx after cleanup",
                    )
                    .map_err(InstallAttemptError::Fatal)?;
                }
            }
            Err(e) => {
                if cleanup_auto_supported {
                    let _ = maybe_cleanup(
                        engine,
                        &ast,
                        &mut scope,
                        ctx_before,
                        "auto.install.failure",
                        /* best_effort */ true,
                        /* require_defined */ false,
                    );
                }
                return Err(InstallAttemptError::Phase {
                    reason: "auto.install.failure",
                    phase: "install",
                    error: e,
                });
            }
        }
    }

    // Anti-reward-hack / correctness: if autofix mode is enabled, require the recipe's
    // `is_installed(ctx)` check to pass after install completes.
    if autofix_enabled && has_fn_arity(&ast, "is_installed", 1) {
        if let Err(e) = engine.call_fn::<rhai::Map>(
            &mut scope.clone(),
            &ast,
            "is_installed",
            (ctx_map.clone(),),
        ) {
            // Allow best-effort failure hygiene.
            if cleanup_auto_supported {
                let _ = maybe_cleanup(
                    engine,
                    &ast,
                    &mut scope,
                    ctx_map.clone(),
                    "auto.install.failure",
                    /* best_effort */ true,
                    /* require_defined */ false,
                );
            }

            return Err(InstallAttemptError::Phase {
                reason: "auto.install.failure",
                phase: "is_installed",
                error: anyhow!("is_installed failed after install: {e}"),
            });
        }
    }

    output::success(&format!("{} installed", name));
    Ok(ctx_map)
}

/// Remove an installed package
///
/// Returns the final ctx map after removal.
pub fn remove(
    engine: &Engine,
    recipe_path: &Path,
    search_path: Option<&Path>,
) -> Result<rhai::Map> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let _lock = acquire_recipe_lock(&recipe_path)?;

    let mut compiled = compile_recipe(engine, &recipe_path, search_path)?;
    let ast = compiled.ast.clone();

    // Derive RECIPE_DIR from the recipe file's parent directory
    let recipe_dir = recipe_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    let mut scope = Scope::new();
    scope.push_constant("RECIPE_DIR", recipe_dir);
    if let Some(ref bd) = compiled.base_dir {
        scope.push_constant("BASE_RECIPE_DIR", bd.to_string_lossy().to_string());
    }

    // Run script to populate scope
    engine.run_ast_with_scope(&mut scope, &ast)?;

    let mut ctx_map: rhai::Map = scope
        .get_value("ctx")
        .ok_or_else(|| anyhow!("Recipe missing ctx"))?;

    let name = ctx_map
        .get("name")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_else(|| "package".to_string());

    if !has_fn(&ast, "remove") {
        return Err(anyhow!("{} has no remove function", name));
    }

    output::action(&format!("Removing {}", name));
    output::sub_action("remove");

    ctx_map = run_phase(engine, &ast, &mut scope, "remove", ctx_map)?;
    persist_ctx(
        &mut compiled,
        &ctx_map,
        "Failed to persist ctx after remove",
    )?;

    output::success(&format!("{} removed", name));
    Ok(ctx_map)
}

/// Clean up build artifacts
///
/// Returns the final ctx map after cleanup.
pub fn cleanup(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
) -> Result<rhai::Map> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let _lock = acquire_recipe_lock(&recipe_path)?;

    let mut compiled = compile_recipe(engine, &recipe_path, search_path)?;
    let ast = compiled.ast.clone();

    // Derive RECIPE_DIR from the recipe file's parent directory
    let recipe_dir = recipe_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    let mut scope = Scope::new();
    scope.push_constant("RECIPE_DIR", recipe_dir);
    if let Some(ref bd) = compiled.base_dir {
        scope.push_constant("BASE_RECIPE_DIR", bd.to_string_lossy().to_string());
    }
    scope.push_constant("BUILD_DIR", build_dir.to_string_lossy().to_string());

    // Run script to populate scope
    engine.run_ast_with_scope(&mut scope, &ast)?;

    let mut ctx_map: rhai::Map = scope
        .get_value("ctx")
        .ok_or_else(|| anyhow!("Recipe missing ctx"))?;

    let name = ctx_map
        .get("name")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_else(|| "package".to_string());

    if !has_fn(&ast, "cleanup") {
        return Err(anyhow!("{} has no cleanup function", name));
    }

    output::action(&format!("Cleaning up {}", name));
    output::sub_action("cleanup");

    ctx_map = maybe_cleanup(
        engine, &ast, &mut scope, ctx_map, "manual", /* best_effort */ false,
        /* require_defined */ true,
    )?;
    persist_ctx(
        &mut compiled,
        &ctx_map,
        "Failed to persist ctx after cleanup",
    )?;

    output::success(&format!("{} cleaned", name));
    Ok(ctx_map)
}

/// Check whether a phase is needed (true when the check throws).
///
/// If the check passes, returns the ctx value that the check returned.
fn check_phase(
    engine: &Engine,
    ast: &AST,
    scope: &Scope,
    fn_name: &str,
    ctx: &rhai::Map,
) -> (bool, rhai::Map) {
    if !has_fn(ast, fn_name) {
        return (true, ctx.clone()); // No check function = needs the phase.
    }

    match engine.call_fn::<rhai::Map>(&mut scope.clone(), ast, fn_name, (ctx.clone(),)) {
        Ok(new_ctx) => (false, new_ctx),
        Err(_) => (true, ctx.clone()),
    }
}

/// Run a phase function and return the updated ctx
fn run_phase(
    engine: &Engine,
    ast: &AST,
    scope: &mut Scope,
    fn_name: &str,
    ctx: rhai::Map,
) -> Result<rhai::Map> {
    engine
        .call_fn::<rhai::Map>(scope, ast, fn_name, (ctx,))
        .map_err(|e| anyhow!("{} failed: {}", fn_name, e))
}

fn maybe_cleanup(
    engine: &Engine,
    ast: &AST,
    scope: &mut Scope,
    ctx: rhai::Map,
    reason: &str,
    best_effort: bool,
    require_defined: bool,
) -> Result<rhai::Map> {
    if !has_fn(ast, "cleanup") {
        if require_defined {
            return Err(anyhow!("Recipe has no cleanup function"));
        }
        return Ok(ctx);
    }

    if !has_fn_arity(ast, "cleanup", 2) {
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

fn persist_ctx(
    compiled: &mut CompiledRecipe,
    ctx_map: &rhai::Map,
    err_ctx: &'static str,
) -> Result<()> {
    // Prefer persisting to the main recipe. If it doesn't declare ctx (common when
    // using `//! extends:` for shared logic), fall back to the base recipe.
    let (path, source) = if ctx::find_ctx_block(&compiled.recipe_source).is_some() {
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
fn resolve_deps(
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
struct EnvRestoreGuard(Vec<(String, Option<String>)>);

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

/// Check if AST has a function with the given name
fn has_fn(ast: &AST, name: &str) -> bool {
    ast.iter_functions().any(|f| f.name == name)
}

fn has_fn_arity(ast: &AST, name: &str, arity: usize) -> bool {
    ast.iter_functions()
        .any(|f| f.name == name && f.params.len() == arity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers;
    use tempfile::TempDir;

    fn create_engine() -> Engine {
        let mut engine = Engine::new();
        helpers::register_all(&mut engine);
        engine
    }

    #[test]
    fn test_install_minimal_recipe() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        let recipe_path = dir.path().join("test.rhai");
        fs::write(
            &recipe_path,
            r#"
	let ctx = #{
	    name: "test",
	    installed: false,
	};

	fn is_installed(ctx) {
	    if !ctx.installed { throw "not installed"; }
	    ctx
	}

	fn acquire(ctx) { ctx }
	fn install(ctx) {
	    ctx.installed = true;
	    ctx
	}

	fn cleanup(ctx, reason) { ctx }
	"#,
        )
        .unwrap();

        let engine = create_engine();
        let result = install(&engine, &build_dir, &recipe_path, &[], None);
        assert!(result.is_ok(), "Failed: {:?}", result);

        // Check ctx was persisted
        let content = fs::read_to_string(&recipe_path).unwrap();
        assert!(content.contains("installed: true"));
    }

    #[cfg(unix)]
    #[test]
    fn test_persist_ctx_preserves_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        let recipe_path = dir.path().join("test.rhai");
        fs::write(
            &recipe_path,
            r#"
    let ctx = #{
        name: "test",
        installed: false,
    };

    fn is_installed(ctx) {
        if !ctx.installed { throw "not installed"; }
        ctx
    }

    fn acquire(ctx) { ctx }
    fn install(ctx) {
        ctx.installed = true;
        ctx
    }

    fn cleanup(ctx, reason) { ctx }
    "#,
        )
        .unwrap();

        fs::set_permissions(&recipe_path, fs::Permissions::from_mode(0o600)).unwrap();
        let before = fs::metadata(&recipe_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(before, 0o600);

        let engine = create_engine();
        install(&engine, &build_dir, &recipe_path, &[], None).unwrap();

        let after = fs::metadata(&recipe_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(after, 0o600);
    }

    #[test]
    fn test_install_already_installed_skips() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        let recipe_path = dir.path().join("test.rhai");
        fs::write(
            &recipe_path,
            r#"
let ctx = #{
    name: "test",
};

fn is_installed(ctx) { ctx }
fn acquire(ctx) { throw "should not run"; }
fn install(ctx) { throw "should not run"; }
"#,
        )
        .unwrap();

        let engine = create_engine();
        let result = install(&engine, &build_dir, &recipe_path, &[], None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_is_check_updates_ctx_used_by_later_phases() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        // If `is_acquired(ctx)` passes and updates ctx (e.g. `ctx.source_path`),
        // that updated ctx must flow into `build(ctx)`.
        let recipe_path = dir.path().join("test.rhai");
        fs::write(
            &recipe_path,
            r#"
let ctx = #{
    name: "test",
    source_path: "",
    built: false,
    installed: false,
};

fn is_installed(ctx) { throw "not installed"; }
fn is_built(ctx) { throw "not built"; }

fn is_acquired(ctx) {
    // Simulate "already acquired" detection populating derived state.
    ctx.source_path = "/tmp/source-tree";
    ctx
}

fn build(ctx) {
    if ctx.source_path == "" { throw "missing source_path"; }
    ctx.built = true;
    ctx
}

fn install(ctx) {
    if !ctx.built { throw "not built"; }
    ctx.installed = true;
    ctx
}

fn cleanup(ctx, reason) { ctx }
"#,
        )
        .unwrap();

        let engine = create_engine();
        let result = install(&engine, &build_dir, &recipe_path, &[], None);
        assert!(result.is_ok(), "Failed: {:?}", result);
    }

    #[test]
    fn test_has_fn() {
        let engine = Engine::new();
        let ast = engine.compile("fn foo() {} fn bar(x) { x }").unwrap();
        assert!(has_fn(&ast, "foo"));
        assert!(has_fn(&ast, "bar"));
        assert!(!has_fn(&ast, "baz"));
    }

    #[test]
    fn test_parse_extends() {
        assert_eq!(
            parse_extends("//! extends: base.rhai\nlet ctx = #{};"),
            Some("base.rhai".to_string())
        );
        assert_eq!(
            parse_extends("//! extends:  linux-base.rhai \nlet ctx = #{};"),
            Some("linux-base.rhai".to_string())
        );
        assert_eq!(
            parse_extends("// comment\n//! extends: base.rhai\nlet ctx = #{};"),
            Some("base.rhai".to_string())
        );
        assert_eq!(parse_extends("let ctx = #{};"), None);
        assert_eq!(
            parse_extends("\n\n//! extends: base.rhai"),
            Some("base.rhai".to_string())
        );
        // Non-comment line before extends stops parsing
        assert_eq!(parse_extends("let x = 1;\n//! extends: base.rhai"), None);
    }

    #[test]
    fn test_extends_merges_functions() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        // Base recipe with acquire + install
        let base_path = dir.path().join("base.rhai");
        fs::write(
            &base_path,
            r#"
let ctx = #{
    name: "base",
    acquired: false,
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    ctx.acquired = true;
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}

fn cleanup(ctx, reason) { ctx }
"#,
        )
        .unwrap();

        // Child recipe that extends base, overrides install
        let child_path = dir.path().join("child.rhai");
        fs::write(
            &child_path,
            r#"//! extends: base.rhai

let ctx = #{
    name: "child",
    acquired: false,
    installed: false,
    child_ran: false,
};

fn install(ctx) {
    ctx.installed = true;
    ctx.child_ran = true;
    ctx
}

fn cleanup(ctx, reason) { ctx }
"#,
        )
        .unwrap();

        let engine = create_engine();
        let result = install(&engine, &build_dir, &child_path, &[], None);
        assert!(result.is_ok(), "Failed: {:?}", result);

        let ctx = result.unwrap();
        // Child's install ran (child_ran = true)
        assert_eq!(ctx.get("child_ran").unwrap().as_bool().unwrap(), true);
        // Base's acquire ran (acquired = true)
        assert_eq!(ctx.get("acquired").unwrap().as_bool().unwrap(), true);
        // Name should be "child" (child ctx wins)
        assert_eq!(
            ctx.get("name").unwrap().clone().into_string().unwrap(),
            "child"
        );
    }

    #[test]
    fn test_extends_recursive_rejected() {
        let dir = TempDir::new().unwrap();

        let grandparent = dir.path().join("grandparent.rhai");
        fs::write(
            &grandparent,
            "//! extends: nonexistent.rhai\nlet ctx = #{};",
        )
        .unwrap();

        let child = dir.path().join("child.rhai");
        fs::write(&child, "//! extends: grandparent.rhai\nlet ctx = #{};").unwrap();

        let engine = create_engine();
        let result = compile_recipe(&engine, &child, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_extends_base_not_found() {
        let dir = TempDir::new().unwrap();
        let child = dir.path().join("child.rhai");
        fs::write(&child, "//! extends: nonexistent.rhai\nlet ctx = #{};").unwrap();

        let engine = create_engine();
        let result = compile_recipe(&engine, &child, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_extends_persists_ctx_in_base_when_child_has_no_ctx() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        let base_path = dir.path().join("base.rhai");
        fs::write(
            &base_path,
            r#"
let ctx = #{
    name: "base",
    acquired: false,
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    ctx.acquired = true;
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}

fn cleanup(ctx, reason) { ctx }
"#,
        )
        .unwrap();

        // Child extends base but does not declare ctx; this is valid as long as
        // ctx persistence targets the file that actually contains `let ctx = #{...};`.
        let child_path = dir.path().join("child.rhai");
        fs::write(
            &child_path,
            r#"//! extends: base.rhai

fn cleanup(ctx, reason) { ctx }
"#,
        )
        .unwrap();

        let engine = create_engine();
        let result = install(&engine, &build_dir, &child_path, &[], None);
        assert!(result.is_ok(), "Failed: {:?}", result);

        // Ensure ctx was persisted into base (acquired/installed should be true).
        let persisted = fs::read_to_string(&base_path).unwrap();
        assert!(persisted.contains("acquired: true"));
        assert!(persisted.contains("installed: true"));
    }
}
