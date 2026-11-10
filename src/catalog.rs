//! What a front end may show about a recipe before anything runs.
//!
//! An entry is read from the compiled recipe alone: the declared name, version
//! and description, the optional `ABOUT` block, NEEDS, the dependency names and
//! the lifecycle hooks the file defines. No setup, hook or helper is evaluated
//! and no Ginux launcher is needed, so a whole collection can be listed by an
//! ordinary user.

use anyhow::{Context, Result, ensure};
use rhai::{ASTFlags, Expr, Stmt};
use std::path::Path;

/// The version of the JSON printed by `recipe list --json`.
pub const SCHEMA_VERSION: u32 = 1;

const HOOKS: [&str; 5] = ["acquire", "build", "install", "remove", "cleanup"];

/// One recipe as a catalog entry.
///
/// `ABOUT` is the recipe author's own description (kind, category, longer
/// text, home page, licence, what it adds, notes on steps). It must be a
/// `const` literal map like NEEDS; its fields are passed through as written.
/// NEEDS is enforced by Ginux; `ABOUT` is a claim.
pub fn entry(recipe: &Path, search_path: Option<&Path>) -> Result<serde_json::Value> {
    let declaration = crate::needs::declaration(recipe, search_path)?;
    let metadata = crate::core::executor::declared_metadata(&declaration.recipe, search_path)?;
    let engine = crate::core::invocation::compile_engine();
    let compiled =
        crate::core::executor::compile_recipe(&engine, &declaration.recipe, search_path)?;

    let mut about = None;
    for statement in compiled.source_ast.statements() {
        if let Stmt::Var(binding, flags, _) = statement
            && binding.0.name == "ABOUT"
        {
            ensure!(about.is_none(), "recipe must declare ABOUT at most once");
            ensure!(
                flags.contains(ASTFlags::CONSTANT) && matches!(&binding.1, Expr::Map(..)),
                "ABOUT must be a const literal map"
            );
            about = Some(
                crate::needs::literal(&binding.1)
                    .context("ABOUT must be literal data, without helper calls or variables")?,
            );
        }
    }

    let field = |name: &str| {
        (!metadata.dynamic_fields.contains(name))
            .then(|| metadata.fields.get(name).cloned())
            .flatten()
    };
    let name = field("name").unwrap_or_else(|| {
        declaration
            .recipe
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_owned()
    });
    let names = |build_only: bool| -> Vec<&str> {
        declaration
            .dependencies
            .iter()
            .filter(|dependency| dependency.build_only == build_only)
            .map(|dependency| dependency.name.as_str())
            .collect()
    };
    let hooks: Vec<&str> = HOOKS
        .into_iter()
        .filter(|hook| {
            compiled
                .ast
                .iter_functions()
                .any(|function| function.name == *hook)
        })
        .collect();

    Ok(serde_json::json!({
        "name": name,
        "version": field("version"),
        "description": field("description"),
        "path": declaration.recipe,
        "about": about,
        "needs": declaration.needs,
        "deps": names(false),
        "build_deps": names(true),
        "hooks": hooks,
    }))
}
