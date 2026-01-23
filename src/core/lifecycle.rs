//! Lifecycle orchestration for recipe execution
//!
//! The lifecycle flow:
//! 1. is_installed() - Check if already done (skip if true)
//! 2. acquire() - Get source materials
//! 3. build() - Compile/transform (optional)
//! 4. install() - Copy to PREFIX

use super::context::{get_installed_files, init_context, ContextGuard};
use super::output;
use super::recipe_state::{self, OptionalString};
use anyhow::{Context, Result};
use fs2::FileExt;
use rhai::{Engine, Scope, AST};
use std::fs::File;
use std::path::Path;

/// Acquire an exclusive lock on a recipe file to prevent concurrent execution.
/// Returns a guard that releases the lock when dropped.
fn acquire_recipe_lock(recipe_path: &Path) -> Result<RecipeLock> {
    let lock_path = recipe_path.with_extension("rhai.lock");
    let lock_file = File::create(&lock_path)
        .with_context(|| format!("Failed to create lock file: {}", lock_path.display()))?;

    if let Err(_) = lock_file.try_lock_exclusive() {
        // Clean up the lock file we created before returning error
        // (the file exists but we couldn't acquire the lock)
        drop(lock_file); // Close the file handle first
        let _ = std::fs::remove_file(&lock_path);
        return Err(anyhow::anyhow!(
            "Recipe '{}' is already being executed by another process. \
             If this is incorrect, delete '{}'",
            recipe_path.display(),
            lock_path.display()
        ));
    }

    Ok(RecipeLock { _file: lock_file, path: lock_path })
}

/// RAII guard for recipe lock - releases lock and deletes lock file when dropped
struct RecipeLock {
    _file: File,
    path: std::path::PathBuf,
}

impl Drop for RecipeLock {
    fn drop(&mut self) {
        // Lock is automatically released when file is dropped
        // Clean up lock file
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Required variables that every recipe MUST define
const REQUIRED_VARS: &[&str] = &["name", "version", "installed"];

/// Required functions that every recipe MUST define
const REQUIRED_FUNCTIONS: &[&str] = &["acquire", "install"];

/// Validate that a recipe has all required variables and functions.
/// Returns an error with a clear message listing ALL missing items.
fn validate_recipe(engine: &Engine, ast: &AST, recipe_path: &Path) -> Result<()> {
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
        } else if name.clone().into_string().map(|s| s.is_empty()).unwrap_or(true) {
            errors.push("`name` cannot be empty".to_string());
        }
    }

    if let Some(version) = scope.get_value::<rhai::Dynamic>("version") {
        if !version.is_string() {
            errors.push(format!(
                "`version` must be a string, got: {}",
                version.type_name()
            ));
        } else if version.clone().into_string().map(|s| s.is_empty()).unwrap_or(true) {
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
            if scope.get_value::<rhai::Dynamic>("installed_version").is_none() {
                errors.push("missing `installed_version` (required when installed = true)".to_string());
            }
            if scope.get_value::<rhai::Dynamic>("installed_files").is_none() {
                errors.push("missing `installed_files` (required when installed = true)".to_string());
            }
        }
    }

