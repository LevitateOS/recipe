//! Shared runtime helpers for recipe execution and dependency resolution.

use super::invocation::Runtime;
use anyhow::{Result, anyhow};
use rhai::AST;

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
pub(crate) fn run_phase(
    ast: &AST,
    scope: &mut Runtime<'_>,
    fn_name: &str,
    ctx: rhai::Map,
) -> Result<rhai::Map> {
    let ctx = scope
        .call_map(fn_name, vec![ctx.into()], false)
        .map_err(|e| anyhow!("{} failed: {}", fn_name, e))?;
    if scope.is_api2() {
        let check = match fn_name {
            "acquire" => Some("is_acquired"),
            "build" => Some("is_built"),
            "install" => Some("is_installed"),
            _ => None,
        };
        if let Some(check) = check.filter(|check| has_fn(ast, check)) {
            return verify_phase(scope, check, &ctx);
        }
    }
    Ok(ctx)
}

/// Run the actual recipe check again after a producer, preserving discovered ctx.
pub(crate) fn verify_phase(
    scope: &mut Runtime<'_>,
    name: &str,
    ctx: &rhai::Map,
) -> Result<rhai::Map> {
    match scope.check(name, ctx)? {
        super::invocation::Readiness::Ready(ctx) => Ok(ctx),
        super::invocation::Readiness::NeedsWork(_, reason) => {
            Err(anyhow!("{name} is not ready: {reason}"))
        }
    }
}
