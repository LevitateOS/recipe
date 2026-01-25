//! Lifecycle orchestration for recipe execution
//!
//! The lifecycle flow:
//! 1. is_installed() - Check if already done (skip if true)
//! 2. acquire() - Get source materials
//! 3. build() - Compile/transform (optional)
//! 4. install() - Copy to PREFIX (via staging for atomicity)

mod action;
mod cleanup;
mod commit;
mod lock;
mod state;
mod validation;
mod version_cmp;

pub use action::{call_action, has_action};
pub use cleanup::cleanup_empty_dirs;
pub use commit::{cleanup_staging_dir, commit_staged_files, create_staging_dir};
pub use lock::acquire_recipe_lock;
pub use state::{clear_recipe_state, get_recipe_name, get_recipe_var, update_recipe_state};
pub use validation::validate_recipe;
pub use version_cmp::is_upgrade_needed;

use super::context::{init_context, ContextGuard};
use super::output;
use super::recipe_state::{self, OptionalString};
use anyhow::{Context, Result};
use rhai::{Engine, Scope};
use std::path::Path;

/// Execute a recipe following the lifecycle phases
///
/// This function uses atomic staging to ensure PREFIX is never left in a partial
/// state. Files are first installed to a staging directory, then committed
/// atomically to PREFIX after successful installation.
pub fn execute(engine: &Engine, prefix: &Path, build_dir: &Path, recipe_path: &Path) -> Result<()> {
    let script = std::fs::read_to_string(recipe_path)
        .with_context(|| format!("Failed to read recipe: {}", recipe_path.display()))?;

    // Canonicalize recipe path for state tracking
    let recipe_path_canonical = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    // Acquire exclusive lock to prevent concurrent execution
    let _lock = acquire_recipe_lock(&recipe_path_canonical)?;

    // Create staging directory for atomic installs
    let stage_dir = create_staging_dir(build_dir)?;

    // Set up execution context - install helpers write to staging dir, not prefix
    init_context(stage_dir.clone(), build_dir.to_path_buf());
    let _context_guard = ContextGuard::new();

    // Create scope with variables
    // Note: PREFIX points to staging dir so install helpers write there
    let mut scope = Scope::new();
    scope.push_constant("PREFIX", stage_dir.to_string_lossy().to_string());
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
    let installed_state: Option<bool> =
        recipe_state::get_var(&recipe_path_canonical, "installed").unwrap_or(None);

    if installed_state == Some(true) {
        // Already installed according to state, but check is_installed() if defined
        if has_action(&ast, "is_installed") {
            let still_installed = engine
                .call_fn::<bool>(&mut scope, &ast, "is_installed", ())
                .unwrap_or(false);

            if still_installed {
                cleanup_staging_dir(&stage_dir);
                output::skip(&format!("{} already installed, skipping", name));
                return Ok(());
            }
            // If is_installed() returns false, files might have been deleted
            // Continue with reinstall
        } else {
            cleanup_staging_dir(&stage_dir);
            output::skip(&format!("{} already installed, skipping", name));
            return Ok(());
        }
    } else if has_action(&ast, "is_installed") {
        // Fallback: check is_installed() function
        let installed = engine
            .call_fn::<bool>(&mut scope, &ast, "is_installed", ())
            .unwrap_or(false);

        if installed {
            cleanup_staging_dir(&stage_dir);
            output::skip(&format!("{} already installed, skipping", name));
            return Ok(());
        }
    }

    output::action(&format!("Installing {}", name));

    // PHASE 2: Acquire source materials
    output::sub_action("acquire");
    if let Err(e) = call_action(engine, &mut scope, &ast, "acquire") {
        cleanup_staging_dir(&stage_dir);
        return Err(e);
    }

    // PHASE 3: Build (only if recipe defines it)
    if has_action(&ast, "build") {
        output::sub_action("build");
        if let Err(e) = call_action(engine, &mut scope, &ast, "build") {
            cleanup_staging_dir(&stage_dir);
            return Err(e);
        }
    }

    // PRE-INSTALL HOOK (if defined)
    if has_action(&ast, "pre_install") {
        output::sub_action("pre_install");
        if let Err(e) = call_action(engine, &mut scope, &ast, "pre_install") {
            cleanup_staging_dir(&stage_dir);
            return Err(e);
        }
    }

    // PHASE 4: Install to staging directory
    output::sub_action("install");
    if let Err(e) = call_action(engine, &mut scope, &ast, "install") {
        // Install failed - staging directory is cleaned up, PREFIX untouched
        cleanup_staging_dir(&stage_dir);
        return Err(e);
    }

    // POST-INSTALL HOOK (if defined) - runs before commit
    if has_action(&ast, "post_install") {
        output::sub_action("post_install");
        if let Err(e) = call_action(engine, &mut scope, &ast, "post_install") {
            cleanup_staging_dir(&stage_dir);
            return Err(e);
        }
    }

    // ATOMIC COMMIT: Move staged files to real PREFIX
    output::sub_action("commit");
    let committed_files = commit_staged_files(&stage_dir, prefix)
        .with_context(|| "Failed to commit staged files to prefix")?;

    // Record installed state in recipe
    update_recipe_state(&recipe_path_canonical, &version, &committed_files)?;

    output::success(&format!("{} installed ({} files)", name, committed_files.len()));
    Ok(())
}

