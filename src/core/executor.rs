//! Recipe executor - install, remove, cleanup operations
//!
//! Executes recipes using the ctx pattern where:
//! - `is_acquired(ctx)`, `is_built(ctx)`, `is_installed(ctx)` throw if phase needed
//! - `acquire(ctx)`, `build(ctx)`, `install(ctx)` return updated ctx
//! - `ctx` is persisted to the recipe file after each phase (unless disabled)

use anyhow::{Context, Result, anyhow};
use rhai::{AST, Engine};
use std::fs;
use std::path::{Path, PathBuf};

mod private;

/// Parse `//! extends: <path>` from leading comments.
///
/// Only looks at comment lines at the top of the file. Stops at the first
/// non-comment, non-empty line.
pub(crate) fn parse_extends(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("//! extends:") {
            return Some(rest.trim().to_string());
        }
        if !trimmed.starts_with("//") {
            break;
        }
    }
    None
}

/// Resolve a base recipe path.
///
/// Tries relative to the child recipe's directory first, then the search path.
fn resolve_base_path(
    base_rel: &str,
    child_path: &Path,
    search_path: Option<&Path>,
) -> Result<PathBuf> {
    // Try relative to child
    if let Some(child_dir) = child_path.parent() {
        let candidate = child_dir.join(base_rel);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    // Try search path
    if let Some(sp) = search_path {
        let candidate = sp.join(base_rel);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    anyhow::bail!(
        "Base recipe '{}' not found (child: {}, search_path: {:?})",
        base_rel,
        child_path.display(),
        search_path
    )
}

#[derive(Debug)]
pub(crate) struct CompiledRecipe {
    pub ast: AST,
    /// The "main" recipe file (the one the user invoked).
    pub recipe_path: PathBuf,
    pub recipe_source: String,
    /// Optional base recipe from `//! extends:`.
    pub base_path: Option<PathBuf>,
    pub base_source: Option<String>,
    pub base_dir: Option<PathBuf>,
}

/// Compile a recipe with `//! extends:` resolution.
///
/// If the child recipe declares `//! extends: <base>`, the base is compiled first
/// and merged with the child AST. Child functions with the same name+arity replace
/// base functions. Top-level statements run base-first, then child.
///
/// Returns the merged AST plus the source texts/paths needed for ctx persistence.
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

    let extends = parse_extends(&source);

    if let Some(base_rel) = extends {
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
        base_ast += child_ast;

        let base_dir = base_path.parent().map(|p| p.to_path_buf());
        Ok(CompiledRecipe {
            ast: base_ast,
            recipe_path,
            recipe_source: source,
            base_path: Some(base_path),
            base_source: Some(base_source),
            base_dir,
        })
    } else {
        let ast = engine
            .compile(&source)
            .map_err(|e| anyhow!("Failed to compile recipe: {}", e))?;
        Ok(CompiledRecipe {
            ast,
            recipe_path,
            recipe_source: source,
            base_path: None,
            base_source: None,
            base_dir: None,
        })
    }
}

/// Install a package by executing its recipe
///
/// Follows the recipe workflow:
/// 1. Check is_installed(ctx) - skip if doesn't throw
/// 2. Check is_built(ctx) - skip build if doesn't throw
/// 3. Check is_acquired(ctx) - skip acquire if doesn't throw
/// 4. Execute needed steps (acquire, build, install)
/// 5. Persist ctx after each step
///
/// Returns the final ctx map containing all recipe state.
pub fn install(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    defines: &[(String, String)],
    persist_ctx: bool,
    search_path: Option<&Path>,
) -> Result<rhai::Map> {
    private::install(
        engine,
        build_dir,
        recipe_path,
        defines,
        persist_ctx,
        search_path,
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
    private::install_with_options(
        engine,
        build_dir,
        recipe_path,
        defines,
        persist_ctx,
        search_path,
        autofix,
    )
}

/// Remove an installed package
///
/// Returns the final ctx map after removal.
pub fn remove(
    engine: &Engine,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    persist_ctx: bool,
) -> Result<rhai::Map> {
    private::remove(engine, recipe_path, search_path, defines, persist_ctx)
}

/// Clean up build artifacts
///
/// Returns the final ctx map after cleanup.
pub fn cleanup(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    reason: &str,
    persist_ctx: bool,
) -> Result<rhai::Map> {
    private::cleanup(
        engine,
        build_dir,
        recipe_path,
        search_path,
        defines,
        reason,
        persist_ctx,
    )
}

/// Execute `is_installed(ctx)` manually.
///
/// Returns the updated ctx map on success.
pub fn is_installed(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
) -> Result<rhai::Map> {
    private::is_installed(engine, build_dir, recipe_path, search_path, defines)
}

/// Execute `is_built(ctx)` manually.
///
/// Returns the updated ctx map on success.
pub fn is_built(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
) -> Result<rhai::Map> {
    private::is_built(engine, build_dir, recipe_path, search_path, defines)
}

/// Execute `is_acquired(ctx)` manually.
///
/// Returns the updated ctx map on success.
pub fn is_acquired(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
) -> Result<rhai::Map> {
    private::is_acquired(engine, build_dir, recipe_path, search_path, defines)
}
