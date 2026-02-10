//! Recipe executor - install, remove, cleanup operations
//!
//! Executes recipes using the ctx pattern where:
//! - `is_acquired(ctx)`, `is_built(ctx)`, `is_installed(ctx)` throw if phase needed
//! - `acquire(ctx)`, `build(ctx)`, `install(ctx)` return updated ctx
//! - ctx is persisted to the recipe file after each phase

use super::{ctx, lock::acquire_recipe_lock, output};
use anyhow::{Context, Result, anyhow};
use rhai::{AST, Engine, Scope};
use std::fs;
use std::path::{Path, PathBuf};

/// Parse `//! extends: <path>` from leading comments.
///
/// Only looks at comment lines at the top of the file. Stops at the first
/// non-comment, non-empty line.
fn parse_extends(source: &str) -> Option<String> {
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

/// Compile a recipe with `//! extends:` resolution.
///
/// If the child recipe declares `//! extends: <base>`, the base is compiled first
/// and merged with the child AST. Child functions with the same name+arity replace
/// base functions. Top-level statements run base-first, then child.
///
/// Returns (merged_ast, child_source, Option<base_dir>).
fn compile_recipe(
    engine: &Engine,
    recipe_path: &Path,
    search_path: Option<&Path>,
) -> Result<(AST, String, Option<PathBuf>)> {
    let source = fs::read_to_string(recipe_path)
        .with_context(|| format!("Failed to read recipe: {}", recipe_path.display()))?;

    let extends = parse_extends(&source);

    if let Some(base_rel) = extends {
        let base_path = resolve_base_path(&base_rel, recipe_path, search_path)?;
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
        Ok((base_ast, source, base_dir))
    } else {
        let ast = engine
            .compile(&source)
            .map_err(|e| anyhow!("Failed to compile recipe: {}", e))?;
        Ok((ast, source, None))
    }
}

/// Install a package by executing its recipe
///
/// Follows the lifecycle:
/// 1. Check is_installed(ctx) - skip if doesn't throw
/// 2. Check is_built(ctx) - skip build if doesn't throw
/// 3. Check is_acquired(ctx) - skip acquire if doesn't throw
/// 4. Execute needed phases (acquire, build, install)
/// 5. Persist ctx after each phase
///
/// Returns the final ctx map containing all recipe state.
pub fn install(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    defines: &[(String, String)],
    search_path: Option<&Path>,
) -> Result<rhai::Map> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let _lock = acquire_recipe_lock(&recipe_path)?;

    let (ast, mut source, base_dir) = compile_recipe(engine, &recipe_path, search_path)?;

    // Derive RECIPE_DIR from the recipe file's parent directory
    let recipe_dir = recipe_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    // Set up scope with constants
    let mut scope = Scope::new();
    scope.push_constant("RECIPE_DIR", recipe_dir);
    if let Some(ref bd) = base_dir {
        scope.push_constant("BASE_RECIPE_DIR", bd.to_string_lossy().to_string());
    }
    scope.push_constant("BUILD_DIR", build_dir.to_string_lossy().to_string());
    scope.push_constant("ARCH", std::env::consts::ARCH);
    scope.push_constant("NPROC", num_cpus::get() as i64);
    scope.push_constant("RPM_PATH", std::env::var("RPM_PATH").unwrap_or_default());

    // Inject user-defined constants (from --define KEY=VALUE)
    for (key, value) in defines {
        scope.push_constant(key.as_str(), value.clone());
    }

    // Run script to populate scope (this sets up ctx)
    engine
        .run_ast_with_scope(&mut scope, &ast)
        .map_err(|e| anyhow!("Failed to run recipe: {}", e))?;

    // Extract ctx from scope
    let mut ctx_map: rhai::Map = scope
        .get_value("ctx")
        .ok_or_else(|| anyhow!("Recipe missing 'let ctx = #{{...}}'"))?;

    // Get package name for logging
    let name = ctx_map
        .get("name")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_else(|| {
            recipe_path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });

    // Check phases (reverse order) - throw means "needs this phase"
    let needs_install = check_throws(engine, &ast, &scope, "is_installed", &ctx_map);
    let needs_build = needs_install && check_throws(engine, &ast, &scope, "is_built", &ctx_map);
    let needs_acquire = needs_build && check_throws(engine, &ast, &scope, "is_acquired", &ctx_map);

    if !needs_install {
        output::skip(&format!("{} already installed, skipping", name));
        return Ok(ctx_map);
    }

    output::action(&format!("Installing {}", name));

    // Execute needed phases
    if needs_acquire {
        output::sub_action("acquire");
        ctx_map = run_phase(engine, &ast, &mut scope, "acquire", ctx_map)?;
        source = ctx::persist(&source, &ctx_map)?;
        fs::write(&recipe_path, &source).with_context(|| "Failed to persist ctx after acquire")?;
    }

    if needs_build && has_fn(&ast, "build") {
        output::sub_action("build");
        ctx_map = run_phase(engine, &ast, &mut scope, "build", ctx_map)?;
        source = ctx::persist(&source, &ctx_map)?;
        fs::write(&recipe_path, &source).with_context(|| "Failed to persist ctx after build")?;
    }

    if needs_install {
        output::sub_action("install");
        ctx_map = run_phase(engine, &ast, &mut scope, "install", ctx_map)?;
        source = ctx::persist(&source, &ctx_map)?;
        fs::write(&recipe_path, &source).with_context(|| "Failed to persist ctx after install")?;
    }

    output::success(&format!("{} installed", name));
    Ok(ctx_map)
}

