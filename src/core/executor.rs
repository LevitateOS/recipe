//! Recipe executor - install, remove, cleanup operations
//!
//! Executes recipes using the ctx pattern where:
//! - `is_acquired(ctx)`, `is_built(ctx)`, `is_installed(ctx)` return ready(ctx) or needs_work(ctx, reason)
//! - `acquire(ctx)`, `build(ctx)`, `install(ctx)` return updated ctx
//! - checkpoints send runtime state to the coordinator without source mutation

use anyhow::{Context, Result, anyhow};
use rhai::{AST, Engine};
use std::fs;
use std::path::{Path, PathBuf};

mod private;

pub(crate) use private::{EngineError, ExecutionContext, Load, check, cleanup, install, remove};

use crate::recipes::parse_extends;

/// Package facts `declared_metadata` reports.
const METADATA_FIELDS: [&str; 3] = ["name", "version", "description"];

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
    pub ast: AST,
    pub source_ast: AST,
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
/// Returns the merged AST and immutable recipe identities.
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
        base_ast += child_ast;

        Ok(CompiledRecipe {
            ast: base_ast,
            source_ast,
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
            source_digests,
            ast,
            recipe_path,
            base_path: None,
        })
    }
}

/// Metadata declared in the same compiled recipe used by the worker.
/// Values requiring execution are listed explicitly rather than evaluated on host.
#[derive(Debug, Default)]
pub struct DeclaredMetadata {
    pub fields: std::collections::BTreeMap<String, String>,
    pub dynamic_fields: std::collections::BTreeSet<String>,
}

/// Read literal metadata from the merged recipe AST without executing setup,
/// imports, hooks or helper calls. Dynamic fields remain unavailable.
pub fn declared_metadata(recipe: &Path, search_path: Option<&Path>) -> Result<DeclaredMetadata> {
    use rhai::{Expr, Stmt};
    let engine = crate::core::invocation::compile_engine();
    let compiled = compile_recipe(&engine, recipe, search_path)?;
    let mut metadata = DeclaredMetadata::default();
    for statement in compiled.ast.statements() {
        match statement {
            Stmt::Var(declaration, _, _) if declaration.0.name == "ctx" => {
                metadata = DeclaredMetadata::default();
                match &declaration.1 {
                    Expr::Map(values, _) => {
                        for (name, value) in &values.0 {
                            metadata.set(name.name.as_str(), value)?;
                        }
                    }
                    _ => {
                        metadata
                            .dynamic_fields
                            .extend(METADATA_FIELDS.map(str::to_owned));
                    }
                }
            }
            Stmt::Assignment(assignment) => {
                let binary = &assignment.1;
                if let Expr::Dot(property, _, _) = &binary.lhs
                    && let (Expr::Variable(variable, _, _), Expr::Property(field, _)) =
                        (&property.lhs, &property.rhs)
                    && variable.1 == "ctx"
                {
                    if assignment.0.is_op_assignment() {
                        metadata.fields.remove(field.2.as_str());
                        metadata.dynamic_fields.insert(field.2.to_string());
                    } else {
                        metadata.set(field.2.as_str(), &binary.rhs)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(metadata)
}

impl DeclaredMetadata {
    fn set(&mut self, name: &str, value: &rhai::Expr) -> Result<()> {
        if !METADATA_FIELDS.contains(&name) {
            return Ok(());
        }
        self.fields.remove(name);
        self.dynamic_fields.remove(name);
        match value {
            rhai::Expr::StringConstant(text, _) => {
                self.fields.insert(name.to_owned(), text.to_string());
            }
            _ if value.get_literal_value().is_some() => {
                anyhow::bail!("declared metadata {name} must be a string")
            }
            _ => {
                self.dynamic_fields.insert(name.to_owned());
            }
        }
        Ok(())
    }
}
