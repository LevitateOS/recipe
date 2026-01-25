//! Recipe validation
//!
//! Validates that recipes have all required variables and functions before execution.

use super::action::has_action;
use anyhow::Result;
use rhai::{AST, Engine, Scope};
use std::path::Path;

/// Required variables that every recipe MUST define
pub const REQUIRED_VARS: &[&str] = &["name", "version", "installed"];

/// Required functions that every recipe MUST define
pub const REQUIRED_FUNCTIONS: &[&str] = &["acquire", "install"];

/// Validate that a recipe has all required variables and functions.
/// Returns an error with a clear message listing ALL missing items.
pub fn validate_recipe(engine: &Engine, ast: &AST, recipe_path: &Path) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    // Check required variables by running the script
    let mut scope = Scope::new();
    if let Err(e) = engine.run_ast_with_scope(&mut scope, ast) {
        return Err(anyhow::anyhow!(
            "Recipe '{}' failed to execute: {}",
            recipe_path.display(),
            e
        ));
    }

    // Check each required variable
    for var in REQUIRED_VARS {
        if scope.get_value::<rhai::Dynamic>(var).is_none() {
            errors.push(format!("missing required variable: `let {} = ...;`", var));
        }
    }

    // Validate variable types
    if let Some(name) = scope.get_value::<rhai::Dynamic>("name") {
        if !name.is_string() {
            errors.push(format!(
                "`name` must be a string, got: {}",
                name.type_name()
            ));
        } else if name
            .clone()
            .into_string()
            .map(|s| s.is_empty())
            .unwrap_or(true)
        {
            errors.push("`name` cannot be empty".to_string());
        }
    }

    if let Some(version) = scope.get_value::<rhai::Dynamic>("version") {
        if !version.is_string() {
            errors.push(format!(
                "`version` must be a string, got: {}",
                version.type_name()
            ));
        } else if version
            .clone()
            .into_string()
            .map(|s| s.is_empty())
            .unwrap_or(true)
        {
            errors.push("`version` cannot be empty".to_string());
        }
    }

    // Validate `installed` is a boolean
    if let Some(installed) = scope.get_value::<rhai::Dynamic>("installed") {
        if !installed.is_bool() {
            errors.push(format!(
                "`installed` must be a boolean (true/false), got: {}",
                installed.type_name()
            ));
        } else if installed.as_bool().unwrap_or(false) {
            // If installed = true, then installed_version and installed_files are REQUIRED
            if scope
                .get_value::<rhai::Dynamic>("installed_version")
                .is_none()
            {
                errors.push(
                    "missing `installed_version` (required when installed = true)".to_string(),
                );
            }
            if scope
                .get_value::<rhai::Dynamic>("installed_files")
                .is_none()
            {
                errors
                    .push("missing `installed_files` (required when installed = true)".to_string());
            }
        }
    }

    // Check required functions
    for func in REQUIRED_FUNCTIONS {
        if !has_action(ast, func) {
            errors.push(format!(
                "missing required function: `fn {}() {{ ... }}`",
                func
            ));
        }
    }

    // If any errors, fail with a comprehensive message
    if !errors.is_empty() {
        let recipe_name = recipe_path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        return Err(anyhow::anyhow!(
            "Invalid recipe '{}' ({}):\n  - {}",
            recipe_name,
            recipe_path.display(),
            errors.join("\n  - ")
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviso_cheat_test::{cheat_aware, cheat_reviewed};
    use tempfile::TempDir;

    fn create_test_recipe(name: &str, content: &str) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(format!("{}.rhai", name));
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    #[cheat_aware(
        protects = "User is warned when recipe missing required 'name' field",
        severity = "HIGH",
        ease = "EASY",
        cheats = [
            "Skip name validation entirely",
            "Use filename as name without warning",
            "Accept empty string as valid name"
        ],
        consequence = "User installs package with no name - can't remove, list, or manage it"
    )]
    #[test]
    fn test_validate_recipe_missing_name() {
        let (_dir, recipe_path) = create_test_recipe(
            "no-name",
            r#"
let version = "1.0";
fn acquire() {}
fn install() {}
"#,
        );
        let engine = Engine::new();
        let ast = engine
            .compile(&std::fs::read_to_string(&recipe_path).unwrap())
            .unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing required variable"));
        assert!(err.contains("name"));
    }

    #[cheat_aware(
        protects = "User is warned when recipe missing required 'version' field",
        severity = "HIGH",
        ease = "EASY",
        cheats = [
            "Skip version validation",
            "Use default version like '0.0.0'",
            "Accept missing version silently"
        ],
        consequence = "User installs package with no version - upgrades and rollbacks impossible"
    )]
    #[test]
    fn test_validate_recipe_missing_version() {
        let (_dir, recipe_path) = create_test_recipe(
            "no-version",
            r#"
let name = "test";
fn acquire() {}
fn install() {}
"#,
        );
        let engine = Engine::new();
        let ast = engine
            .compile(&std::fs::read_to_string(&recipe_path).unwrap())
            .unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing required variable"));
        assert!(err.contains("version"));
    }

    #[cheat_aware(
        protects = "User is warned when recipe missing required 'acquire' function",
        severity = "HIGH",
        ease = "EASY",
        cheats = [
            "Skip acquire validation",
            "Create empty acquire function automatically",
            "Accept recipes without acquire"
        ],
        consequence = "User installs package but nothing is downloaded - install fails silently"
    )]
    #[test]
    fn test_validate_recipe_missing_acquire() {
        let (_dir, recipe_path) = create_test_recipe(
            "no-acquire",
            r#"
let name = "test";
let version = "1.0";
fn install() {}
"#,
        );
        let engine = Engine::new();
        let ast = engine
            .compile(&std::fs::read_to_string(&recipe_path).unwrap())
            .unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing required function"));
        assert!(err.contains("acquire"));
    }

    #[cheat_aware(
        protects = "User is warned when recipe missing required 'install' function",
        severity = "HIGH",
        ease = "EASY",
        cheats = [
            "Skip install validation",
            "Create empty install function automatically",
            "Accept recipes without install"
        ],
        consequence = "User installs package - acquire succeeds but nothing gets installed"
    )]
    #[test]
    fn test_validate_recipe_missing_install() {
        let (_dir, recipe_path) = create_test_recipe(
            "no-install",
            r#"
let name = "test";
let version = "1.0";
fn acquire() {}
"#,
        );
        let engine = Engine::new();
        let ast = engine
            .compile(&std::fs::read_to_string(&recipe_path).unwrap())
            .unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing required function"));
        assert!(err.contains("install"));
    }

    #[cheat_reviewed("Validation test - multiple errors reported at once")]
    #[test]
    fn test_validate_recipe_multiple_errors() {
        let (_dir, recipe_path) = create_test_recipe(
            "many-errors",
            r#"
// Completely empty recipe - missing everything
let x = 1;
"#,
        );
        let engine = Engine::new();
        let ast = engine
            .compile(&std::fs::read_to_string(&recipe_path).unwrap())
            .unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        // Should list ALL missing items
        assert!(err.contains("name"));
        assert!(err.contains("version"));
        assert!(err.contains("installed"));
        assert!(err.contains("acquire"));
        assert!(err.contains("install"));
    }

    #[cheat_reviewed("Validation test - empty name string rejected")]
    #[test]
    fn test_validate_recipe_empty_name() {
        let (_dir, recipe_path) = create_test_recipe(
            "empty-name",
            r#"
let name = "";
let version = "1.0";
let installed = false;
fn acquire() {}
fn install() {}
"#,
        );
        let engine = Engine::new();
        let ast = engine
            .compile(&std::fs::read_to_string(&recipe_path).unwrap())
            .unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cannot be empty"));
    }

    #[cheat_reviewed("Validation test - name must be string type")]
    #[test]
    fn test_validate_recipe_wrong_type_name() {
        let (_dir, recipe_path) = create_test_recipe(
            "wrong-type",
            r#"
let name = 123;  // Should be string
let version = "1.0";
let installed = false;
fn acquire() {}
fn install() {}
"#,
        );
        let engine = Engine::new();
        let ast = engine
            .compile(&std::fs::read_to_string(&recipe_path).unwrap())
            .unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must be a string"));
    }

    #[cheat_reviewed("Validation test - installed field required")]
    #[test]
    fn test_validate_recipe_missing_installed() {
        let (_dir, recipe_path) = create_test_recipe(
            "no-installed",
            r#"
let name = "test";
let version = "1.0";
fn acquire() {}
fn install() {}
"#,
        );
        let engine = Engine::new();
        let ast = engine
            .compile(&std::fs::read_to_string(&recipe_path).unwrap())
            .unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("installed"));
    }

    #[cheat_reviewed("Validation test - installed must be boolean")]
    #[test]
    fn test_validate_recipe_installed_wrong_type() {
        let (_dir, recipe_path) = create_test_recipe(
            "installed-wrong-type",
            r#"
let name = "test";
let version = "1.0";
let installed = "yes";  // Should be boolean
fn acquire() {}
fn install() {}
"#,
        );
        let engine = Engine::new();
        let ast = engine
            .compile(&std::fs::read_to_string(&recipe_path).unwrap())
            .unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must be a boolean"));
    }

    #[cheat_reviewed("Validation test - installed=true requires installed_version")]
    #[test]
    fn test_validate_recipe_installed_true_missing_version() {
        let (_dir, recipe_path) = create_test_recipe(
            "installed-no-version",
            r#"
let name = "test";
let version = "1.0";
let installed = true;
let installed_files = [];
fn acquire() {}
fn install() {}
"#,
        );
        let engine = Engine::new();
        let ast = engine
            .compile(&std::fs::read_to_string(&recipe_path).unwrap())
            .unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("installed_version"));
        assert!(err.contains("required when installed = true"));
    }

    #[cheat_reviewed("Validation test - installed=true requires installed_files")]
    #[test]
    fn test_validate_recipe_installed_true_missing_files() {
        let (_dir, recipe_path) = create_test_recipe(
            "installed-no-files",
            r#"
let name = "test";
let version = "1.0";
let installed = true;
let installed_version = "1.0";
fn acquire() {}
fn install() {}
"#,
        );
        let engine = Engine::new();
        let ast = engine
            .compile(&std::fs::read_to_string(&recipe_path).unwrap())
            .unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("installed_files"));
        assert!(err.contains("required when installed = true"));
    }

    #[cheat_reviewed("Validation test - installed=true with all required fields passes")]
    #[test]
    fn test_validate_recipe_installed_true_valid() {
        let (_dir, recipe_path) = create_test_recipe(
            "installed-valid",
            r#"
let name = "test";
let version = "1.0";
let installed = true;
let installed_version = "1.0";
let installed_files = ["/usr/bin/test"];
fn acquire() {}
fn install() {}
"#,
        );
        let engine = Engine::new();
        let ast = engine
            .compile(&std::fs::read_to_string(&recipe_path).unwrap())
            .unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_ok());
    }

    #[cheat_reviewed("Validation test - complete valid recipe passes")]
    #[test]
    fn test_validate_recipe_valid() {
        let (_dir, recipe_path) = create_test_recipe(
            "valid",
            r#"
let name = "test-package";
let version = "1.0.0";
let installed = false;
let description = "A test package";  // Optional
fn acquire() {}
fn install() {}
"#,
        );
        let engine = Engine::new();
        let ast = engine
            .compile(&std::fs::read_to_string(&recipe_path).unwrap())
            .unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_ok());
    }
}