/// Remove an installed package
///
/// Returns the final ctx map after removal.
pub fn remove(
    engine: &Engine,
    recipe_path: &Path,
    search_path: Option<&Path>,
) -> Result<rhai::Map> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let _lock = acquire_recipe_lock(&recipe_path)?;

    let (ast, mut source, base_dir) = compile_recipe(engine, &recipe_path, search_path)?;

    // Derive RECIPE_DIR from the recipe file's parent directory
    let recipe_dir = recipe_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    let mut scope = Scope::new();
    scope.push_constant("RECIPE_DIR", recipe_dir);
    if let Some(ref bd) = base_dir {
        scope.push_constant("BASE_RECIPE_DIR", bd.to_string_lossy().to_string());
    }

    // Run script to populate scope
    engine.run_ast_with_scope(&mut scope, &ast)?;

    let mut ctx_map: rhai::Map = scope
        .get_value("ctx")
        .ok_or_else(|| anyhow!("Recipe missing ctx"))?;

    let name = ctx_map
        .get("name")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_else(|| "package".to_string());

    if !has_fn(&ast, "remove") {
        return Err(anyhow!("{} has no remove function", name));
    }

    output::action(&format!("Removing {}", name));
    output::sub_action("remove");

    ctx_map = run_phase(engine, &ast, &mut scope, "remove", ctx_map)?;
    source = ctx::persist(&source, &ctx_map)?;
    fs::write(&recipe_path, &source)?;

    output::success(&format!("{} removed", name));
    Ok(ctx_map)
}

/// Clean up build artifacts
///
/// Returns the final ctx map after cleanup.
pub fn cleanup(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
    search_path: Option<&Path>,
) -> Result<rhai::Map> {
    let recipe_path = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let _lock = acquire_recipe_lock(&recipe_path)?;

    let (ast, mut source, base_dir) = compile_recipe(engine, &recipe_path, search_path)?;

    // Derive RECIPE_DIR from the recipe file's parent directory
    let recipe_dir = recipe_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    let mut scope = Scope::new();
    scope.push_constant("RECIPE_DIR", recipe_dir);
    if let Some(ref bd) = base_dir {
        scope.push_constant("BASE_RECIPE_DIR", bd.to_string_lossy().to_string());
    }
    scope.push_constant("BUILD_DIR", build_dir.to_string_lossy().to_string());

    // Run script to populate scope
    engine.run_ast_with_scope(&mut scope, &ast)?;

    let mut ctx_map: rhai::Map = scope
        .get_value("ctx")
        .ok_or_else(|| anyhow!("Recipe missing ctx"))?;

    let name = ctx_map
        .get("name")
        .and_then(|v| v.clone().into_string().ok())
        .unwrap_or_else(|| "package".to_string());

    if !has_fn(&ast, "cleanup") {
        return Err(anyhow!("{} has no cleanup function", name));
    }

    output::action(&format!("Cleaning up {}", name));
    output::sub_action("cleanup");

    ctx_map = run_phase(engine, &ast, &mut scope, "cleanup", ctx_map)?;
    source = ctx::persist(&source, &ctx_map)?;
    fs::write(&recipe_path, &source)?;

    output::success(&format!("{} cleaned", name));
    Ok(ctx_map)
}

/// Check if a phase check function throws (meaning the phase is needed)
fn check_throws(engine: &Engine, ast: &AST, scope: &Scope, fn_name: &str, ctx: &rhai::Map) -> bool {
    if !has_fn(ast, fn_name) {
        return true; // No check function = needs the phase
    }
    engine
        .call_fn::<rhai::Map>(&mut scope.clone(), ast, fn_name, (ctx.clone(),))
        .is_err()
}