/// Remove an installed package
pub fn remove(engine: &Engine, prefix: &Path, recipe_path: &Path) -> Result<()> {
    let recipe_path_canonical = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    // Acquire exclusive lock to prevent concurrent operations
    let _lock = acquire_recipe_lock(&recipe_path_canonical)?;

    // Check if package is installed
    let installed: Option<bool> =
        recipe_state::get_var(&recipe_path_canonical, "installed").unwrap_or(None);

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
    let installed_files: Option<Vec<String>> =
        recipe_state::get_var(&recipe_path_canonical, "installed_files").unwrap_or(None);

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
            failed,
            files.len(),
            name
        );
    }

    // Clean up empty directories
    cleanup_empty_dirs(&files, prefix);

    // Only update recipe state if ALL files were removed successfully
    clear_recipe_state(&recipe_path_canonical)?;

    // POST-REMOVE HOOK (if defined) - runs after all files are deleted
    if has_action(&ast, "post_remove") {
        output::sub_action("post_remove");
        let _ = call_action(engine, &mut scope, &ast, "post_remove");
    }

    output::success(&format!("{} removed ({} files)", name, deleted));

    Ok(())
}

/// Update a package (check for new versions)
pub fn update(engine: &Engine, recipe_path: &Path) -> Result<Option<String>> {
    let recipe_path_canonical = recipe_path
        .canonicalize()
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

            if let Some(ver_str) = new_version.clone().try_cast::<String>()
                && Some(&ver_str) != current_version.as_ref()
            {
                output::info(&format!(
                    "{} {} -> {} available",
                    name,
                    current_version.as_deref().unwrap_or("?"),
                    ver_str
                ));

                // Update the version variable in the recipe
                recipe_state::set_var(&recipe_path_canonical, "version", &ver_str)
                    .with_context(|| "Failed to update version")?;

                return Ok(Some(ver_str));
            }

            Ok(None)
        }
        Err(e) => Err(anyhow::anyhow!("{} update check failed: {}", name, e)),
    }
}

