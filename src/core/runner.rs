//! Shared runtime helpers for recipe execution and dependency resolution.

use super::invocation::{Runtime, has_fn, has_fn_arity};
use anyhow::{Result, anyhow};
use rhai::AST;

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
    {
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

/// Cleanup is optional, but a present lifecycle hook must accept ctx and reason.
/// Validate before phase execution so a typo cannot silently disable hygiene.
pub(crate) fn cleanup_available(ast: &AST) -> Result<bool> {
    if !has_fn(ast, "cleanup") {
        return Ok(false);
    }
    if !has_fn_arity(ast, "cleanup", 2) {
        return Err(anyhow!(
            "cleanup hook must be cleanup(ctx, reason) (found no 2-arg cleanup)"
        ));
    }
    Ok(true)
}

/// An absent hook is a no-op. After a failed phase, preserve its primary error;
/// after a successful phase, a cleanup failure is itself a command failure.
pub(crate) fn run_cleanup(
    ast: &AST,
    scope: &mut Runtime<'_>,
    ctx: rhai::Map,
    reason: &str,
) -> Result<rhai::Map> {
    if !cleanup_available(ast)? {
        return Ok(ctx);
    }
    match scope.call_map("cleanup", vec![ctx.clone().into(), reason.into()], false) {
        Ok(ctx) => Ok(ctx),
        Err(error) => Err(anyhow!("cleanup failed (reason={reason}): {error}")),
    }
}
