//! Recipe executor: compiling a recipe (with its `//! extends:` base) and the
//! install, remove and cleanup operations.
//!
//! C2: the operations below `private` still run the API 2 lifecycle (ctx,
//! `is_*` checks and hooks); C2 replaces them with docs/recipe-format.md
//! sections 3 and 4 (build(src, out), configure, verify, cleanup(reason) and
//! the engine's constants). Compiling and the declarations are API 1.

use anyhow::{Context, Result, anyhow};
use rhai::{AST, Engine};
use std::fs;
use std::path::{Path, PathBuf};

mod private;

pub(crate) use private::{EngineError, ExecutionContext, Load, check, cleanup, install, remove};

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
    pub source_digests: std::collections::BTreeMap<PathBuf, String>,
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
/// base and its pins file (`NAME.pins`, docs/recipe-format.md section 2.7).
pub(crate) fn compile_recipe(
    engine: &Engine,
    recipe_path: &Path,
    search_path: Option<&Path>,
) -> Result<CompiledRecipe> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let source = fs::read_to_string(&recipe_path)
        .with_context(|| format!("Failed to read recipe: {}", recipe_path.display()))?;

    use sha2::{Digest, Sha256};
    let mut source_digests = std::collections::BTreeMap::new();
    source_digests.insert(
        recipe_path.clone(),
        hex::encode(Sha256::digest(source.as_bytes())),
    );
    let pins = crate::pins::Pins::path_of(&recipe_path);
    if let Ok(bytes) = fs::read(&pins) {
        source_digests.insert(pins, hex::encode(Sha256::digest(&bytes)));
    }
    let extends = parse_extends(&source);

    if let Some(base_rel) = extends {
        let base_path = resolve_base_path(&base_rel, &recipe_path, search_path)?;
        let base_path = base_path
            .canonicalize()
            .unwrap_or_else(|_| base_path.to_path_buf());
        let base_source = fs::read_to_string(&base_path)
            .with_context(|| format!("Failed to read base recipe: {}", base_path.display()))?;

        source_digests.insert(
            base_path.clone(),
            hex::encode(Sha256::digest(base_source.as_bytes())),
        );
        // Reject recursive extends
        if parse_extends(&base_source).is_some() {
            anyhow::bail!(
                "Recursive extends not supported: {} extends {} which also extends",
                recipe_path.display(),
                base_path.display()
            );
        }

        let mut base_ast = engine.compile(&base_source).map_err(|e| {
            anyhow!(
                "Failed to compile base recipe {}: {}",
                base_path.display(),
                e
            )
        })?;

        let child_ast = engine
            .compile(&source)
            .map_err(|e| anyhow!("Failed to compile recipe {}: {}", recipe_path.display(), e))?;

        // Merge: child overrides base functions, top-level runs base then child
        let source_ast = child_ast.clone();
        let base_source_ast = base_ast.clone();
        base_ast += child_ast;

        Ok(CompiledRecipe {
            ast: base_ast,
            source_ast,
            base_ast: Some(base_source_ast),
            source_digests,
            recipe_path,
            base_path: Some(base_path),
        })
    } else {
        let ast = engine
            .compile(&source)
            .map_err(|e| anyhow!("Failed to compile recipe {}: {}", recipe_path.display(), e))?;
        Ok(CompiledRecipe {
            source_ast: ast.clone(),
            base_ast: None,
            source_digests,
            ast,
            recipe_path,
            base_path: None,
        })
    }
}
