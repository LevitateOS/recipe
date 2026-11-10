//! Install: decide which phases a recipe needs, then run them in order.

use super::context::ExecutionContext;
use super::loaded::{EngineError, Load, Loaded, with_recipe};
use super::reporting::{report_check, report_phase_failure, report_phase_success};
use crate::core::events::HookStatus;
use crate::core::invocation::Readiness;
use crate::core::output;
use crate::lifecycle::{CleanupReason, Phase};
use anyhow::anyhow;
use rhai::{Engine, Map};
use std::path::Path;

pub(crate) fn install(
    engine: &Engine,
    exec: &mut ExecutionContext<'_>,
    recipe: &Path,
    load: &Load<'_>,
) -> Result<Map, EngineError> {
    with_recipe(engine, exec, recipe, load, |recipe, exec| {
        let name = recipe.name.clone();
        output::set_event_recipe(&name);
        output::action(&format!("Preparing recipe for {name}"));
        output::hook_event(
            &name,
            "prepare",
            HookStatus::Running,
            "starting recipe execution",
        );
        output::detail(&format!("Recipe: {}", recipe.path.display()));
        if let Some(base_path) = &recipe.base_path {
            output::detail(&format!("Extends base recipe: {}", base_path.display()));
        }

        let (mut ctx, phases) = plan(recipe, exec.options())?;
        // For a front end: which steps run this time; the others are not needed.
        output::machine_event(serde_json::json!({
            "event": "plan",
            "recipe": name,
            "steps": phases.iter().map(|phase| phase.name()).collect::<Vec<_>>(),
        }));
        if phases.is_empty() {
            recipe.needs.validate_worker_outputs(&ctx, true)?;
            exec.checkpoint_reused(&recipe.path, &ctx)?;
            output::hook_event(&name, "install", HookStatus::Skipped, "already installed");
            output::detail("All checks passed; nothing to do.");
            output::skip(&format!("{name} already installed, skipping"));
            return Ok(ctx);
        }
        output::detail(&format!(
            "Recipe flow: {}",
            phases
                .iter()
                .map(|phase| phase.name())
                .collect::<Vec<_>>()
                .join(" → ")
        ));

        let deps = recipe.runtime.dependency_names("deps")?;
        let build_deps = recipe.runtime.dependency_names("build_deps")?;
        describe_dependencies("Runtime dependencies", "deps", &deps);
        describe_dependencies("Build-time dependencies", "build_deps", &build_deps);
        // `deps` are needed by every phase; their environment lasts until install ends.
        let _deps_environment = super::dependencies::select(&deps)?;

        output::action(&format!("Installing {name}"));
        output::hook_event(
            &name,
            "install",
            HookStatus::Requested,
            "hook execution queued",
        );
        let acquires = phases.contains(&Phase::Acquire);
        for phase in phases {
            // `build_deps` are needed only while building.
            let _build_environment = if phase == Phase::Build {
                if acquires && !still_needs_build(recipe, exec, &mut ctx)? {
                    continue;
                }
                super::dependencies::select(&build_deps)?
            } else {
                None
            };
            ctx = run_phase(recipe, exec, phase, ctx)?;
        }

        // A success cleanup may have changed both products and rediscovered
        // output/state values. Keep the actual verification context.
        if recipe.runtime.has_cleanup() && recipe.runtime.has_fn(Phase::Install.check()) {
            match recipe.runtime.verify(Phase::Install.check(), &ctx) {
                Ok(verified) => ctx = verified,
                Err(error) => {
                    let error = anyhow!("is_installed failed after install: {error}");
                    report_phase_failure(&name, "post-install verification", &error);
                    failure_cleanup(recipe, exec, Phase::Install, ctx);
                    return Err(EngineError::Phase {
                        phase: Phase::Install,
                        error,
                    });
                }
            }
        }
        recipe.needs.validate_worker_outputs(&ctx, true)?;
        output::success(&format!("{name} installed"));
        output::hook_event(&name, "install", HookStatus::Success, "recipe completed");
        Ok(ctx)
    })
}

