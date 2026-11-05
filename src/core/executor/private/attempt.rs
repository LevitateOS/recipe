use anyhow::{Context, Result};
use rhai::Engine;
use std::path::Path;

use crate::core::output;

use super::context::ExecutionContext;
use super::flow::install_once;

#[derive(Debug)]
pub(crate) enum InstallAttemptError {
    Fatal(anyhow::Error),
    Phase {
        reason: &'static str,
        phase: &'static str,
        error: anyhow::Error,
    },
}

pub(crate) fn install(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    defines: &[(String, String)],
    persist_ctx: bool,
    search_path: Option<&Path>,
) -> Result<rhai::Map> {
    install_with_options(
        engine,
        build_dir,
        recipe_path,
        defines,
        persist_ctx,
        search_path,
        None,
    )
}

pub(crate) fn install_with_options(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    defines: &[(String, String)],
    persist_ctx: bool,
    search_path: Option<&Path>,
    autofix: Option<&crate::AutoFixConfig>,
) -> Result<rhai::Map> {
    let mut exec = ExecutionContext::host();
    install_with_context(
        engine,
        build_dir,
        recipe_path,
        defines,
        persist_ctx,
        search_path,
        autofix,
        &mut exec,
    )
}

/// Same retry flow with caller-provided lock ownership and checkpoint sink.
///
/// Worker seam (R02 stub): the host coordinator holds the recipe lock and
/// receives each checkpoint through `exec` instead of this operation taking
/// a second lock or rewriting recipe sources itself.
pub(crate) fn install_with_context(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    defines: &[(String, String)],
    persist_ctx: bool,
    search_path: Option<&Path>,
    autofix: Option<&crate::AutoFixConfig>,
    exec: &mut ExecutionContext<'_>,
) -> Result<rhai::Map> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let _lock = exec.acquire_lock(&recipe_path)?;

    let max_attempts = autofix.map(|c| c.attempts as usize).unwrap_or(0);

    for attempt in 0..=max_attempts {
        match install_once(
            engine,
            build_dir,
            &recipe_path,
            defines,
            persist_ctx,
            search_path,
            autofix,
            exec,
        ) {
            Ok(ctx) => return Ok(ctx),
            Err(InstallAttemptError::Fatal(e)) => {
                output::error(&format!("Fatal install failure: {}", recipe_path.display()));
                output::detail(&format!("  reason: {e}"));
                output::detail(
                    "  action: inspect recipe loading/validation and ensure required helper functions are present.",
                );
                return Err(e);
            }
            Err(InstallAttemptError::Phase {
                reason,
                phase,
                error,
            }) => {
                let Some(cfg) = autofix else {
                    return Err(error);
                };

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
                crate::core::autofix::run_and_apply(
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
