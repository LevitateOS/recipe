//! Recipe executor: compiling a recipe (with its `//! extends:` base) and the
//! worker's operations: download (the download step), build and install (the
//! download step and the step that builds the outputs), cleanup
//! (`cleanup("manual")`) and latest (`latest()`), per docs/recipe-format.md
//! sections 3, 4, 6 and 7.

use anyhow::{Context, Result, anyhow};
use rhai::{AST, Engine};
use std::fs;
use std::path::{Path, PathBuf};

mod private;

pub(crate) use private::{EngineError, Load, cleanup, download, install, latest};

#[derive(Debug)]
pub(crate) struct CompiledRecipe {
    /// Base and child merged: what the worker runs.
    pub ast: AST,
    /// The child's own source.
    pub source_ast: AST,
    /// The base's own source, for a child that extends one.
    pub base_ast: Option<AST>,
    /// Every file the recipe is compiled from, by SHA-256 of the bytes read
    /// here: the recipe, its pins file, the base it extends and the modules
    /// it, its base or a module imports.
    pub source_digests: std::collections::BTreeMap<PathBuf, String>,
    /// The modules it, its base or a module imports, canonical, each once
    /// and with its own AST (compiled without the optimizer).
    pub modules: Vec<(PathBuf, AST)>,
    /// The "main" recipe file (the one the user invoked).
    pub recipe_path: PathBuf,
    /// Optional base recipe from `//! extends:`.
    pub base_path: Option<PathBuf>,
}

/// Compile a recipe with `//! extends:` resolution.
///
/// If the child recipe declares `//! extends: <base>`, the base is compiled first
/// and merged with the child AST. Child functions with the same name+arity replace
/// base functions. Top-level statements run base-first, then child.
///
/// Returns the merged AST and immutable recipe identities: the recipe, its
/// base, its pins file (`NAME.pins`, docs/recipe-format.md section 2.7) and
/// the modules they import, each read once.
pub(crate) fn compile_recipe(
    engine: &Engine,
    recipe_path: &Path,
    search_path: Option<&Path>,
) -> Result<CompiledRecipe> {
    use sha2::{Digest, Sha256};
    let digest = |bytes: &[u8]| hex::encode(Sha256::digest(bytes));

    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let source = fs::read_to_string(&recipe_path)
        .with_context(|| format!("Failed to read recipe: {}", recipe_path.display()))?;

    let related = crate::recipes::related(&recipe_path, &source, search_path)?;
    let base = related.base;
    let modules = related.modules;

    let base_ast = base
        .as_ref()
        .map(|base| {
            engine.compile(&base.text).map_err(|e| {
                anyhow!(
                    "Failed to compile base recipe {}: {}",
                    base.canonical.display(),
                    e
                )
            })
        })
        .transpose()?;
    let source_ast = engine
        .compile(&source)
        .map_err(|e| anyhow!("Failed to compile recipe {}: {}", recipe_path.display(), e))?;

    let mut source_digests = std::collections::BTreeMap::new();
    source_digests.insert(recipe_path.clone(), digest(source.as_bytes()));
    let pins = crate::pins::Pins::path_of(&recipe_path);
    if let Ok(bytes) = fs::read(&pins) {
        source_digests.insert(pins, digest(&bytes));
    }
    if let Some(base) = &base {
        source_digests.insert(base.canonical.clone(), digest(base.text.as_bytes()));
    }
    for (path, module) in &modules {
        source_digests.insert(path.clone(), digest(module.text.as_bytes()));
    }

    // Merge: child overrides base functions, top-level runs base then child
    let ast = match &base_ast {
        Some(base_ast) => {
            let mut merged = base_ast.clone();
            merged += source_ast.clone();
            merged
        }
        None => source_ast.clone(),
    };

    Ok(CompiledRecipe {
        ast,
        source_ast,
        base_ast,
        source_digests,
        modules: modules
            .into_iter()
            .map(|(path, module)| (path, module.ast))
            .collect(),
        recipe_path,
        base_path: base.map(|base| base.canonical),
    })
}