/// Resolve a dependency - calls resolve() and returns the path
///
/// This is a lightweight lifecycle that doesn't require name/version/installed fields.
/// The recipe only needs to define a `fn resolve() -> String` that returns the path.
pub fn resolve(
    engine: &Engine,
    build_dir: &Path,
    recipe_path: &Path,
) -> Result<std::path::PathBuf> {
    let script = std::fs::read_to_string(recipe_path)
        .with_context(|| format!("Failed to read recipe: {}", recipe_path.display()))?;

    // Canonicalize recipe path for locking
    let recipe_path_canonical = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    // Acquire exclusive lock to prevent concurrent resolution
    let _lock = acquire_recipe_lock(&recipe_path_canonical)?;

    // Set up minimal execution context for helpers (git_clone needs BUILD_DIR)
    init_context(build_dir.to_path_buf(), build_dir.to_path_buf());
    let _context_guard = ContextGuard::new();

    // Create scope with variables
    let mut scope = Scope::new();
    scope.push_constant("BUILD_DIR", build_dir.to_string_lossy().to_string());
    scope.push_constant("ARCH", std::env::consts::ARCH);

    // Compile script
    let ast = engine
        .compile(&script)
        .map_err(|e| anyhow::anyhow!("Failed to compile recipe: {}", e))?;

    // Check that resolve() function exists
    if !has_action(&ast, "resolve") {
        return Err(anyhow::anyhow!(
            "Recipe '{}' does not define a resolve() function",
            recipe_path.display()
        ));
    }

    // Get recipe name for logging
    let name = recipe_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    output::action(&format!("Resolving {}", name));

    // Call resolve() function
    let result = engine
        .call_fn::<rhai::Dynamic>(&mut scope, &ast, "resolve", ())
        .map_err(|e| anyhow::anyhow!("resolve() failed: {}", e))?;

    // Validate return type - must be a String
    let result_type = result.type_name();
    let result = result.try_cast::<String>().ok_or_else(|| {
        anyhow::anyhow!(
            "resolve() must return a String path, got: {}",
            result_type
        )
    })?;

    // Validate and normalize the returned path
    let path = std::path::PathBuf::from(&result);

    // Handle relative paths by joining with build_dir
    let path = if path.is_relative() {
        build_dir.join(&path)
    } else {
        path
    };

    // Verify the path exists before canonicalizing
    if !path.exists() {
        return Err(anyhow::anyhow!(
            "resolve() returned path that doesn't exist: {}",
            path.display()
        ));
    }

    // Canonicalize the path to prevent path traversal attacks
    let path = path.canonicalize().with_context(|| {
        format!(
            "Failed to canonicalize resolved path: {}",
            path.display()
        )
    })?;

    output::success(&format!("{} resolved to {}", name, path.display()));
    Ok(path)
}

