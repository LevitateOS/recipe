use crate::core::executor::compile_recipe;
use crate::core::output;
use crate::core::runner;
use anyhow::anyhow;
use rhai::Engine;
use std::path::Path;

use super::{
    attempt::InstallAttemptError,
    context::{CheckpointPoint, ExecutionContext},
    reporting::{friendly_reason, report_check_result, report_phase_failure, report_phase_success},
    state::{check_phase_with_reason, resolve_deps},
};

/// Malformed dependency declarations are errors, not silently omitted tools.
fn dependency_names(scope: &rhai::Scope<'_>, field: &str) -> anyhow::Result<Vec<String>> {
    let Some(value) = scope.get_value::<rhai::Dynamic>(field) else {
        return Ok(Vec::new());
    };
    let values = value
        .try_cast::<rhai::Array>()
        .ok_or_else(|| anyhow!("{field} must be an array of recipe names"))?;
    values
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .into_string()
                .map_err(|_| anyhow!("{field}[{index}] must be a recipe name string"))
        })
        .collect()
}

pub(crate) fn install_once(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    defines: &[(String, String)],
    persist_state_enabled: bool,
    search_path: Option<&Path>,
    exec: &mut ExecutionContext<'_>,
) -> std::result::Result<rhai::Map, InstallAttemptError> {
    let mut compiled =
        compile_recipe(engine, recipe_path, search_path).map_err(InstallAttemptError::Fatal)?;
    let ast = compiled.ast.clone();
    let cleanup_auto_supported =
        runner::cleanup_available(&ast).map_err(InstallAttemptError::Fatal)?;

    let scope = crate::core::invocation::scope(
        &compiled.recipe_path,
        compiled.base_dir.as_deref(),
        Some(build_dir),
        Some(&exec.tools_prefix().map_err(InstallAttemptError::Fatal)?),
        defines,
        &exec.options(),
    );

    let mut scope = crate::core::invocation::Runtime::new(
        engine,
        &ast,
        scope,
        "install",
        persist_state_enabled,
    )
    .map_err(InstallAttemptError::Fatal)?;

    exec.restore_runtime(&compiled.recipe_path, &mut scope)
        .map_err(InstallAttemptError::Fatal)?;

    // Extract ctx from scope
    let mut ctx_map: rhai::Map = scope.scope().get_value("ctx").ok_or_else(|| {
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

    output::action(&format!("Preparing recipe for {}", name));
    output::hook_event(&name, "prepare", "running", "starting recipe execution");
    output::detail(&format!("Recipe: {}", recipe_path.display()));
    if let Some(base_path) = &compiled.base_path {
        output::detail(&format!("Extends base recipe: {}", base_path.display()));
    }

    let policy = exec.options();

    // Check steps (reverse order) - throw means "needs this step".
    //
    // IMPORTANT: `is_*` checks may also *return an updated ctx* (e.g. setting
    // `ctx.source_path` or `ctx.build_dir`). When the check passes, we must carry
    // the returned ctx forward even if the phase is skipped.
    let (needs_install, checked_ctx, needs_install_reason) =
        check_phase_with_reason(&mut scope, "is_installed", &ctx_map)
            .map_err(InstallAttemptError::Fatal)?;
    let needs_install = needs_install || policy.forced();
    report_check_result(
        &name,
        "is_installed",
        needs_install,
        needs_install_reason.as_deref(),
    );
    ctx_map = checked_ctx;

    let mut needs_build = false;
    let mut needs_acquire = false;
    if needs_install {
        let (nb, checked_ctx, build_reason) =
            check_phase_with_reason(&mut scope, "is_built", &ctx_map)
                .map_err(InstallAttemptError::Fatal)?;
        report_check_result(&name, "is_built", nb, build_reason.as_deref());
        needs_build = nb || policy.force_build || policy.force_acquire;
        ctx_map = checked_ctx;

        if needs_build {
            let (na, checked_ctx, acquire_reason) =
                check_phase_with_reason(&mut scope, "is_acquired", &ctx_map)
                    .map_err(InstallAttemptError::Fatal)?;
            report_check_result(&name, "is_acquired", na, acquire_reason.as_deref());
            needs_acquire = na || policy.force_acquire;
            ctx_map = checked_ctx;
        }
    }

    if needs_install {
        let mut planned = Vec::new();
        if needs_acquire {
            planned.push("acquire");
        }
        if needs_build && runner::has_fn(&ast, "build") {
            planned.push("build");
        }
        planned.push("install");
        output::detail(&format!("Recipe flow: {}", planned.join(" → ")));
    }

    if !needs_install {
        exec.checkpoint_reused(&mut compiled, &ctx_map)
            .map_err(InstallAttemptError::Fatal)?;
        output::hook_event(&name, "install", "skipped", "already installed");
        output::detail("All checks passed; nothing to do.");
        output::skip(&format!("{} already installed, skipping", name));
        return Ok(ctx_map);
    }

    // Resolve dependencies declared in scope.
    // - `deps`: resolved before all phases (tools needed for acquire/build/install)
    // - `build_deps`: resolved only before build phase (compile-time tools)
    let deps = dependency_names(scope.scope(), "deps").map_err(InstallAttemptError::Fatal)?;
    let build_deps =
        dependency_names(scope.scope(), "build_deps").map_err(InstallAttemptError::Fatal)?;

    if deps.is_empty() {
        output::detail("No runtime dependency recipes declared (`deps`).");
    } else {
        output::detail(&format!("Runtime dependencies: {}", deps.join(", ")));
    }

    if build_deps.is_empty() {
        output::detail("No build-time dependency recipes declared (`build_deps`).");
    } else {
        output::detail(&format!(
            "Build-time dependencies: {}",
            build_deps.join(", ")
        ));
    }

    // Resolve `deps` immediately (needed for all phases)
    let _env_guard = if !deps.is_empty() {
        Some(
            resolve_deps(
                engine,
                build_dir,
                search_path,
                defines,
                &deps,
                persist_state_enabled,
                exec,
            )
            .map_err(InstallAttemptError::Fatal)?,
        )
    } else {
        None
    };

    output::action(&format!("Installing {}", name));
    output::hook_event(&name, "install", "requested", "hook execution queued");

    // Execute needed phases
    if needs_acquire {
        output::sub_action("acquire");
        output::detail("Checking/refreshing source artifacts");
        output::hook_event(&name, "acquire", "running", "executing recipe hook");
        let ctx_before = ctx_map.clone();
        match runner::run_phase(&ast, &mut scope, "acquire", ctx_map) {
            Ok(new_ctx) => {
                ctx_map = new_ctx;
                report_phase_success(&name, "acquire");
                exec.persist_checkpoint(
                    &mut compiled,
                    &ctx_map,
                    CheckpointPoint {
                        phase: "acquire",
                        reason: "phase:acquire",
                    },
                    persist_state_enabled,
                )
                .map_err(InstallAttemptError::Fatal)?;

                // Hygiene: allow recipes to clean up intermediate acquire artifacts.
                if cleanup_auto_supported {
                    ctx_map =
                        runner::run_cleanup(&ast, &mut scope, ctx_map, "auto.acquire.success")
                            .map_err(InstallAttemptError::Fatal)?;
                    exec.persist_checkpoint(
                        &mut compiled,
                        &ctx_map,
                        CheckpointPoint {
                            phase: "cleanup",
                            reason: "auto.acquire.success",
                        },
                        persist_state_enabled,
                    )
                    .map_err(InstallAttemptError::Fatal)?;
                }
            }
            Err(e) => {
                report_phase_failure(&name, "acquire", &e);
                // Best-effort failure hygiene: don't mask the original error.
                if cleanup_auto_supported {
                    checkpoint_failure_cleanup(
                        &ast,
                        &mut scope,
                        ctx_before,
                        "auto.acquire.failure",
                        &mut compiled,
                        exec,
                        persist_state_enabled,
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

    if needs_build && runner::has_fn(&ast, "build") {
        // Re-check: does the recipe still need building after acquire ran?
        // (acquire may have updated ctx such that is_built now passes)
        let (still_needs_build, checked_ctx, post_acquire_build_reason) =
            check_phase_with_reason(&mut scope, "is_built", &ctx_map)
                .map_err(InstallAttemptError::Fatal)?;
        if let Some(reason) = post_acquire_build_reason {
            output::detail(&friendly_reason(
                "is_built (post acquire)",
                &reason,
                &ctx_map,
            ));
        }
        ctx_map = checked_ctx;

        let still_needs_build = still_needs_build || policy.force_build || policy.force_acquire;

        // Resolve build_deps only when actually building
        let _build_env_guard = if still_needs_build && !build_deps.is_empty() {
            Some(
                resolve_deps(
                    engine,
                    build_dir,
                    search_path,
                    defines,
                    &build_deps,
                    persist_state_enabled,
                    exec,
                )
                .map_err(InstallAttemptError::Fatal)?,
            )
        } else {
            None
        };

        if still_needs_build {
            output::sub_action("build");
            output::detail("Compiling or assembling build products");
            output::hook_event(&name, "build", "running", "executing recipe hook");
            let ctx_before = ctx_map.clone();
            match runner::run_phase(&ast, &mut scope, "build", ctx_map) {
                Ok(new_ctx) => {
                    ctx_map = new_ctx;
                    report_phase_success(&name, "build");
                    exec.persist_checkpoint(
                        &mut compiled,
                        &ctx_map,
                        CheckpointPoint {
                            phase: "build",
                            reason: "phase:build",
                        },
                        persist_state_enabled,
                    )
                    .map_err(InstallAttemptError::Fatal)?;

                    if cleanup_auto_supported {
                        ctx_map =
                            runner::run_cleanup(&ast, &mut scope, ctx_map, "auto.build.success")
                                .map_err(InstallAttemptError::Fatal)?;
                        exec.persist_checkpoint(
                            &mut compiled,
                            &ctx_map,
                            CheckpointPoint {
                                phase: "cleanup",
                                reason: "auto.build.success",
                            },
                            persist_state_enabled,
                        )
                        .map_err(InstallAttemptError::Fatal)?;
                    }
                }
                Err(e) => {
                    report_phase_failure(&name, "build", &e);
                    if cleanup_auto_supported {
                        checkpoint_failure_cleanup(
                            &ast,
                            &mut scope,
                            ctx_before,
                            "auto.build.failure",
                            &mut compiled,
                            exec,
                            persist_state_enabled,
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
        output::detail("Applying package files to destination");
        output::hook_event(&name, "install", "running", "executing recipe hook");
        let ctx_before = ctx_map.clone();
        match runner::run_phase(&ast, &mut scope, "install", ctx_map) {
            Ok(new_ctx) => {
                ctx_map = new_ctx;
                report_phase_success(&name, "install");
                exec.persist_checkpoint(
                    &mut compiled,
                    &ctx_map,
                    CheckpointPoint {
                        phase: "install",
                        reason: "phase:install",
                    },
                    persist_state_enabled,
                )
                .map_err(InstallAttemptError::Fatal)?;

                if cleanup_auto_supported {
                    ctx_map =
                        runner::run_cleanup(&ast, &mut scope, ctx_map, "auto.install.success")
                            .map_err(InstallAttemptError::Fatal)?;
                    exec.persist_checkpoint(
                        &mut compiled,
                        &ctx_map,
                        CheckpointPoint {
                            phase: "cleanup",
                            reason: "auto.install.success",
                        },
                        persist_state_enabled,
                    )
                    .map_err(InstallAttemptError::Fatal)?;
                }
            }
            Err(e) => {
                report_phase_failure(&name, "install", &e);
                if cleanup_auto_supported {
                    checkpoint_failure_cleanup(
                        &ast,
                        &mut scope,
                        ctx_before,
                        "auto.install.failure",
                        &mut compiled,
                        exec,
                        persist_state_enabled,
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

    // Verify installation after success cleanup before reporting completion.
    let verified = if runner::has_fn_arity(&ast, "is_installed", 1) {
        runner::verify_phase(&mut scope, "is_installed", &ctx_map)
    } else {
        Ok(ctx_map.clone())
    };
    ctx_map = match verified {
        Ok(ctx) => ctx,
        Err(e) => {
            report_phase_failure(
                &name,
                "post-install verification",
                &anyhow!("is_installed failed after install: {e}"),
            );
            // Allow best-effort failure hygiene.
            if cleanup_auto_supported {
                checkpoint_failure_cleanup(
                    &ast,
                    &mut scope,
                    ctx_map.clone(),
                    "auto.install.failure",
                    &mut compiled,
                    exec,
                    persist_state_enabled,
                );
            }

            return Err(InstallAttemptError::Phase {
                reason: "auto.install.failure",
                phase: "is_installed",
                error: anyhow!("is_installed failed after install: {e}"),
            });
        }
    };
    output::success(&format!("{} installed", name));
    output::hook_event(&name, "install", "success", "recipe completed");
    Ok(ctx_map)
}

/// A completed failure-cleanup hook is a recovery point; the producer error
/// remains authoritative and no failed output is published.
fn checkpoint_failure_cleanup(
    ast: &rhai::AST,
    scope: &mut crate::core::invocation::Runtime<'_>,
    ctx: rhai::Map,
    reason: &'static str,
    compiled: &mut crate::core::executor::CompiledRecipe,
    exec: &mut ExecutionContext<'_>,
    persist: bool,
) {
    match runner::run_cleanup(ast, scope, ctx, reason) {
        Ok(cleaned) => {
            if let Err(error) = exec.persist_checkpoint(
                compiled,
                &cleaned,
                CheckpointPoint {
                    phase: "cleanup",
                    reason,
                },
                persist,
            ) {
                output::warning(&format!(
                    "failure cleanup checkpoint failed ({reason}): {error:#}"
                ));
            }
        }
        Err(error) => output::warning(&format!("{error:#}")),
    }
}
