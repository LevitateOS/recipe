//! Recipe action invocation
//!
//! Functions for checking and calling recipe action functions.

use anyhow::Result;
use rhai::{AST, Engine, Scope};

/// Check if an action function exists in the AST
pub fn has_action(ast: &AST, name: &str) -> bool {
    ast.iter_functions().any(|f| f.name == name)
}

/// Call an action function in the recipe
pub fn call_action(engine: &Engine, scope: &mut Scope, ast: &AST, action: &str) -> Result<()> {
    if !has_action(ast, action) {
        return Err(anyhow::anyhow!("Action '{}' not defined", action));
    }

    engine
        .call_fn::<()>(scope, ast, action, ())
        .map_err(|e| anyhow::anyhow!("Action '{}' failed: {}", action, e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviso_cheat_test::cheat_reviewed;

    #[cheat_reviewed("API test - has_action detects existing functions")]
    #[test]
    fn test_has_action_exists() {
        let engine = Engine::new();
        let ast = engine.compile("fn acquire() {} fn install() {}").unwrap();
        assert!(has_action(&ast, "acquire"));
        assert!(has_action(&ast, "install"));
    }

    #[cheat_reviewed("API test - has_action returns false for missing functions")]
    #[test]
    fn test_has_action_missing() {
        let engine = Engine::new();
        let ast = engine.compile("fn acquire() {}").unwrap();
        assert!(!has_action(&ast, "install"));
        assert!(!has_action(&ast, "build"));
    }

    #[cheat_reviewed("API test - has_action on script with no functions")]
    #[test]
    fn test_has_action_empty_script() {
        let engine = Engine::new();
        let ast = engine.compile("let x = 1;").unwrap();
        assert!(!has_action(&ast, "acquire"));
    }

    #[cheat_reviewed("API test - call_action succeeds on valid function")]
    #[test]
    fn test_call_action_success() {
        let engine = Engine::new();
        let ast = engine.compile("fn test_action() { let x = 1; }").unwrap();
        let mut scope = Scope::new();
        let result = call_action(&engine, &mut scope, &ast, "test_action");
        assert!(result.is_ok());
    }

    #[cheat_reviewed("API test - call_action fails on missing function")]
    #[test]
    fn test_call_action_missing() {
        let engine = Engine::new();
        let ast = engine.compile("fn other() {}").unwrap();
        let mut scope = Scope::new();
        let result = call_action(&engine, &mut scope, &ast, "missing");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not defined"));
    }

    #[cheat_reviewed("API test - call_action propagates runtime errors")]
    #[test]
    fn test_call_action_runtime_error() {
        let engine = Engine::new();
        // This will cause a runtime error (undefined variable)
        let ast = engine.compile("fn bad_action() { undefined_var }").unwrap();
        let mut scope = Scope::new();
        let result = call_action(&engine, &mut scope, &ast, "bad_action");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("failed"));
    }
}