/// Run a phase function and return the updated ctx
fn run_phase(
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

/// Check if AST has a function with the given name
fn has_fn(ast: &AST, name: &str) -> bool {
    ast.iter_functions().any(|f| f.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers;
    use tempfile::TempDir;

    fn create_engine() -> Engine {
        let mut engine = Engine::new();
        helpers::register_all(&mut engine);
        engine
    }

    #[test]
    fn test_install_minimal_recipe() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        let recipe_path = dir.path().join("test.rhai");
        fs::write(
            &recipe_path,
            r#"
let ctx = #{
    name: "test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }
fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#,
        )
        .unwrap();

        let engine = create_engine();
        let result = install(&engine, &build_dir, &recipe_path, &[], None);
        assert!(result.is_ok(), "Failed: {:?}", result);

        // Check ctx was persisted
        let content = fs::read_to_string(&recipe_path).unwrap();
        assert!(content.contains("installed: true"));
    }

    #[test]
    fn test_install_already_installed_skips() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        let recipe_path = dir.path().join("test.rhai");
        fs::write(
            &recipe_path,
            r#"
let ctx = #{
    name: "test",
};

fn is_installed(ctx) { ctx }
fn acquire(ctx) { throw "should not run"; }
fn install(ctx) { throw "should not run"; }
"#,
        )
        .unwrap();

        let engine = create_engine();
        let result = install(&engine, &build_dir, &recipe_path, &[], None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_has_fn() {
        let engine = Engine::new();
        let ast = engine.compile("fn foo() {} fn bar(x) { x }").unwrap();
        assert!(has_fn(&ast, "foo"));
        assert!(has_fn(&ast, "bar"));
        assert!(!has_fn(&ast, "baz"));
    }

    #[test]
    fn test_parse_extends() {
        assert_eq!(
            parse_extends("//! extends: base.rhai\nlet ctx = #{};"),
            Some("base.rhai".to_string())
        );
        assert_eq!(
            parse_extends("//! extends:  linux-base.rhai \nlet ctx = #{};"),
            Some("linux-base.rhai".to_string())
        );
        assert_eq!(
            parse_extends("// comment\n//! extends: base.rhai\nlet ctx = #{};"),
            Some("base.rhai".to_string())
        );
        assert_eq!(parse_extends("let ctx = #{};"), None);
        assert_eq!(
            parse_extends("\n\n//! extends: base.rhai"),
            Some("base.rhai".to_string())
        );
        // Non-comment line before extends stops parsing
        assert_eq!(parse_extends("let x = 1;\n//! extends: base.rhai"), None);
    }

    #[test]
    fn test_extends_merges_functions() {
        let dir = TempDir::new().unwrap();
        let build_dir = dir.path().join("build");
        fs::create_dir_all(&build_dir).unwrap();

        // Base recipe with acquire + install
        let base_path = dir.path().join("base.rhai");
        fs::write(
            &base_path,
            r#"
let ctx = #{
    name: "base",
    acquired: false,
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    ctx.acquired = true;
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#,
        )
        .unwrap();

        // Child recipe that extends base, overrides install
        let child_path = dir.path().join("child.rhai");
        fs::write(
            &child_path,
            r#"//! extends: base.rhai

let ctx = #{
    name: "child",
    acquired: false,
    installed: false,
    child_ran: false,
};

fn install(ctx) {
    ctx.installed = true;
    ctx.child_ran = true;
    ctx
}
"#,
        )
        .unwrap();

        let engine = create_engine();
        let result = install(&engine, &build_dir, &child_path, &[], None);
        assert!(result.is_ok(), "Failed: {:?}", result);

        let ctx = result.unwrap();
        // Child's install ran (child_ran = true)
        assert_eq!(ctx.get("child_ran").unwrap().as_bool().unwrap(), true);
        // Base's acquire ran (acquired = true)
        assert_eq!(ctx.get("acquired").unwrap().as_bool().unwrap(), true);
        // Name should be "child" (child ctx wins)
        assert_eq!(
            ctx.get("name").unwrap().clone().into_string().unwrap(),
            "child"
        );
    }

    #[test]
    fn test_extends_recursive_rejected() {
        let dir = TempDir::new().unwrap();

        let grandparent = dir.path().join("grandparent.rhai");
        fs::write(
            &grandparent,
            "//! extends: nonexistent.rhai\nlet ctx = #{};",
        )
        .unwrap();

        let child = dir.path().join("child.rhai");
        fs::write(&child, "//! extends: grandparent.rhai\nlet ctx = #{};").unwrap();

        let engine = create_engine();
        let result = compile_recipe(&engine, &child, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_extends_base_not_found() {
        let dir = TempDir::new().unwrap();
        let child = dir.path().join("child.rhai");
        fs::write(&child, "//! extends: nonexistent.rhai\nlet ctx = #{};").unwrap();

        let engine = create_engine();
        let result = compile_recipe(&engine, &child, None);
        assert!(result.is_err());
    }
}
