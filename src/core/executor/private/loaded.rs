//! Loading one recipe for one operation. Every operation goes through
//! [`with_recipe`].

use crate::core::executor::compile_recipe;
use crate::core::invocation::{Runtime, compile_engine, literal_constants, set_constants};
use crate::lifecycle::Phase;
use rhai::Engine;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How an engine operation failed.
#[derive(Debug)]
pub(crate) enum EngineError {
    /// Outside a step: compile, setup, dependencies, `cleanup`, `latest`.
    Recipe(anyhow::Error),
    /// A step (`download`, or the step that builds the outputs).
    Phase { phase: Phase, error: anyhow::Error },
}

impl From<anyhow::Error> for EngineError {
    fn from(error: anyhow::Error) -> Self {
        Self::Recipe(error)
    }
}

/// A recipe compiled with this run's constants.
pub(crate) struct Loaded<'e> {
    /// Canonical identity (checkpoints and saved state are keyed by it).
    pub path: PathBuf,
    pub base_path: Option<PathBuf>,
    /// The recipe's name, its file name without `.rhai` (equal to
    /// `PACKAGE.name`, docs/recipe-format.md, decision 3).
    pub name: String,
    pub runtime: Runtime<'e>,
    pub declaration: crate::needs::Declaration,
}

/// What [`with_recipe`] needs besides the recipe itself.
pub(crate) struct Load<'x> {
    /// The recipe's name as the coordinator read it from the real file (the
    /// request's `recipe_name`); the staged file is `main.rhai`.
    pub name: &'x str,
    pub search_path: Option<&'x Path>,
    /// A fresh worker engine with its helpers and module resolver.
    pub make_engine: &'x dyn Fn() -> Engine,
    pub defines: &'x [(String, String)],
    /// Each dependency's declared output names (the request's).
    pub dependencies: &'x BTreeMap<String, Vec<String>>,
    pub options: &'x crate::ExecutionOptions,
    /// `latest()`: it runs with no inputs and may lack a required define;
    /// what is missing is `()` instead of an error.
    pub lenient: bool,
}

/// Read `recipe`'s declarations, build this run's constants, compile it with
/// them, then run `f`.
pub(crate) fn with_recipe<T>(
    recipe: &Path,
    load: &Load<'_>,
    f: impl FnOnce(&mut Loaded<'_>) -> Result<T, EngineError>,
) -> Result<T, EngineError> {
    let declaration = crate::needs::declaration_named(recipe, load.name, load.search_path)?;
    // Literal constants come from the source as written: the optimizer of
    // the worker's engine would fold a computed constant into a literal.
    let plain = compile_recipe(&compile_engine(), recipe, load.search_path)?;
    let mut constants = literal_constants(&plain.ast);
    constants.extend(super::constants::of(&declaration, load)?);
    let mut engine = (load.make_engine)();
    set_constants(&mut engine, constants);
    let compiled = compile_recipe(&engine, recipe, load.search_path)?;
    if declaration.source_digests != compiled.source_digests
        || plain.source_digests != compiled.source_digests
    {
        return Err(anyhow::anyhow!("worker declaration changed during compile").into());
    }
    let runtime = Runtime::new(&engine, compiled.ast.clone())?;
    let mut loaded = Loaded {
        path: compiled.recipe_path.clone(),
        base_path: compiled.base_path.clone(),
        name: declaration.name().to_owned(),
        runtime,
        declaration,
    };
    f(&mut loaded)
}