    // Check required functions
    for func in REQUIRED_FUNCTIONS {
        if !has_action(ast, func) {
            errors.push(format!("missing required function: `fn {}() {{ ... }}`", func));
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

/// Compare two version strings semantically.
/// Returns true if `current` is the same as or newer than `installed`.
/// Falls back to string comparison for non-semver versions.
fn version_is_up_to_date(installed: Option<&str>, current: Option<&str>) -> bool {
    match (installed, current) {
        (None, None) => true,
        (Some(_), None) => false, // No current version but something installed
        (None, Some(_)) => false, // Current version but nothing installed
        (Some(installed), Some(current)) => {
            // Try semver parsing first
            if let (Ok(installed_ver), Ok(current_ver)) = (
                semver::Version::parse(installed.trim_start_matches('v')),
                semver::Version::parse(current.trim_start_matches('v')),
            ) {
                // Up to date if installed >= current (no upgrade needed)
                installed_ver >= current_ver
            } else {
                // Fall back to string comparison for non-semver versions
                installed == current
            }
        }
    }
}

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

    // Acquire exclusive lock to prevent concurrent execution
    let _lock = acquire_recipe_lock(&recipe_path_canonical)?;

    // Set up execution context
    // Use ContextGuard to ensure cleanup even if execution panics
    init_context(prefix.to_path_buf(), build_dir.to_path_buf());
    let _context_guard = ContextGuard::new();

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

    // VALIDATE: Check required variables and functions BEFORE doing anything else
    validate_recipe(engine, &ast, &recipe_path_canonical)?;

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
                output::skip(&format!("{} already installed, skipping", name));
                return Ok(());
            }
            // If is_installed() returns false, files might have been deleted
            // Continue with reinstall
        } else {
            output::skip(&format!("{} already installed, skipping", name));
            return Ok(());
        }
    } else if has_action(&ast, "is_installed") {
        // Fallback: check is_installed() function
        let installed = engine
            .call_fn::<bool>(&mut scope, &ast, "is_installed", ())
            .unwrap_or(false);

        if installed {
            output::skip(&format!("{} already installed, skipping", name));
            return Ok(());
        }
    }

    output::action(&format!("Installing {}", name));

    // PHASE 2: Acquire source materials
    output::sub_action("acquire");
    call_action(engine, &mut scope, &ast, "acquire")?;

    // PHASE 3: Build (only if recipe defines it)
    if has_action(&ast, "build") {
        output::sub_action("build");
        call_action(engine, &mut scope, &ast, "build")?;
    }

    // PRE-INSTALL HOOK (if defined)
    if has_action(&ast, "pre_install") {
        output::sub_action("pre_install");
        call_action(engine, &mut scope, &ast, "pre_install")?;
    }

    // PHASE 4: Install to PREFIX
    // Execute install phase - helpers track installed files in context
    output::sub_action("install");
    let install_result = call_action(engine, &mut scope, &ast, "install");

    // Get installed files regardless of success/failure (for cleanup on failure)
    let installed_files = get_installed_files();

    // Handle install failure - clean up any partially installed files
    if let Err(e) = install_result {
        output::warning("Install failed, cleaning up partially installed files...");
        for file in &installed_files {
            if file.exists() {
                let _ = std::fs::remove_file(file);
                output::detail(&format!("rm {}", file.display()));
            }
        }
        return Err(e);
    }

    // POST-INSTALL HOOK (if defined)
    if has_action(&ast, "post_install") {
        output::sub_action("post_install");
        if let Err(e) = call_action(engine, &mut scope, &ast, "post_install") {
            // Rollback installed files on post_install failure
            output::warning("Post-install failed, cleaning up...");
            for file in &installed_files {
                if file.exists() {
                    let _ = std::fs::remove_file(file);
                }
            }
            return Err(e);
        }
    }

    // Record installed state in recipe
    update_recipe_state(&recipe_path_canonical, &version, &installed_files)?;

    // Context cleanup handled by _context_guard Drop

    output::success(&format!("{} installed", name));
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

    // Acquire exclusive lock to prevent concurrent operations
    let _lock = acquire_recipe_lock(&recipe_path_canonical)?;

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

    output::action(&format!("Removing {}", name));

    // PRE-REMOVE HOOK (if defined) - runs before any files are deleted
    if has_action(&ast, "pre_remove") {
        output::sub_action("pre_remove");
        let _ = call_action(engine, &mut scope, &ast, "pre_remove");
    }

    // Get installed files list
    let installed_files: Option<Vec<String>> = recipe_state::get_var(&recipe_path_canonical, "installed_files")
        .unwrap_or(None);

    let files = installed_files.unwrap_or_default();

    // Run recipe's remove() function if defined (for custom cleanup during file deletion)
    if has_action(&ast, "remove") {
        output::sub_action("remove (custom cleanup)");
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
                    output::detail(&format!("rm {}", file));
                    deleted += 1;
                }
                Err(e) => {
                    output::warning(&format!("failed to remove {}: {}", file, e));
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

    // POST-REMOVE HOOK (if defined) - runs after all files are deleted
    if has_action(&ast, "post_remove") {
        output::sub_action("post_remove");
        let _ = call_action(engine, &mut scope, &ast, "post_remove");
    }

    output::success(&format!("{} removed ({} files)", name, deleted));

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
        output::detail(&format!("{} has no update checker", name));
        return Ok(None);
    }

    // Get current version
    let current_version = get_recipe_var(engine, &mut scope, &ast, "version");

    // Call check_update() which should return new version or ()
    let result = engine.call_fn::<rhai::Dynamic>(&mut scope, &ast, "check_update", ());

    match result {
        Ok(new_version) => {
            if new_version.is_unit() {
                output::detail(&format!("{} is up to date", name));
                return Ok(None);
            }

            if let Some(ver_str) = new_version.clone().try_cast::<String>() {
                if Some(&ver_str) != current_version.as_ref() {
                    output::info(&format!("{} {} -> {} available", name,
                        current_version.as_deref().unwrap_or("?"),
                        ver_str));

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

    // Compare versions semantically
    if version_is_up_to_date(installed_version.as_deref(), recipe_version.as_deref()) {
        output::skip(&format!("{} is up to date ({})", name, recipe_version.as_deref().unwrap_or("?")));
        return Ok(false);
    }

    output::action(&format!("Upgrading {} ({} -> {})",
        name,
        installed_version.as_deref().unwrap_or("?"),
        recipe_version.as_deref().unwrap_or("?")));

    // Remove old version
    remove(engine, prefix, recipe_path)?;

    // Install new version (need to reset installed state first)
    recipe_state::set_var(&recipe_path_canonical, "installed", &false)?;

    execute(engine, prefix, build_dir, recipe_path)?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RecipeEngine;
    use leviso_cheat_test::{cheat_aware, cheat_reviewed};
    use tempfile::TempDir;

    fn create_test_env() -> (TempDir, std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let prefix = dir.path().join("prefix");
        let build_dir = dir.path().join("build");
        let recipes_dir = dir.path().join("recipes");
        std::fs::create_dir_all(&prefix).unwrap();
        std::fs::create_dir_all(&build_dir).unwrap();
        std::fs::create_dir_all(&recipes_dir).unwrap();
        (dir, prefix, build_dir, recipes_dir)
    }

    fn write_recipe(recipes_dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = recipes_dir.join(format!("{}.rhai", name));
        std::fs::write(&path, content).unwrap();
        path
    }

    // ==================== validate_recipe tests ====================

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
        let (_dir, _prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "no-name", r#"
let version = "1.0";
fn acquire() {}
fn install() {}
"#);
        let engine = rhai::Engine::new();
        let ast = engine.compile(&std::fs::read_to_string(&recipe_path).unwrap()).unwrap();
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
        let (_dir, _prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "no-version", r#"
let name = "test";
fn acquire() {}
fn install() {}
"#);
        let engine = rhai::Engine::new();
        let ast = engine.compile(&std::fs::read_to_string(&recipe_path).unwrap()).unwrap();
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
        let (_dir, _prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "no-acquire", r#"
let name = "test";
let version = "1.0";
fn install() {}
"#);
        let engine = rhai::Engine::new();
        let ast = engine.compile(&std::fs::read_to_string(&recipe_path).unwrap()).unwrap();
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
        let (_dir, _prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "no-install", r#"
let name = "test";
let version = "1.0";
fn acquire() {}
"#);
        let engine = rhai::Engine::new();
        let ast = engine.compile(&std::fs::read_to_string(&recipe_path).unwrap()).unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("missing required function"));
        assert!(err.contains("install"));
    }

    #[cheat_reviewed("Validation test - multiple errors reported at once")]
    #[test]
    fn test_validate_recipe_multiple_errors() {
        let (_dir, _prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "many-errors", r#"
// Completely empty recipe - missing everything
let x = 1;
"#);
        let engine = rhai::Engine::new();
        let ast = engine.compile(&std::fs::read_to_string(&recipe_path).unwrap()).unwrap();
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
        let (_dir, _prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "empty-name", r#"
let name = "";
let version = "1.0";
let installed = false;
fn acquire() {}
fn install() {}
"#);
        let engine = rhai::Engine::new();
        let ast = engine.compile(&std::fs::read_to_string(&recipe_path).unwrap()).unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cannot be empty"));
    }

    #[cheat_reviewed("Validation test - name must be string type")]
    #[test]
    fn test_validate_recipe_wrong_type_name() {
        let (_dir, _prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "wrong-type", r#"
let name = 123;  // Should be string
let version = "1.0";
let installed = false;
fn acquire() {}
fn install() {}
"#);
        let engine = rhai::Engine::new();
        let ast = engine.compile(&std::fs::read_to_string(&recipe_path).unwrap()).unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must be a string"));
    }

    #[cheat_reviewed("Validation test - installed field required")]
    #[test]
    fn test_validate_recipe_missing_installed() {
        let (_dir, _prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "no-installed", r#"
let name = "test";
let version = "1.0";
fn acquire() {}
fn install() {}
"#);
        let engine = rhai::Engine::new();
        let ast = engine.compile(&std::fs::read_to_string(&recipe_path).unwrap()).unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("installed"));
    }

    #[cheat_reviewed("Validation test - installed must be boolean")]
    #[test]
    fn test_validate_recipe_installed_wrong_type() {
        let (_dir, _prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "installed-wrong-type", r#"
let name = "test";
let version = "1.0";
let installed = "yes";  // Should be boolean
fn acquire() {}
fn install() {}
"#);
        let engine = rhai::Engine::new();
        let ast = engine.compile(&std::fs::read_to_string(&recipe_path).unwrap()).unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must be a boolean"));
    }

    #[cheat_reviewed("Validation test - installed=true requires installed_version")]
    #[test]
    fn test_validate_recipe_installed_true_missing_version() {
        let (_dir, _prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "installed-no-version", r#"
let name = "test";
let version = "1.0";
let installed = true;
let installed_files = [];
fn acquire() {}
fn install() {}
"#);
        let engine = rhai::Engine::new();
        let ast = engine.compile(&std::fs::read_to_string(&recipe_path).unwrap()).unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("installed_version"));
        assert!(err.contains("required when installed = true"));
    }

    #[cheat_reviewed("Validation test - installed=true requires installed_files")]
    #[test]
    fn test_validate_recipe_installed_true_missing_files() {
        let (_dir, _prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "installed-no-files", r#"
let name = "test";
let version = "1.0";
let installed = true;
let installed_version = "1.0";
fn acquire() {}
fn install() {}
"#);
        let engine = rhai::Engine::new();
        let ast = engine.compile(&std::fs::read_to_string(&recipe_path).unwrap()).unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("installed_files"));
        assert!(err.contains("required when installed = true"));
    }

    #[cheat_reviewed("Validation test - installed=true with all required fields passes")]
    #[test]
    fn test_validate_recipe_installed_true_valid() {
        let (_dir, _prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "installed-valid", r#"
let name = "test";
let version = "1.0";
let installed = true;
let installed_version = "1.0";
let installed_files = ["/usr/bin/test"];
fn acquire() {}
fn install() {}
"#);
        let engine = rhai::Engine::new();
        let ast = engine.compile(&std::fs::read_to_string(&recipe_path).unwrap()).unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_ok());
    }

    #[cheat_reviewed("Validation test - complete valid recipe passes")]
    #[test]
    fn test_validate_recipe_valid() {
        let (_dir, _prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "valid", r#"
let name = "test-package";
let version = "1.0.0";
let installed = false;
let description = "A test package";  // Optional
fn acquire() {}
fn install() {}
"#);
        let engine = rhai::Engine::new();
        let ast = engine.compile(&std::fs::read_to_string(&recipe_path).unwrap()).unwrap();
        let result = validate_recipe(&engine, &ast, &recipe_path);
        assert!(result.is_ok());
    }

    // ==================== has_action tests ====================

    #[cheat_reviewed("API test - has_action detects existing functions")]
    #[test]
    fn test_has_action_exists() {
        let engine = rhai::Engine::new();
        let ast = engine.compile("fn acquire() {} fn install() {}").unwrap();
        assert!(has_action(&ast, "acquire"));
        assert!(has_action(&ast, "install"));
    }

    #[cheat_reviewed("API test - has_action returns false for missing functions")]
    #[test]
    fn test_has_action_missing() {
        let engine = rhai::Engine::new();
        let ast = engine.compile("fn acquire() {}").unwrap();
        assert!(!has_action(&ast, "install"));
        assert!(!has_action(&ast, "build"));
    }

    #[cheat_reviewed("API test - has_action on script with no functions")]
    #[test]
    fn test_has_action_empty_script() {
        let engine = rhai::Engine::new();
        let ast = engine.compile("let x = 1;").unwrap();
        assert!(!has_action(&ast, "acquire"));
    }

    // ==================== get_recipe_name tests ====================

    #[cheat_reviewed("API test - recipe name extracted from variable")]
    #[test]
    fn test_get_recipe_name_from_variable() {
        let engine = rhai::Engine::new();
        let ast = engine.compile(r#"let name = "my-package";"#).unwrap();
        let mut scope = rhai::Scope::new();
        let name = get_recipe_name(&engine, &mut scope, &ast, Path::new("/test/fallback.rhai"));
        assert_eq!(name, "my-package");
    }

    #[cheat_reviewed("API test - recipe name falls back to filename")]
    #[test]
    fn test_get_recipe_name_fallback_to_filename() {
        let engine = rhai::Engine::new();
        let ast = engine.compile("let version = \"1.0\";").unwrap();
        let mut scope = rhai::Scope::new();
        let name = get_recipe_name(&engine, &mut scope, &ast, Path::new("/test/fallback-pkg.rhai"));
        assert_eq!(name, "fallback-pkg");
    }

    // ==================== call_action tests ====================

    #[cheat_reviewed("API test - call_action succeeds on valid function")]
    #[test]
    fn test_call_action_success() {
        let engine = rhai::Engine::new();
        let ast = engine.compile("fn test_action() { let x = 1; }").unwrap();
        let mut scope = rhai::Scope::new();
        let result = call_action(&engine, &mut scope, &ast, "test_action");
        assert!(result.is_ok());
    }

    #[cheat_reviewed("API test - call_action fails on missing function")]
    #[test]
    fn test_call_action_missing() {
        let engine = rhai::Engine::new();
        let ast = engine.compile("fn other() {}").unwrap();
        let mut scope = rhai::Scope::new();
        let result = call_action(&engine, &mut scope, &ast, "missing");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not defined"));
    }

    #[cheat_reviewed("API test - call_action propagates runtime errors")]
    #[test]
    fn test_call_action_runtime_error() {
        let engine = rhai::Engine::new();
        // This will cause a runtime error (undefined variable)
        let ast = engine.compile("fn bad_action() { undefined_var }").unwrap();
        let mut scope = rhai::Scope::new();
        let result = call_action(&engine, &mut scope, &ast, "bad_action");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("failed"));
    }

    // ==================== remove tests ====================

    #[cheat_aware(
        protects = "User is warned when trying to remove uninstalled package",
        severity = "MEDIUM",
        ease = "EASY",
        cheats = [
            "Don't check installed state before remove",
            "Return success even if not installed",
            "Silently skip uninstalled packages"
        ],
        consequence = "User runs 'recipe remove pkg' on uninstalled package - confusing success message"
    )]
    #[test]
    fn test_remove_not_installed() {
        let (_dir, prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "test", r#"
let name = "test";
let installed = false;
fn acquire() {}
fn install() {}
"#);
        let engine = RecipeEngine::new(prefix.clone(), _build_dir);
        let result = remove(&engine.engine, &prefix, &recipe_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not installed"));
    }

    #[cheat_reviewed("Remove test - empty installed_files list handled")]
    #[test]
    fn test_remove_with_no_files() {
        let (_dir, prefix, _build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "test", r#"
let name = "test";
let installed = true;
let installed_files = [];
fn acquire() {}
fn install() {}
"#);
        let engine = RecipeEngine::new(prefix.clone(), _build_dir);
        let result = remove(&engine.engine, &prefix, &recipe_path);
        assert!(result.is_ok());

        // Verify state was updated
        let installed: Option<bool> = recipe_state::get_var(&recipe_path, "installed").unwrap();
        assert_eq!(installed, Some(false));
    }

    #[cheat_aware(
        protects = "User's installed files are actually deleted on remove",
        severity = "HIGH",
        ease = "EASY",
        cheats = [
            "Mark as uninstalled but don't delete files",
            "Delete from list but not from filesystem",
            "Silently skip files that don't exist"
        ],
        consequence = "User removes package - files remain on disk, wasting space and causing conflicts"
    )]
    #[test]
    fn test_remove_deletes_files() {
        let (_dir, prefix, _build_dir, recipes_dir) = create_test_env();

        // Create a file to be "installed"
        let bin_dir = prefix.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let test_file = bin_dir.join("test-binary");
        std::fs::write(&test_file, "binary content").unwrap();

        let recipe_path = write_recipe(&recipes_dir, "test", &format!(r#"
let name = "test";
let installed = true;
let installed_files = ["{}"];
fn acquire() {{}}
fn install() {{}}
"#, test_file.display()));

        let engine = RecipeEngine::new(prefix.clone(), _build_dir);
        let result = remove(&engine.engine, &prefix, &recipe_path);
        assert!(result.is_ok());

        // File should be deleted
        assert!(!test_file.exists());
    }

    #[cheat_reviewed("Remove test - partial failure preserves installed state")]
    #[test]
    fn test_remove_partial_failure_preserves_state() {
        let (_dir, prefix, _build_dir, recipes_dir) = create_test_env();

        // Create a directory instead of a file (can't remove with remove_file)
        let bin_dir = prefix.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let non_removable = bin_dir.join("subdir");
        std::fs::create_dir(&non_removable).unwrap();
        // Put a file inside so the directory isn't empty
        std::fs::write(non_removable.join("file"), "content").unwrap();

        let recipe_path = write_recipe(&recipes_dir, "test", &format!(r#"
let name = "test";
let installed = true;
let installed_files = ["{}"];
fn acquire() {{}}
fn install() {{}}
"#, non_removable.display()));

        let engine = RecipeEngine::new(prefix.clone(), _build_dir);
        let result = remove(&engine.engine, &prefix, &recipe_path);

        // Should fail because we can't remove a directory with remove_file
        assert!(result.is_err());

        // State should be preserved (still installed)
        let installed: Option<bool> = recipe_state::get_var(&recipe_path, "installed").unwrap();
        assert_eq!(installed, Some(true));
    }

    // ==================== update tests ====================

    #[cheat_reviewed("Update test - no check_update function returns None")]
    #[test]
    fn test_update_no_check_update_function() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "test", r#"
let name = "test";
let version = "1.0";
fn acquire() {}
fn install() {}
"#);
        let engine = RecipeEngine::new(prefix, build_dir);
        let result = update(&engine.engine, &recipe_path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[cheat_reviewed("Update test - check_update returning unit means no update")]
    #[test]
    fn test_update_returns_unit_no_update() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "test", r#"
let name = "test";
let version = "1.0";
fn acquire() {}
fn install() {}
fn check_update() { () }
"#);
        let engine = RecipeEngine::new(prefix, build_dir);
        let result = update(&engine.engine, &recipe_path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[cheat_aware(
        protects = "User gets correct new version when update available",
        severity = "HIGH",
        ease = "MEDIUM",
        cheats = [
            "Return check_update result but don't update version variable",
            "Update version variable but don't return it",
            "Silently ignore check_update return value"
        ],
        consequence = "User runs 'recipe update pkg' - sees new version but install uses old version"
    )]
    #[test]
    fn test_update_returns_new_version() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "test", r#"
let name = "test";
let version = "1.0";
fn acquire() {}
fn install() {}
fn check_update() { "2.0" }
"#);
        let engine = RecipeEngine::new(prefix, build_dir);
        let result = update(&engine.engine, &recipe_path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("2.0".to_string()));

        // Version should be updated in recipe
        let version: Option<String> = recipe_state::get_var(&recipe_path, "version").unwrap();
        assert_eq!(version, Some("2.0".to_string()));
    }

    #[cheat_reviewed("Update test - check_update errors propagated")]
    #[test]
    fn test_update_check_fails() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "test", r#"
let name = "test";
let version = "1.0";
fn acquire() {}
fn install() {}
fn check_update() { undefined_var }
"#);
        let engine = RecipeEngine::new(prefix, build_dir);
        let result = update(&engine.engine, &recipe_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("update check failed"));
    }

    // ==================== upgrade tests ====================

    #[cheat_aware(
        protects = "User is warned when trying to upgrade uninstalled package",
        severity = "MEDIUM",
        ease = "EASY",
        cheats = [
            "Skip installed check and run upgrade anyway",
            "Return success without doing anything",
            "Install instead of upgrade without warning"
        ],
        consequence = "User runs 'recipe upgrade pkg' on uninstalled package - confusing behavior"
    )]
    #[test]
    fn test_upgrade_not_installed() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "test", r#"
let name = "test";
let version = "1.0";
let installed = false;
fn acquire() {}
fn install() {}
"#);
        let engine = RecipeEngine::new(prefix, build_dir);
        let result = upgrade(&engine.engine, &engine.prefix, &engine.build_dir, &recipe_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not installed"));
    }

    #[cheat_reviewed("Upgrade test - up-to-date package returns false")]
    #[test]
    fn test_upgrade_already_up_to_date() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(&recipes_dir, "test", r#"
let name = "test";
let version = "1.0";
let installed = true;
let installed_version = "1.0";
let installed_files = [];
fn acquire() {}
fn install() {}
"#);
        let engine = RecipeEngine::new(prefix, build_dir);
        let result = upgrade(&engine.engine, &engine.prefix, &engine.build_dir, &recipe_path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), false); // No upgrade performed
    }

    // ==================== cleanup_empty_dirs tests ====================

    #[cheat_reviewed("Cleanup test - empty directories removed")]
    #[test]
    fn test_cleanup_empty_dirs_removes_empty() {
        let (_dir, prefix, _build_dir, _recipes_dir) = create_test_env();

        // Create nested empty directories
        let nested = prefix.join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();

        let files = vec![nested.join("file.txt").to_string_lossy().to_string()];

        cleanup_empty_dirs(&files, &prefix);

        // All empty directories should be removed
        assert!(!prefix.join("a/b/c").exists());
        assert!(!prefix.join("a/b").exists());
        assert!(!prefix.join("a").exists());
    }

    #[cheat_reviewed("Cleanup test - non-empty directories preserved")]
    #[test]
    fn test_cleanup_empty_dirs_preserves_nonempty() {
        let (_dir, prefix, _build_dir, _recipes_dir) = create_test_env();

        // Create directories with one containing a file
        let a = prefix.join("a");
        let b = a.join("b");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("keep.txt"), "content").unwrap();

        let files = vec![b.join("deleted.txt").to_string_lossy().to_string()];

        cleanup_empty_dirs(&files, &prefix);

        // "a" should still exist (has keep.txt), "b" should be removed
        assert!(a.exists());
        assert!(!b.exists());
    }

    #[cheat_reviewed("Cleanup test - stops at prefix directory, doesn't delete it")]
    #[test]
    fn test_cleanup_empty_dirs_stops_at_prefix() {
        let (_dir, prefix, _build_dir, _recipes_dir) = create_test_env();

        let nested = prefix.join("a");
        std::fs::create_dir_all(&nested).unwrap();

        let files = vec![nested.join("file.txt").to_string_lossy().to_string()];

        cleanup_empty_dirs(&files, &prefix);

        // "a" removed but prefix itself should remain
        assert!(!nested.exists());
        assert!(prefix.exists());
    }
}
