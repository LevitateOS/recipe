//! Lifecycle orchestration for recipe execution
//!
//! The lifecycle flow:
//! 1. is_installed() - Check if already done (skip if true)
//! 2. acquire() - Get source materials
//! 3. build() - Compile/transform (optional)
//! 4. install() - Copy to PREFIX

use crate::engine::context::{clear_context, get_installed_files, init_context_with_recipe};
use crate::engine::recipe_state::{self, OptionalString};
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

    // Canonicalize recipe path for state tracking
    let recipe_path_canonical = recipe_path.canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    // Set up execution context with recipe path
    init_context_with_recipe(
        prefix.to_path_buf(),
        build_dir.to_path_buf(),
        Some(recipe_path_canonical.clone()),
    );

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

    // Extract package info for logging and state
    let name = get_recipe_name(engine, &mut scope, &ast, recipe_path);
    let version = get_recipe_var(engine, &mut scope, &ast, "version");

    // PHASE 1: Check if already installed
    // First check the recipe's `installed` state variable
    let installed_state: Option<bool> = recipe_state::get_var(&recipe_path_canonical, "installed")
        .unwrap_or(None);

    if installed_state == Some(true) {
        // Already installed according to state, but check is_installed() if defined
        if has_action(&ast, "is_installed") {
            let still_installed = engine
                .call_fn::<bool>(&mut scope, &ast, "is_installed", ())
                .unwrap_or(false);

            if still_installed {
                println!("==> {} already installed, skipping", name);
                clear_context();
                return Ok(());
            }
            // If is_installed() returns false, files might have been deleted
            // Continue with reinstall
        } else {
            println!("==> {} already installed, skipping", name);
            clear_context();
            return Ok(());
        }
    } else if has_action(&ast, "is_installed") {
        // Fallback: check is_installed() function
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

    // Record installed state in recipe
    let installed_files = get_installed_files();
    update_recipe_state(&recipe_path_canonical, &version, &installed_files)?;

    // Clean up context
    clear_context();

    println!("==> {} installed", name);
    Ok(())
}

