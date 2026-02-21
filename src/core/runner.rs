//! Shared runtime helpers for recipe execution and dependency resolution.

use anyhow::{Result, anyhow};
use rhai::{AST, Engine, Scope};

/// Check whether an AST contains a function by name.
pub fn has_fn(ast: &AST, name: &str) -> bool {
    ast.iter_functions().any(|f| f.name == name)
}

/// Check whether an AST contains a function with the expected arity.
pub fn has_fn_arity(ast: &AST, name: &str, arity: usize) -> bool {
    ast.iter_functions()
        .any(|f| f.name == name && f.params.len() == arity)
}

/// Execute a phase function that takes and returns `ctx`.
pub fn run_phase(
    engine: &Engine,
    ast: &AST,
    scope: &mut Scope,
    fn_name: &str,
    ctx: rhai::Map,
) -> Result<rhai::Map> {
    engine
        .call_fn::<rhai::Map>(scope, ast, fn_name, (ctx,))
        .map_err(|e| anyhow!("{} failed: {}", fn_name, e))
}

/// Return `true` when a check function throws.
pub fn check_throws(
    engine: &Engine,
    ast: &AST,
    scope: &Scope,
    fn_name: &str,
    ctx: &rhai::Map,
) -> bool {
    if !has_fn(ast, fn_name) {
        return true;
    }

    engine
        .call_fn::<rhai::Map>(&mut scope.clone(), ast, fn_name, (ctx.clone(),))
        .is_err()
}
