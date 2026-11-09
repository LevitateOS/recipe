//! Loading one recipe for one operation. Every operation goes through
//! [`with_recipe`].

use super::context::ExecutionContext;
use crate::core::executor::compile_recipe;
use crate::core::invocation::{Runtime, RuntimeInputs};
use crate::lifecycle::Phase;
use rhai::Engine;
use std::path::{Path, PathBuf};

/// How an engine operation failed.
#[derive(Debug)]
pub(crate) enum EngineError {
    /// Outside a phase: compile, setup, checks, dependencies, remove/cleanup hooks.
    Recipe(anyhow::Error),
    /// A phase hook or its verification.
    Phase { phase: Phase, error: anyhow::Error },
}

impl From<anyhow::Error> for EngineError {
    fn from(error: anyhow::Error) -> Self {
        Self::Recipe(error)
    }
}

/// A recipe compiled, set up and given its saved state.
pub(crate) struct Loaded<'e> {
    /// Canonical identity (checkpoints and saved state are keyed by it).
    pub path: PathBuf,
    pub base_path: Option<PathBuf>,
    /// `ctx.name`, or the file stem.
    pub name: String,
    pub runtime: Runtime<'e>,
    pub needs: crate::needs::Needs,
}

/// What [`with_recipe`] needs besides the recipe itself.
pub(crate) struct Load<'x> {
    pub search_path: Option<&'x Path>,
    pub build_dir: &'x Path,
    pub defines: &'x [(String, String)],
    pub operation: &'x str,
}

/// Compile `recipe`, run its setup with its saved state, then run `f`.
pub(crate) fn with_recipe<'e, T>(
    engine: &'e Engine,
    exec: &mut ExecutionContext<'_>,
    recipe: &Path,
    load: &Load<'_>,
    f: impl FnOnce(&mut Loaded<'e>, &mut ExecutionContext<'_>) -> Result<T, EngineError>,
) -> Result<T, EngineError> {
    let compiled = compile_recipe(engine, recipe, load.search_path)?;
    let declaration = crate::needs::declaration(&compiled.recipe_path, load.search_path)?;
    if declaration.source_digests != compiled.source_digests {
        return Err(anyhow::anyhow!("worker declaration changed during compile").into());
    }
    {
        let base_dir = compiled.base_path.as_deref().and_then(Path::parent);
        let mut runtime = Runtime::new(
            engine,
            compiled.ast.clone(),
            RuntimeInputs {
                recipe: &compiled.recipe_path,
                base_dir,
                build_dir: load.build_dir,
                defines: load.defines,
                options: exec.options().clone(),
                operation: load.operation,
                persist_state: exec.resumable(),
            },
        )?;
        if let Some(state) = exec.saved_state(&compiled.recipe_path) {
            runtime.restore_state(state)?;
        }
        let name = runtime
            .ctx()
            .get("name")
            .and_then(|value| value.clone().into_string().ok())
            .unwrap_or_else(|| {
                compiled
                    .recipe_path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
        let mut loaded = Loaded {
            path: compiled.recipe_path.clone(),
            base_path: compiled.base_path.clone(),
            name,
            runtime,
            needs: declaration.needs,
        };
        f(&mut loaded, exec)
    }
}
