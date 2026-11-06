//! The single-step operations: remove, manual cleanup and a manual check.

use super::context::ExecutionContext;
use super::loaded::{EngineError, Load, with_recipe};
use super::reporting::{report_phase_failure, report_phase_success};
use crate::core::events::HookStatus;
use crate::core::output;
use anyhow::anyhow;
use rhai::{Engine, Map};
use std::path::Path;

pub(crate) fn remove(
    engine: &Engine,
    exec: &mut ExecutionContext<'_>,
    recipe: &Path,
    load: &Load<'_>,
) -> Result<Map, EngineError> {
    with_recipe(engine, exec, recipe, load, |recipe, exec| {
        let name = recipe.name.clone();
        if !recipe.runtime.has_fn("remove") {
            output::hook_event(&name, "remove", HookStatus::Missing, "required remove hook missing");
            return Err(anyhow!("{name} has no remove function").into());
        }
        output::action(&format!("Removing {name}"));
        output::sub_action("remove");
        output::hook_event(&name, "remove", HookStatus::Running, "executing recipe hook");
        let ctx = recipe.runtime.ctx();
        let ctx = recipe
            .runtime
            .run_hook("remove", None, ctx)
            .inspect_err(|e| report_phase_failure(&name, "remove", e))?;
        report_phase_success(&name, "remove");
        exec.checkpoint(&recipe.path, &ctx)?;
        output::success(&format!("{name} removed"));
        Ok(ctx)
    })
}

pub(crate) fn cleanup(
    engine: &Engine,
    exec: &mut ExecutionContext<'_>,
    recipe: &Path,
    load: &Load<'_>,
    reason: &str,
) -> Result<Map, EngineError> {
    with_recipe(engine, exec, recipe, load, |recipe, exec| {
        let name = recipe.name.clone();
        let ctx = recipe.runtime.ctx();
        if !recipe.runtime.has_cleanup() {
            output::hook_event(&name, "cleanup", HookStatus::Skipped, "recipe has no cleanup hook");
            output::skip(&format!("{name} has no cleanup hook; nothing to clean"));
            return Ok(ctx);
        }
        output::action(&format!("Cleaning up {name}"));
        output::sub_action("cleanup");
        output::hook_event(&name, "cleanup", HookStatus::Running, "executing recipe hook");
        let ctx = recipe
            .runtime
            .cleanup(ctx, reason)
            .inspect_err(|e| report_phase_failure(&name, "cleanup", e))?;
        report_phase_success(&name, "cleanup");
        exec.checkpoint(&recipe.path, &ctx)?;
        output::success(&format!("{name} cleaned"));
        Ok(ctx)
    })
}

/// Run one check on request. Unlike install, where a missing check means the
/// phase must run, asking for a check the recipe lacks is an error.
pub(crate) fn check(
    engine: &Engine,
    exec: &mut ExecutionContext<'_>,
    recipe: &Path,
    load: &Load<'_>,
    check: &str,
) -> Result<Map, EngineError> {
    with_recipe(engine, exec, recipe, load, |recipe, _exec| {
        let name = recipe.name.clone();
        let event = format!("check.{check}");
        if !recipe.runtime.has_fn(check) {
            output::hook_event(&name, &event, HookStatus::Missing, "required check function missing");
            output::error(&format!("{name} is missing required check function `{check}(ctx)`"));
            output::detail(
                "Action: define this check function and return ready(ctx) when ready, or needs_work(ctx, reason) otherwise.",
            );
            return Err(anyhow!("{name} has no {check} function").into());
        }
        output::action(&format!("Checking recipe {name}"));
        output::sub_action(&format!("{check} check"));
        output::hook_event(&name, &event, HookStatus::Manual, "manual check requested");
        let ctx = recipe.runtime.ctx();
        let ctx = recipe.runtime.verify(check, &ctx).map_err(|e| {
            output::hook_event(&name, &event, HookStatus::Failed, &format!("{e}"));
            output::error(&format!("{name}: {check} check failed"));
            output::detail(&format!("  reason: {e}"));
            output::detail("  action: check that function; it must return ready(ctx) or needs_work(ctx, reason).");
            anyhow!("{check} failed: {e}")
        })?;
        output::success(&format!("{name}: {check} check complete"));
        output::hook_event(&name, &event, HookStatus::Success, "manual check complete");
        Ok(ctx)
    })
}