/// Run the checks outermost first and list the phases to run. Each check may
/// return an updated ctx, which carries forward even when its phase is skipped.
fn plan(
    recipe: &mut Loaded<'_>,
    options: &crate::ExecutionOptions,
) -> anyhow::Result<(Map, Vec<Phase>)> {
    let mut ctx = recipe.runtime.ctx();
    let mut needed = Vec::new();
    for phase in Phase::ALL.into_iter().rev() {
        let readiness = recipe.runtime.check(phase.check(), &ctx)?;
        let forced = options.forces(phase);
        let needs_work = matches!(readiness, Readiness::NeedsWork(..));
        report_check(&recipe.name, phase.check(), &readiness, forced);
        ctx = match readiness {
            Readiness::Ready(ctx) | Readiness::NeedsWork(ctx, _) => ctx,
        };
        recipe.needs.validate_worker_outputs(&ctx, false)?;
        if !(needs_work || forced) {
            break;
        }
        // `build` is the one optional producer.
        if phase != Phase::Build || recipe.runtime.has_fn(phase.name()) {
            needed.push(phase);
        }
    }
    needed.reverse();
    Ok((ctx, needed))
}

/// Acquire may have produced what the build would (a prebuilt artifact, say).
fn still_needs_build(
    recipe: &mut Loaded<'_>,
    exec: &ExecutionContext<'_>,
    ctx: &mut Map,
) -> anyhow::Result<bool> {
    let readiness = recipe.runtime.check(Phase::Build.check(), ctx)?;
    let forced = exec.options().forces(Phase::Build);
    let needs_work = matches!(readiness, Readiness::NeedsWork(..));
    if needs_work {
        report_check(&recipe.name, "is_built (post acquire)", &readiness, forced);
    }
    *ctx = match readiness {
        Readiness::Ready(ctx) | Readiness::NeedsWork(ctx, _) => ctx,
    };
    recipe.needs.validate_worker_outputs(ctx, false)?;
    Ok(needs_work || forced)
}

/// Run one phase hook and its verification, checkpoint it, then run the
/// success cleanup and checkpoint that. On failure, run the failure cleanup
/// on the ctx the phase started with; the phase error stays the result.
fn run_phase(
    recipe: &mut Loaded<'_>,
    exec: &mut ExecutionContext<'_>,
    phase: Phase,
    ctx: Map,
) -> Result<Map, EngineError> {
    let name = recipe.name.clone();
    output::sub_action(phase.name());
    output::detail(match phase {
        Phase::Acquire => "Checking/refreshing source artifacts",
        Phase::Build => "Compiling or assembling build products",
        Phase::Install => "Applying package files to destination",
    });
    output::hook_event(
        &name,
        phase.name(),
        HookStatus::Running,
        "executing recipe hook",
    );
    let ctx_before = ctx.clone();
    let ctx = match recipe
        .runtime
        .run_hook(phase.name(), Some(phase.check()), ctx)
        .and_then(|ctx| {
            recipe
                .needs
                .validate_worker_outputs(&ctx, phase == Phase::Install)?;
            Ok(ctx)
        }) {
        Ok(ctx) => ctx,
        Err(error) => {
            report_phase_failure(&name, phase.name(), &error);
            failure_cleanup(recipe, exec, phase, ctx_before);
            return Err(EngineError::Phase { phase, error });
        }
    };
    report_phase_success(&name, phase.name());
    exec.checkpoint(&recipe.path, &ctx)?;
    if !recipe.runtime.has_cleanup() {
        return Ok(ctx);
    }
    let ctx = recipe
        .runtime
        .cleanup(ctx, CleanupReason::Succeeded(phase).as_str())?;
    recipe
        .needs
        .validate_worker_outputs(&ctx, phase == Phase::Install)?;
    exec.checkpoint(&recipe.path, &ctx)?;
    Ok(ctx)
}

/// A completed failure cleanup is a recovery point. Its own failure is only
/// reported: the phase error stays authoritative and nothing is published.
fn failure_cleanup(
    recipe: &mut Loaded<'_>,
    exec: &mut ExecutionContext<'_>,
    phase: Phase,
    ctx: Map,
) {
    if !recipe.runtime.has_cleanup() {
        return;
    }
    let reason = CleanupReason::Failed(phase).as_str();
    match recipe.runtime.cleanup(ctx, reason) {
        Ok(cleaned) => {
            if let Err(error) = recipe
                .needs
                .validate_worker_outputs(&cleaned, false)
                .and_then(|()| exec.checkpoint(&recipe.path, &cleaned))
            {
                output::warning(&format!(
                    "failure cleanup checkpoint failed ({reason}): {error:#}"
                ));
            }
        }
        Err(error) => output::warning(&format!("{error:#}")),
    }
}

fn describe_dependencies(label: &str, field: &str, names: &[String]) {
    if names.is_empty() {
        let kind = if field == "deps" {
            "runtime"
        } else {
            "build-time"
        };
        output::detail(&format!(
            "No {kind} dependency recipes declared (`{field}`)."
        ));
    } else {
        output::detail(&format!("{label}: {}", names.join(", ")));
    }
}
