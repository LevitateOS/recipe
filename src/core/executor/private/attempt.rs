use rhai::Engine;
use std::path::Path;

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

pub(crate) fn install_with_context(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    defines: &[(String, String)],
    persist_state: bool,
    search_path: Option<&Path>,
    exec: &mut ExecutionContext<'_>,
) -> std::result::Result<rhai::Map, InstallAttemptError> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    exec.enter_recipe(&recipe_path, build_dir)
        .map_err(InstallAttemptError::Fatal)?;
    let result = install_once(
        engine,
        build_dir,
        &recipe_path,
        defines,
        persist_state,
        search_path,
        exec,
    );
    exec.leave_recipe();
    result
}
