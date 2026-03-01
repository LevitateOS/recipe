use crate::core::executor::compile_recipe;
use crate::core::output;
use crate::core::runner;
use anyhow::anyhow;
use rhai::{Engine, Scope};
use std::path::Path;

use super::{
    attempt::InstallAttemptError,
    reporting::{friendly_reason, report_check_result, report_phase_failure, report_phase_success},
    state::{check_phase_with_reason, maybe_cleanup, persist_ctx, resolve_deps},
};

pub(crate) fn install_once(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    defines: &[(String, String)],
    persist_ctx_enabled: bool,
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

    output::action(&format!("Preparing recipe for {}", name));
    output::hook_event(&name, "prepare", "running", "starting recipe execution");
    output::detail(&format!("Recipe: {}", recipe_path.display()));
    if let Some(base_path) = &compiled.base_path {
        output::detail(&format!("Extends base recipe: {}", base_path.display()));
    }

    // Check steps (reverse order) - throw means "needs this step".
    //
    // IMPORTANT: `is_*` checks may also *return an updated ctx* (e.g. setting
    // `ctx.source_path` or `ctx.build_dir`). When the check passes, we must carry
    // the returned ctx forward even if the phase is skipped.
    let (needs_install, checked_ctx, needs_install_reason) =
        check_phase_with_reason(engine, &ast, &scope, "is_installed", &ctx_map);
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
            check_phase_with_reason(engine, &ast, &scope, "is_built", &ctx_map);
        report_check_result(&name, "is_built", nb, build_reason.as_deref());
        needs_build = nb;
        ctx_map = checked_ctx;

        if needs_build {
            let (na, checked_ctx, acquire_reason) =
                check_phase_with_reason(engine, &ast, &scope, "is_acquired", &ctx_map);
            report_check_result(&name, "is_acquired", na, acquire_reason.as_deref());
            needs_acquire = na;
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

    let cleanup_auto_supported = runner::has_fn_arity(&ast, "cleanup", 2);

    if !needs_install {
        output::hook_event(&name, "install", "skipped", "already installed");
        output::detail("All checks passed; nothing to do.");
        output::skip(&format!("{} already installed, skipping", name));
        return Ok(ctx_map);
    }

    // Cleanup is required in this repository: it provides the hygiene hooks needed
    // for consistent build dir behavior (especially on failure paths).
    if !cleanup_auto_supported {
        output::hook_event(
            &name,
            "cleanup",
            "missing",
            "required cleanup(ctx, reason) hook missing",
        );
        output::error(&format!(
            "{} requires cleanup(ctx, reason) for this repo and it is missing.",
            name
        ));
        output::detail("Expected signature: fn cleanup(ctx, reason) -> { ... }");
        output::detail(
            "Action: add a cleanup hook to your recipe or temporarily run in a test environment that allows skipping it.",
        );
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
            resolve_deps(engine, build_dir, search_path, defines, &deps, autofix)
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
        match runner::run_phase(engine, &ast, &mut scope, "acquire", ctx_map) {
            Ok(new_ctx) => {
                ctx_map = new_ctx;
                report_phase_success(&name, "acquire");
                if persist_ctx_enabled {
                    persist_ctx(
                        &mut compiled,
                        &ctx_map,
                        "Failed to persist ctx after acquire",
                    )
                    .map_err(InstallAttemptError::Fatal)?;
                }

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
                    if persist_ctx_enabled {
                        persist_ctx(
                            &mut compiled,
                            &ctx_map,
                            "Failed to persist ctx after cleanup",
                        )
                        .map_err(InstallAttemptError::Fatal)?;
                    }
                }
            }
            Err(e) => {
                report_phase_failure(&name, "acquire", &e);
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

    if needs_build && runner::has_fn(&ast, "build") {
        // Re-check: does the recipe still need building after acquire ran?
        // (acquire may have updated ctx such that is_built now passes)
        let (still_needs_build, checked_ctx, post_acquire_build_reason) =
            check_phase_with_reason(engine, &ast, &scope, "is_built", &ctx_map);
        if let Some(reason) = post_acquire_build_reason {
            output::detail(&friendly_reason(
                "is_built (post acquire)",
                &reason,
                &ctx_map,
            ));
        }
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
            output::detail("Compiling or assembling build products");
            output::hook_event(&name, "build", "running", "executing recipe hook");
            let ctx_before = ctx_map.clone();
            match runner::run_phase(engine, &ast, &mut scope, "build", ctx_map) {
                Ok(new_ctx) => {
                    ctx_map = new_ctx;
                    report_phase_success(&name, "build");
                    if persist_ctx_enabled {
                        persist_ctx(&mut compiled, &ctx_map, "Failed to persist ctx after build")
                            .map_err(InstallAttemptError::Fatal)?;
                    }

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
                        if persist_ctx_enabled {
                            persist_ctx(
                                &mut compiled,
                                &ctx_map,
                                "Failed to persist ctx after cleanup",
                            )
                            .map_err(InstallAttemptError::Fatal)?;
                        }
                    }
                }
                Err(e) => {
                    report_phase_failure(&name, "build", &e);
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
        output::detail("Applying package files to destination");
        output::hook_event(&name, "install", "running", "executing recipe hook");
        let ctx_before = ctx_map.clone();
        match runner::run_phase(engine, &ast, &mut scope, "install", ctx_map) {
            Ok(new_ctx) => {
                ctx_map = new_ctx;
                report_phase_success(&name, "install");
                if persist_ctx_enabled {
                    persist_ctx(
                        &mut compiled,
                        &ctx_map,
                        "Failed to persist ctx after install",
                    )
                    .map_err(InstallAttemptError::Fatal)?;
                }

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
                    if persist_ctx_enabled {
                        persist_ctx(
                            &mut compiled,
                            &ctx_map,
                            "Failed to persist ctx after cleanup",
                        )
                        .map_err(InstallAttemptError::Fatal)?;
                    }
                }
            }
            Err(e) => {
                report_phase_failure(&name, "install", &e);
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
    if autofix_enabled
        && runner::has_fn_arity(&ast, "is_installed", 1)
        && let Err(e) = engine.call_fn::<rhai::Map>(
            &mut scope.clone(),
            &ast,
            "is_installed",
            (ctx_map.clone(),),
        )
    {
        report_phase_failure(
            &name,
            "post-install verification",
            &anyhow!("is_installed failed after install: {e}"),
        );
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

    output::success(&format!("{} installed", name));
    output::hook_event(&name, "install", "success", "recipe completed");
    Ok(ctx_map)
}