/// Update recipe state variables after successful install
fn update_recipe_state(recipe_path: &Path, version: &Option<String>, installed_files: &[std::path::PathBuf]) -> Result<()> {
    // Set installed = true
    recipe_state::set_var(recipe_path, "installed", &true)
        .with_context(|| "Failed to set installed state")?;

    // Set installed_version
    if let Some(ver) = version {
        recipe_state::set_var(recipe_path, "installed_version", &OptionalString::Some(ver.clone()))
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

/// Get a string variable from the recipe
fn get_recipe_var(engine: &Engine, scope: &mut Scope, ast: &AST, var_name: &str) -> Option<String> {
    // Run the script to populate scope
    let mut test_scope = scope.clone();
    engine.run_ast_with_scope(&mut test_scope, ast).ok()?;
    test_scope.get_value::<String>(var_name)
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

/// Remove a package by deleting its installed files
pub fn remove(
    engine: &Engine,
    prefix: &Path,
    recipe_path: &Path,
) -> Result<()> {
    let recipe_path_canonical = recipe_path.canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    // Check if package is installed
    let installed: Option<bool> = recipe_state::get_var(&recipe_path_canonical, "installed")
        .unwrap_or(None);

    if installed != Some(true) {
        anyhow::bail!("Package is not installed");
    }

    // Get package name for logging
    let script = std::fs::read_to_string(&recipe_path_canonical)
        .with_context(|| format!("Failed to read recipe: {}", recipe_path.display()))?;

    let ast = engine
        .compile(&script)
        .map_err(|e| anyhow::anyhow!("Failed to compile recipe: {}", e))?;

    let mut scope = Scope::new();
    scope.push_constant("PREFIX", prefix.to_string_lossy().to_string());

    let name = get_recipe_name(engine, &mut scope, &ast, recipe_path);

    println!("==> Removing {}", name);

    // Get installed files list
    let installed_files: Option<Vec<String>> = recipe_state::get_var(&recipe_path_canonical, "installed_files")
        .unwrap_or(None);

    let files = installed_files.unwrap_or_default();

    // Run recipe's remove() function if defined (for custom cleanup)
    if has_action(&ast, "remove") {
        println!("  -> remove (custom cleanup)");
        let _ = call_action(engine, &mut scope, &ast, "remove");
    }

    // Delete installed files
    let mut deleted = 0;
    let mut failed = 0;
    for file in &files {
        let path = std::path::Path::new(file);
        if path.exists() {
            match std::fs::remove_file(path) {
                Ok(()) => {
                    println!("     rm {}", file);
                    deleted += 1;
                }
                Err(e) => {
                    eprintln!("     failed to remove {}: {}", file, e);
                    failed += 1;
                }
            }
        }
    }

    // If any files failed to delete, don't mark as uninstalled
    if failed > 0 {
        anyhow::bail!(
            "Failed to remove {} of {} files for {}. Package state unchanged. \
             Fix permissions or run with sudo, then try again.",
            failed, files.len(), name
        );
    }

    // Clean up empty directories
    cleanup_empty_dirs(&files, prefix);

    // Only update recipe state if ALL files were removed successfully
    recipe_state::set_var(&recipe_path_canonical, "installed", &false)
        .with_context(|| "Failed to update installed state")?;
    recipe_state::set_var(&recipe_path_canonical, "installed_version", &OptionalString::None)
        .with_context(|| "Failed to clear installed_version")?;
    recipe_state::set_var(&recipe_path_canonical, "installed_at", &OptionalString::None)
        .with_context(|| "Failed to clear installed_at")?;
    recipe_state::set_var(&recipe_path_canonical, "installed_files", &Vec::<String>::new())
        .with_context(|| "Failed to clear installed_files")?;

    println!("==> {} removed ({} files)", name, deleted);

    Ok(())
}

/// Clean up empty directories after file removal
fn cleanup_empty_dirs(files: &[String], prefix: &Path) {
    use std::collections::HashSet;

    // Collect all parent directories
    let mut dirs: HashSet<std::path::PathBuf> = HashSet::new();
    for file in files {
        let mut path = std::path::Path::new(file).to_path_buf();
        while let Some(parent) = path.parent() {
            if !parent.starts_with(prefix) || parent == prefix {
                break;
            }
            dirs.insert(parent.to_path_buf());
            path = parent.to_path_buf();
        }
    }

    // Sort by depth (deepest first) and try to remove empty ones
    let mut dirs: Vec<_> = dirs.into_iter().collect();
    dirs.sort_by(|a, b| b.components().count().cmp(&a.components().count()));

    for dir in dirs {
        if dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                if entries.count() == 0 {
                    let _ = std::fs::remove_dir(&dir);
                }
            }
        }
    }
}

/// Update a package (check for new versions)
pub fn update(
    engine: &Engine,
    recipe_path: &Path,
) -> Result<Option<String>> {
    let recipe_path_canonical = recipe_path.canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    let script = std::fs::read_to_string(&recipe_path_canonical)
        .with_context(|| format!("Failed to read recipe: {}", recipe_path.display()))?;

    let ast = engine
        .compile(&script)
        .map_err(|e| anyhow::anyhow!("Failed to compile recipe: {}", e))?;

    let mut scope = Scope::new();
    let name = get_recipe_name(engine, &mut scope, &ast, recipe_path);

    // Check if recipe has check_update function
    if !has_action(&ast, "check_update") {
        println!("  {} has no update checker", name);
        return Ok(None);
    }

    // Get current version
    let current_version = get_recipe_var(engine, &mut scope, &ast, "version");

    // Call check_update() which should return new version or ()
    let result = engine.call_fn::<rhai::Dynamic>(&mut scope, &ast, "check_update", ());

    match result {
        Ok(new_version) => {
            if new_version.is_unit() {
                println!("  {} is up to date", name);
                return Ok(None);
            }

            if let Some(ver_str) = new_version.clone().try_cast::<String>() {
                if Some(&ver_str) != current_version.as_ref() {
                    println!("  {} {} -> {} available", name,
                        current_version.as_deref().unwrap_or("?"),
                        ver_str);

                    // Update the version variable in the recipe
                    recipe_state::set_var(&recipe_path_canonical, "version", &ver_str)
                        .with_context(|| "Failed to update version")?;

                    return Ok(Some(ver_str));
                }
            }

            Ok(None)
        }
        Err(e) => {
            Err(anyhow::anyhow!("{} update check failed: {}", name, e))
        }
    }
}

/// Upgrade a package (reinstall if new version available)
pub fn upgrade(
    engine: &Engine,
    prefix: &Path,
    build_dir: &Path,
    recipe_path: &Path,
) -> Result<bool> {
    let recipe_path_canonical = recipe_path.canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    // Check if installed
    let installed: Option<bool> = recipe_state::get_var(&recipe_path_canonical, "installed")
        .unwrap_or(None);

    if installed != Some(true) {
        anyhow::bail!("Package is not installed");
    }

    // Get current version from recipe and installed version
    let script = std::fs::read_to_string(&recipe_path_canonical)
        .with_context(|| format!("Failed to read recipe: {}", recipe_path.display()))?;

    let ast = engine
        .compile(&script)
        .map_err(|e| anyhow::anyhow!("Failed to compile recipe: {}", e))?;

    let mut scope = Scope::new();
    let name = get_recipe_name(engine, &mut scope, &ast, recipe_path);
    let recipe_version = get_recipe_var(engine, &mut scope, &ast, "version");

    let installed_version: Option<OptionalString> = recipe_state::get_var(&recipe_path_canonical, "installed_version")
        .unwrap_or(None);
    let installed_version: Option<String> = installed_version.and_then(|v| v.into());

    // Compare versions
    if recipe_version == installed_version {
        println!("==> {} is up to date ({})", name, recipe_version.as_deref().unwrap_or("?"));
        return Ok(false);
    }

    println!("==> Upgrading {} ({} -> {})",
        name,
        installed_version.as_deref().unwrap_or("?"),
        recipe_version.as_deref().unwrap_or("?"));

    // Remove old version
    remove(engine, prefix, recipe_path)?;

    // Install new version (need to reset installed state first)
    recipe_state::set_var(&recipe_path_canonical, "installed", &false)?;

    execute(engine, prefix, build_dir, recipe_path)?;

    Ok(true)
}
