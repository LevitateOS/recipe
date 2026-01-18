//! Lifecycle orchestration for recipe execution
//!
//! The lifecycle flow:
//! 1. is_installed() - Check if already done (skip if true)
//! 2. acquire() - Get source materials
//! 3. build() - Compile/transform (optional)
//! 4. install() - Copy to PREFIX

use crate::engine::context::{clear_context, init_context};
use anyhow::{Context, Result};
use rhai::{Engine, Scope, AST};
use std::path::Path;

/// Execute a recipe following the lifecycle phases
pub fn execute(
    engine: &Engine,
    prefix: &Path,
    build_dir: &Path,
    recipe_path: &Path,
) -> Result<()> {
    let script = std::fs::read_to_string(recipe_path)
        .with_context(|| format!("Failed to read recipe: {}", recipe_path.display()))?;

    // Set up execution context
    init_context(prefix.to_path_buf(), build_dir.to_path_buf());

    // Create scope with variables
    let mut scope = Scope::new();
    scope.push_constant("PREFIX", prefix.to_string_lossy().to_string());
    scope.push_constant("BUILD_DIR", build_dir.to_string_lossy().to_string());
    scope.push_constant("ARCH", std::env::consts::ARCH);
    scope.push_constant("NPROC", num_cpus::get() as i64);
    scope.push_constant("RPM_PATH", std::env::var("RPM_PATH").unwrap_or_default());

    // Compile script
    let ast = engine
        .compile(&script)
        .map_err(|e| anyhow::anyhow!("Failed to compile recipe: {}", e))?;

    // Extract package name for logging
    let name = get_recipe_name(engine, &mut scope, &ast, recipe_path);

    // PHASE 1: Check if already installed (skip entire recipe if true)
    if has_action(&ast, "is_installed") {
        let installed = engine
            .call_fn::<bool>(&mut scope, &ast, "is_installed", ())
            .unwrap_or(false);

        if installed {
            println!("==> {} already installed, skipping", name);
            clear_context();
            return Ok(());
        }
    }

    println!("==> Installing {}", name);

    // PHASE 2: Acquire source materials
    println!("  -> acquire");
    call_action(engine, &mut scope, &ast, "acquire")?;

    // PHASE 3: Build (only if recipe defines it)
    if has_action(&ast, "build") {
        println!("  -> build");
        call_action(engine, &mut scope, &ast, "build")?;
    }

    // PHASE 4: Install to PREFIX
    println!("  -> install");
    call_action(engine, &mut scope, &ast, "install")?;

    // Clean up context
    clear_context();

    println!("==> {} installed", name);
    Ok(())
}

/// Get the recipe name from script variables or filename
fn get_recipe_name(engine: &Engine, scope: &mut Scope, ast: &AST, recipe_path: &Path) -> String {
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

/// Check if an action function exists in the AST
fn has_action(ast: &AST, name: &str) -> bool {
    ast.iter_functions().any(|f| f.name == name)
}

/// Call an action function in the recipe
fn call_action(engine: &Engine, scope: &mut Scope, ast: &AST, action: &str) -> Result<()> {
    if !has_action(ast, action) {
        return Err(anyhow::anyhow!("Action '{}' not defined", action));
    }

    engine
        .call_fn::<()>(scope, ast, action, ())
        .map_err(|e| anyhow::anyhow!("Action '{}' failed: {}", action, e))?;

    Ok(())
}