/// Upgrade a package (reinstall if new version available)
pub fn upgrade(
    engine: &Engine,
    prefix: &Path,
    build_dir: &Path,
    recipe_path: &Path,
) -> Result<bool> {
    let recipe_path_canonical = recipe_path
        .canonicalize()
        .unwrap_or_else(|_| recipe_path.to_path_buf());

    // Check if installed
    let installed: Option<bool> =
        recipe_state::get_var(&recipe_path_canonical, "installed").unwrap_or(None);

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

    let installed_version: Option<OptionalString> =
        recipe_state::get_var(&recipe_path_canonical, "installed_version").unwrap_or(None);
    let installed_version: Option<String> = installed_version.and_then(|v| v.into());

    // Compare versions - use is_upgrade_needed for clear semantics
    if !is_upgrade_needed(installed_version.as_deref(), recipe_version.as_deref()) {
        output::skip(&format!(
            "{} is up to date ({})",
            name,
            recipe_version.as_deref().unwrap_or("?")
        ));
        return Ok(false);
    }

    output::action(&format!(
        "Upgrading {} ({} -> {})",
        name,
        installed_version.as_deref().unwrap_or("?"),
        recipe_version.as_deref().unwrap_or("?")
    ));

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

    fn create_test_env() -> (
        TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
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
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(
            &recipes_dir,
            "test",
            r#"
let name = "test";
let installed = false;
fn acquire() {}
fn install() {}
"#,
        );
        let engine = RecipeEngine::new(prefix.clone(), build_dir);
        let result = remove(&engine.engine, &prefix, &recipe_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not installed"));
    }

    #[cheat_reviewed("Remove test - empty installed_files list handled")]
    #[test]
    fn test_remove_with_no_files() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(
            &recipes_dir,
            "test",
            r#"
let name = "test";
let installed = true;
let installed_files = [];
fn acquire() {}
fn install() {}
"#,
        );
        let engine = RecipeEngine::new(prefix.clone(), build_dir);
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
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

        // Create a file to be "installed"
        let bin_dir = prefix.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let test_file = bin_dir.join("test-binary");
        std::fs::write(&test_file, "binary content").unwrap();

        let recipe_path = write_recipe(
            &recipes_dir,
            "test",
            &format!(
                r#"
let name = "test";
let installed = true;
let installed_files = ["{}"];
fn acquire() {{}}
fn install() {{}}
"#,
                test_file.display()
            ),
        );

        let engine = RecipeEngine::new(prefix.clone(), build_dir);
        let result = remove(&engine.engine, &prefix, &recipe_path);
        assert!(result.is_ok());

        // File should be deleted
        assert!(!test_file.exists());
    }

    #[cheat_reviewed("Remove test - partial failure preserves installed state")]
    #[test]
    fn test_remove_partial_failure_preserves_state() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

        // Create a directory instead of a file (can't remove with remove_file)
        let bin_dir = prefix.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let non_removable = bin_dir.join("subdir");
        std::fs::create_dir(&non_removable).unwrap();
        // Put a file inside so the directory isn't empty
        std::fs::write(non_removable.join("file"), "content").unwrap();

        let recipe_path = write_recipe(
            &recipes_dir,
            "test",
            &format!(
                r#"
let name = "test";
let installed = true;
let installed_files = ["{}"];
fn acquire() {{}}
fn install() {{}}
"#,
                non_removable.display()
            ),
        );

        let engine = RecipeEngine::new(prefix.clone(), build_dir);
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
        let recipe_path = write_recipe(
            &recipes_dir,
            "test",
            r#"
let name = "test";
let version = "1.0";
fn acquire() {}
fn install() {}
"#,
        );
        let engine = RecipeEngine::new(prefix, build_dir);
        let result = update(&engine.engine, &recipe_path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None);
    }

    #[cheat_reviewed("Update test - check_update returning unit means no update")]
    #[test]
    fn test_update_returns_unit_no_update() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(
            &recipes_dir,
            "test",
            r#"
let name = "test";
let version = "1.0";
fn acquire() {}
fn install() {}
fn check_update() { () }
"#,
        );
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
        let recipe_path = write_recipe(
            &recipes_dir,
            "test",
            r#"
let name = "test";
let version = "1.0";
fn acquire() {}
fn install() {}
fn check_update() { "2.0" }
"#,
        );
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
        let recipe_path = write_recipe(
            &recipes_dir,
            "test",
            r#"
let name = "test";
let version = "1.0";
fn acquire() {}
fn install() {}
fn check_update() { undefined_var }
"#,
        );
        let engine = RecipeEngine::new(prefix, build_dir);
        let result = update(&engine.engine, &recipe_path);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("update check failed")
        );
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
        let recipe_path = write_recipe(
            &recipes_dir,
            "test",
            r#"
let name = "test";
let version = "1.0";
let installed = false;
fn acquire() {}
fn install() {}
"#,
        );
        let engine = RecipeEngine::new(prefix, build_dir);
        let result = upgrade(
            &engine.engine,
            &engine.prefix,
            &engine.build_dir,
            &recipe_path,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not installed"));
    }

    #[cheat_reviewed("Upgrade test - up-to-date package returns false")]
    #[test]
    fn test_upgrade_already_up_to_date() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(
            &recipes_dir,
            "test",
            r#"
let name = "test";
let version = "1.0";
let installed = true;
let installed_version = "1.0";
let installed_files = [];
fn acquire() {}
fn install() {}
"#,
        );
        let engine = RecipeEngine::new(prefix, build_dir);
        let result = upgrade(
            &engine.engine,
            &engine.prefix,
            &engine.build_dir,
            &recipe_path,
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), false); // No upgrade performed
    }

    // ==================== resolve() tests ====================

    #[cheat_aware(
        protects = "User sees helpful error when resolve() returns wrong type",
        severity = "MEDIUM",
        ease = "EASY",
        cheats = [
            "Accept any return type and coerce to string",
            "Show generic 'resolve failed' error without type info"
        ],
        consequence = "User sees cryptic error message when debugging recipes"
    )]
    #[test]
    fn test_resolve_wrong_return_type() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(
            &recipes_dir,
            "wrong-type-resolve",
            r#"
fn resolve() {
    return 42;  // Returns integer instead of string
}
"#,
        );
        let engine = RecipeEngine::new(prefix, build_dir);
        let result = engine.resolve(&recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must return a String"));
        assert!(err.contains("i64")); // Rhai's integer type
    }

    #[cheat_aware(
        protects = "User sees clear error when resolve() function is missing",
        severity = "HIGH",
        ease = "EASY",
        cheats = [
            "Return empty path silently",
            "Skip resolve entirely if missing"
        ],
        consequence = "User's recipe silently fails to resolve sources"
    )]
    #[test]
    fn test_resolve_missing_function() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(
            &recipes_dir,
            "no-resolve",
            r#"
let name = "test";
let version = "1.0";
// No resolve() function defined
"#,
        );
        let engine = RecipeEngine::new(prefix, build_dir);
        let result = engine.resolve(&recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not define a resolve() function"));
    }

    #[cheat_aware(
        protects = "User sees error when resolve() returns non-existent path",
        severity = "HIGH",
        ease = "EASY",
        cheats = [
            "Create the directory automatically",
            "Return success with non-existent path"
        ],
        consequence = "Build fails later with confusing 'file not found' error"
    )]
    #[test]
    fn test_resolve_path_not_exist() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();
        let recipe_path = write_recipe(
            &recipes_dir,
            "nonexistent-path",
            r#"
fn resolve() {
    return "/nonexistent/path/that/does/not/exist";
}
"#,
        );
        let engine = RecipeEngine::new(prefix, build_dir);
        let result = engine.resolve(&recipe_path);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("doesn't exist"));
    }

    #[cheat_aware(
        protects = "Relative paths from resolve() are correctly joined with build_dir",
        severity = "HIGH",
        ease = "MEDIUM",
        cheats = [
            "Use relative path as-is without joining",
            "Join with wrong base directory"
        ],
        consequence = "User's build finds wrong source directory or fails"
    )]
    #[test]
    fn test_resolve_relative_path_joined() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

        // Create the source directory inside build_dir
        let source_dir = build_dir.join("my-source");
        std::fs::create_dir_all(&source_dir).unwrap();

        let recipe_path = write_recipe(
            &recipes_dir,
            "relative-resolve",
            r#"
fn resolve() {
    return "my-source";  // Relative path
}
"#,
        );
        let engine = RecipeEngine::new(prefix, build_dir);
        let result = engine.resolve(&recipe_path);
        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert_eq!(resolved, source_dir.canonicalize().unwrap());
    }

    #[cheat_aware(
        protects = "Absolute paths from resolve() are accepted as-is",
        severity = "HIGH",
        ease = "MEDIUM",
        cheats = [
            "Force absolute paths to be relative to build_dir",
            "Reject absolute paths entirely"
        ],
        consequence = "User can't reference sources outside build_dir (breaks ../linux pattern)"
    )]
    #[test]
    fn test_resolve_absolute_path_accepted() {
        let (_dir, prefix, build_dir, recipes_dir) = create_test_env();

        // Create a source directory (will use its absolute path)
        let source_dir = recipes_dir.join("external-source");
        std::fs::create_dir_all(&source_dir).unwrap();
        let absolute_path = source_dir.canonicalize().unwrap();

        let recipe_content = format!(
            r#"
fn resolve() {{
    return "{}";
}}
"#,
            absolute_path.display()
        );
        let recipe_path = write_recipe(&recipes_dir, "absolute-resolve", &recipe_content);

        let engine = RecipeEngine::new(prefix, build_dir);
        let result = engine.resolve(&recipe_path);
        assert!(result.is_ok());
        let resolved = result.unwrap();
        assert_eq!(resolved, absolute_path);
    }
}
