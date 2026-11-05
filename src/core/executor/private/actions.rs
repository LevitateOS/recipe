use crate::core::{output, runner};
use anyhow::{Result, anyhow};
use rhai::Engine;
use std::path::Path;

use super::context::{CheckpointPoint, ExecutionContext};
use super::reporting::{report_phase_failure, report_phase_success};
use crate::core::executor::compile_recipe;

pub(crate) fn remove_with_context(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    persist_state_enabled: bool,
    exec: &mut ExecutionContext<'_>,
) -> Result<rhai::Map> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let mut compiled = compile_recipe(engine, &recipe_path, search_path)?;
    let ast = compiled.ast.clone();

    let scope = crate::core::invocation::scope(
        &compiled.recipe_path,
        compiled.base_dir.as_deref(),
        Some(build_dir),
        exec.tools_prefix_override(),
        defines,
        &exec.options(),
    );

    let mut scope = crate::core::invocation::Runtime::new(
        engine,
        &ast,
        scope,
        "remove",
        persist_state_enabled,
    )?;
    exec.restore_runtime(&compiled.recipe_path, &mut scope)?;

    let mut ctx_map: rhai::Map = scope
        .scope()
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

    ctx_map = runner::run_phase(&ast, &mut scope, "remove", ctx_map).inspect_err(|e| {
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
        persist_state_enabled,
    )?;

    output::success(&format!("{} removed", name));
    Ok(ctx_map)
}

pub(crate) fn cleanup_with_context(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    reason: &str,
    persist_state_enabled: bool,
    exec: &mut ExecutionContext<'_>,
) -> Result<rhai::Map> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let mut compiled = compile_recipe(engine, &recipe_path, search_path)?;
    let ast = compiled.ast.clone();
    let has_cleanup = runner::cleanup_available(&ast)?;

    let scope = crate::core::invocation::scope(
        &compiled.recipe_path,
        compiled.base_dir.as_deref(),
        Some(build_dir),
        exec.tools_prefix_override(),
        defines,
        &exec.options(),
    );

    let mut scope = crate::core::invocation::Runtime::new(
        engine,
        &ast,
        scope,
        "cleanup",
        persist_state_enabled,
    )?;
    exec.restore_runtime(&compiled.recipe_path, &mut scope)?;

    let mut ctx_map: rhai::Map = scope
        .scope()
        .get_value("ctx")
        .ok_or_else(|| anyhow!("Recipe missing ctx"))?;

    let name = ctx_map
        .get("name")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_else(|| "package".to_string());

    if !has_cleanup {
        output::hook_event(&name, "cleanup", "skipped", "recipe has no cleanup hook");
        output::skip(&format!("{name} has no cleanup hook; nothing to clean"));
        return Ok(ctx_map);
    }

    output::action(&format!("Cleaning up {}", name));
    output::sub_action("cleanup");
    output::hook_event(&name, "cleanup", "running", "executing recipe hook");

    ctx_map = runner::run_cleanup(&ast, &mut scope, ctx_map, reason, false).inspect_err(|e| {
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
        persist_state_enabled,
    )?;

    output::success(&format!("{} cleaned", name));
    Ok(ctx_map)
}

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

    let compiled = compile_recipe(engine, &recipe_path, search_path)?;
    let ast = compiled.ast.clone();

    let scope = crate::core::invocation::scope(
        &compiled.recipe_path,
        compiled.base_dir.as_deref(),
        Some(build_dir),
        exec.tools_prefix_override(),
        defines,
        &exec.options(),
    );

    let mut scope =
        crate::core::invocation::Runtime::new(engine, &ast, scope, check_name, exec.resumable())?;
    exec.restore_runtime(&compiled.recipe_path, &mut scope)?;

    let ctx_map: rhai::Map = scope
        .scope()
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

    let checked_ctx = runner::verify_phase(&mut scope, check_name, &ctx_map)
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
