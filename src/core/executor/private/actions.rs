use crate::core::{output, runner};
use anyhow::{Result, anyhow};
use rhai::Engine;
use std::path::Path;

use super::context::{CheckpointPoint, ExecutionContext};
use super::reporting::{report_phase_failure, report_phase_success};
use super::state::maybe_cleanup;
use crate::core::executor::compile_recipe;

pub(crate) fn remove(
    engine: &Engine,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    persist_ctx_enabled: bool,
) -> Result<rhai::Map> {
    let mut exec = ExecutionContext::host();
    remove_with_context(
        engine,
        recipe_path,
        search_path,
        defines,
        persist_ctx_enabled,
        &mut exec,
    )
}

/// Same removal flow with caller-provided lock ownership and checkpoint sink.
///
/// Worker seam (R02 stub): the host coordinator holds the recipe lock and
/// receives the checkpoint through `exec` instead of this operation taking
/// a second lock or rewriting recipe sources itself.
pub(crate) fn remove_with_context(
    engine: &Engine,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    persist_ctx_enabled: bool,
    exec: &mut ExecutionContext<'_>,
) -> Result<rhai::Map> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let _lock = exec.acquire_lock(&recipe_path)?;

    let mut compiled = compile_recipe(engine, &recipe_path, search_path)?;
    let ast = compiled.ast.clone();

    // Removal's compatibility entry point receives its build directory in defines.
    let build_dir = defines
        .iter()
        .rev()
        .find(|(key, _)| key == "BUILD_DIR")
        .map(|(_, value)| Path::new(value));
    let mut scope = crate::core::invocation::scope(
        &compiled.recipe_path,
        compiled.base_dir.as_deref(),
        build_dir,
        None,
        defines,
    );

    // Run script to populate scope
    engine.run_ast_with_scope(&mut scope, &ast)?;

    let mut ctx_map: rhai::Map = scope
        .get_value("ctx")
        .ok_or_else(|| anyhow!("Recipe missing ctx"))?;

    let name = ctx_map
        .get("name")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_else(|| "package".to_string());

    if !runner::has_fn(&ast, "remove") {
        output::hook_event(&name, "remove", "missing", "required remove hook missing");
        return Err(anyhow!("{} has no remove function", name));
    }

    output::action(&format!("Removing {}", name));
    output::sub_action("remove");
    output::hook_event(&name, "remove", "running", "executing recipe hook");

    ctx_map = runner::run_phase(engine, &ast, &mut scope, "remove", ctx_map).inspect_err(|e| {
        report_phase_failure(&name, "remove", e);
    })?;
    report_phase_success(&name, "remove");
    exec.persist_checkpoint(
        &mut compiled,
        &ctx_map,
        CheckpointPoint {
            phase: "remove",
            reason: "phase:remove",
        },
        persist_ctx_enabled,
    )?;

    output::success(&format!("{} removed", name));
    Ok(ctx_map)
}

/// Clean up build artifacts
///
/// Returns the final ctx map after cleanup.
pub(crate) fn cleanup(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    reason: &str,
    persist_ctx_enabled: bool,
) -> Result<rhai::Map> {
    let mut exec = ExecutionContext::host();
    cleanup_with_context(
        engine,
        build_dir,
        recipe_path,
        search_path,
        defines,
        reason,
        persist_ctx_enabled,
        &mut exec,
    )
}

/// Same cleanup flow with caller-provided lock ownership and checkpoint sink.
///
/// Worker seam (R02 stub): the host coordinator holds the recipe lock and
/// receives the checkpoint through `exec` instead of this operation taking
/// a second lock or rewriting recipe sources itself.
pub(crate) fn cleanup_with_context(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    reason: &str,
    persist_ctx_enabled: bool,
    exec: &mut ExecutionContext<'_>,
) -> Result<rhai::Map> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let _lock = exec.acquire_lock(&recipe_path)?;

    let mut compiled = compile_recipe(engine, &recipe_path, search_path)?;
    let ast = compiled.ast.clone();

    let mut scope = crate::core::invocation::scope(
        &compiled.recipe_path,
        compiled.base_dir.as_deref(),
        Some(build_dir),
        None,
        defines,
    );

    // Run script to populate scope
    engine.run_ast_with_scope(&mut scope, &ast)?;

    let mut ctx_map: rhai::Map = scope
        .get_value("ctx")
        .ok_or_else(|| anyhow!("Recipe missing ctx"))?;

    let name = ctx_map
        .get("name")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_else(|| "package".to_string());

    if !runner::has_fn(&ast, "cleanup") {
        output::hook_event(&name, "cleanup", "missing", "required cleanup hook missing");
        return Err(anyhow!("{} has no cleanup function", name));
    }

    output::action(&format!("Cleaning up {}", name));
    output::sub_action("cleanup");
    output::hook_event(&name, "cleanup", "running", "executing recipe hook");

    ctx_map = maybe_cleanup(
        engine, &ast, &mut scope, ctx_map, reason, /* best_effort */ false,
        /* require_defined */ true,
    )
    .inspect_err(|e| {
        report_phase_failure(&name, "cleanup", e);
    })?;
    report_phase_success(&name, "cleanup");
    exec.persist_checkpoint(
        &mut compiled,
        &ctx_map,
        CheckpointPoint {
            phase: "cleanup",
            reason: "manual",
        },
        persist_ctx_enabled,
    )?;

    output::success(&format!("{} cleaned", name));
    Ok(ctx_map)
}

