//! Recipe state management
//!
//! Functions for reading and updating recipe state variables.

use crate::core::recipe_state::{self, OptionalString};
use anyhow::{Context, Result};
use rhai::{AST, Engine, Scope};
use std::path::Path;

/// Update recipe state variables after successful install
pub fn update_recipe_state(
    recipe_path: &Path,
    version: &Option<String>,
    installed_files: &[std::path::PathBuf],
) -> Result<()> {
    // Set installed = true
    recipe_state::set_var(recipe_path, "installed", &true)
        .with_context(|| "Failed to set installed state")?;

    // Set installed_version
    if let Some(ver) = version {
        recipe_state::set_var(
            recipe_path,
            "installed_version",
            &OptionalString::Some(ver.clone()),
        )
        .with_context(|| "Failed to set installed_version")?;
    }

    // Set installed_at (Unix timestamp)
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    recipe_state::set_var(recipe_path, "installed_at", &timestamp)
        .with_context(|| "Failed to set installed_at")?;

    // Set installed_files
    let files: Vec<String> = installed_files
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    recipe_state::set_var(recipe_path, "installed_files", &files)
        .with_context(|| "Failed to set installed_files")?;

    Ok(())
}

/// Clear recipe state after successful removal
pub fn clear_recipe_state(recipe_path: &Path) -> Result<()> {
    recipe_state::set_var(recipe_path, "installed", &false)
        .with_context(|| "Failed to update installed state")?;
    recipe_state::set_var(
        recipe_path,
        "installed_version",
        &OptionalString::None,
    )
    .with_context(|| "Failed to clear installed_version")?;
    recipe_state::set_var(
        recipe_path,
        "installed_at",
        &OptionalString::None,
    )
    .with_context(|| "Failed to clear installed_at")?;
    recipe_state::set_var(
        recipe_path,
        "installed_files",
        &Vec::<String>::new(),
    )
    .with_context(|| "Failed to clear installed_files")?;

    Ok(())
}

/// Get a string variable from the recipe
pub fn get_recipe_var(engine: &Engine, scope: &mut Scope, ast: &AST, var_name: &str) -> Option<String> {
    // Run the script to populate scope
    let mut test_scope = scope.clone();
    engine.run_ast_with_scope(&mut test_scope, ast).ok()?;
    test_scope.get_value::<String>(var_name)
}

/// Get the recipe name from script variables or filename
pub fn get_recipe_name(engine: &Engine, scope: &mut Scope, ast: &AST, recipe_path: &Path) -> String {
    engine
        .eval_ast_with_scope::<String>(scope, ast)
        .ok()
        .or_else(|| {
            // Try to get 'name' variable from script
            let mut test_scope = scope.clone();
            engine.run_ast_with_scope(&mut test_scope, ast).ok()?;
            test_scope.get_value::<String>("name")
        })
        .unwrap_or_else(|| {
            recipe_path
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviso_cheat_test::cheat_reviewed;
    use std::path::Path;

    #[cheat_reviewed("API test - recipe name extracted from variable")]
    #[test]
    fn test_get_recipe_name_from_variable() {
        let engine = Engine::new();
        let ast = engine.compile(r#"let name = "my-package";"#).unwrap();
        let mut scope = Scope::new();
        let name = get_recipe_name(&engine, &mut scope, &ast, Path::new("/test/fallback.rhai"));
        assert_eq!(name, "my-package");
    }

    #[cheat_reviewed("API test - recipe name falls back to filename")]
    #[test]
    fn test_get_recipe_name_fallback_to_filename() {
        let engine = Engine::new();
        let ast = engine.compile("let version = \"1.0\";").unwrap();
        let mut scope = Scope::new();
        let name = get_recipe_name(
            &engine,
            &mut scope,
            &ast,
            Path::new("/test/fallback-pkg.rhai"),
        );
        assert_eq!(name, "fallback-pkg");
    }
}
