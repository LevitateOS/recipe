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

use crate::recipes::parse_extends;

/// Resolve a base recipe: beside the child first, then the search path.
fn resolve_base_path(
    base_rel: &str,
    child_path: &Path,
    search_path: Option<&Path>,
) -> Result<PathBuf> {
    crate::recipes::check_related_target(base_rel)
        .map_err(|problem| anyhow::anyhow!("Base recipe '{base_rel}' {problem}"))?;
    crate::recipes::find_related(base_rel, child_path, search_path).ok_or_else(|| {
        anyhow::anyhow!(
            "Base recipe '{}' not found (child: {}, search_path: {:?})",
            base_rel,
            child_path.display(),
            search_path
        )
    })
}

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

    let base = match parse_extends(&source) {
        Some(base_rel) => {
            let base_path = resolve_base_path(&base_rel, &recipe_path, search_path)?;
            let base_path = base_path
                .canonicalize()
                .unwrap_or_else(|_| base_path.to_path_buf());
            let base_source = fs::read_to_string(&base_path)
                .with_context(|| format!("Failed to read base recipe: {}", base_path.display()))?;
            // Reject recursive extends
            if parse_extends(&base_source).is_some() {
                anyhow::bail!(
                    "Recursive extends not supported: {} extends {} which also extends",
                    recipe_path.display(),
                    base_path.display()
                );
            }
            Some((base_path, base_source))
        }
        None => None,
    };

    let base_ast = base
        .as_ref()
        .map(|(base_path, base_source)| {
            engine.compile(base_source).map_err(|e| {
                anyhow!(
                    "Failed to compile base recipe {}: {}",
                    base_path.display(),
                    e
                )
            })
        })
        .transpose()?;
    let source_ast = engine
        .compile(&source)
        .map_err(|e| anyhow!("Failed to compile recipe {}: {}", recipe_path.display(), e))?;

    let mut files = vec![(recipe_path.as_path(), source.as_str())];
    if let Some((base_path, base_source)) = &base {
        files.push((base_path.as_path(), base_source.as_str()));
    }
    let modules = crate::recipes::imports(&files, search_path)?.modules;

    let mut source_digests = std::collections::BTreeMap::new();
    source_digests.insert(recipe_path.clone(), digest(source.as_bytes()));
    let pins = crate::pins::Pins::path_of(&recipe_path);
    if let Ok(bytes) = fs::read(&pins) {
        source_digests.insert(pins, digest(&bytes));
    }
    if let Some((base_path, base_source)) = &base {
        source_digests.insert(base_path.clone(), digest(base_source.as_bytes()));
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
        base_path: base.map(|(base_path, _)| base_path),
    })
}