/// Execute `is_installed(ctx)` manually.
///
/// Returns the updated ctx map on success.
pub(crate) fn is_installed(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
) -> Result<rhai::Map> {
    run_check(
        engine,
        build_dir,
        recipe_path,
        search_path,
        defines,
        "is_installed",
    )
}

/// Execute `is_built(ctx)` manually.
///
/// Returns the updated ctx map on success.
pub(crate) fn is_built(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
) -> Result<rhai::Map> {
    run_check(
        engine,
        build_dir,
        recipe_path,
        search_path,
        defines,
        "is_built",
    )
}

/// Execute `is_acquired(ctx)` manually.
///
/// Returns the updated ctx map on success.
pub(crate) fn is_acquired(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
) -> Result<rhai::Map> {
    run_check(
        engine,
        build_dir,
        recipe_path,
        search_path,
        defines,
        "is_acquired",
    )
}

pub(crate) fn run_check(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    check_name: &str,
) -> Result<rhai::Map> {
    let mut exec = ExecutionContext::host();
    run_check_with_context(
        engine,
        build_dir,
        recipe_path,
        search_path,
        defines,
        check_name,
        &mut exec,
    )
}

/// Same manual-check flow with caller-provided lock ownership.
///
/// Worker seam (R02 stub): checks report no checkpoints (they never persist),
/// but the lock must still be coordinator-owned so the worker does not take
/// a second lock.
pub(crate) fn run_check_with_context(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    check_name: &str,
    exec: &mut ExecutionContext<'_>,
) -> Result<rhai::Map> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let _lock = exec.acquire_lock(&recipe_path)?;

    let compiled = compile_recipe(engine, &recipe_path, search_path)?;
    let ast = compiled.ast.clone();

    let mut scope = crate::core::invocation::scope(
        &compiled.recipe_path,
        compiled.base_dir.as_deref(),
        Some(build_dir),
        None,
        defines,
    );

    // Run script to populate scope
    engine.run_ast_with_scope(&mut scope, &ast)?;

    let ctx_map: rhai::Map = scope
        .get_value("ctx")
        .ok_or_else(|| anyhow!("Recipe missing ctx"))?;

    let name = ctx_map
        .get("name")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_else(|| "package".to_string());

    if !runner::has_fn(&ast, check_name) {
        output::hook_event(
            &name,
            &format!("check.{check_name}"),
            "missing",
            "required check function missing",
        );
        output::error(&format!(
            "{name} is missing required check function `{check_name}(ctx)`",
        ));
        output::detail(
            "Action: define this check function and return an updated ctx map when the check passes.",
        );
        return Err(anyhow!("{} has no {} function", name, check_name));
    }

    output::action(&format!("Checking recipe {}", name));
    output::sub_action(&format!("{check_name} check"));
    output::hook_event(
        &name,
        &format!("check.{check_name}"),
        "manual",
        "manual check requested",
    );

    let checked_ctx = engine
        .call_fn::<rhai::Map>(&mut scope, &ast, check_name, (ctx_map,))
        .map_err(|e| {
            output::hook_event(&name, &format!("check.{check_name}"), "failed", &format!("{e}"));
            output::error(&format!("{name}: {check_name} check failed"));
            output::detail(&format!("  reason: {e}"));
            output::detail("  action: check that function and return ctx on success path; rerun with RECIPE_TRACE_HELPERS=1.");
            anyhow!("{check_name} failed: {e}")
        })?;

    output::success(&format!("{}: {} check complete", name, check_name));
    output::hook_event(
        &name,
        &format!("check.{check_name}"),
        "success",
        "manual check complete",
    );
    Ok(checked_ctx)
}
