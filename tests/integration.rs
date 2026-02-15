//! Integration tests for recipe lifecycle (ctx-based design)
//!
//! These tests verify the ctx-based recipe execution pattern.

use leviso_cheat_test::cheat_aware;
use levitate_recipe::RecipeEngine;
use std::path::Path;
use tempfile::TempDir;

/// Create a test environment with build_dir and recipes directories
fn create_test_env() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let build_dir = dir.path().join("build");
    let recipes_dir = dir.path().join("recipes");
    std::fs::create_dir_all(&build_dir).unwrap();
    std::fs::create_dir_all(&recipes_dir).unwrap();
    (dir, build_dir, recipes_dir)
}

/// Write a recipe file and return its path
fn write_recipe(recipes_dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let path = recipes_dir.join(format!("{}.rhai", name));
    let mut content = content.to_string();
    // Cleanup is required by repo policy; tests default to a no-op stub.
    if !content.contains("fn cleanup(") {
        content.push_str("\nfn cleanup(ctx, reason) { ctx }\n");
    }
    std::fs::write(&path, content).unwrap();
    path
}

// =============================================================================
// Basic Install Lifecycle Tests
// =============================================================================

#[test]
fn test_install_basic_recipe() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "simple",
        r#"
let ctx = #{
    name: "simple",
    version: "1.0.0",
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
    );

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);
    assert!(result.is_ok(), "Execute failed: {:?}", result.err());

    // Verify ctx was persisted with installed: true
    let content = std::fs::read_to_string(&recipe_path).unwrap();
    assert!(content.contains("installed: true"));
}

#[cheat_aware(
    protects = "Already-installed packages skip acquire/install phases",
    severity = "MEDIUM",
    ease = "MEDIUM",
    cheats = ["Always run all phases regardless of is_installed()", "Ignore is_installed check"],
    consequence = "User waits for unnecessary downloads/builds, potential overwrites of customizations",
    legitimate_change = "is_installed() returning without throw means package is already installed. \
        This enables proper caching and idempotent operations."
)]
#[test]
fn test_skip_already_installed() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "already-installed",
        r#"
let ctx = #{
    name: "already-installed",
    installed: true,
};

fn is_installed(ctx) { ctx }  // Doesn't throw = already installed

fn acquire(ctx) {
    throw "acquire should not be called";
}

fn install(ctx) {
    throw "install should not be called";
}
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);
    assert!(result.is_ok());
}

#[test]
fn test_install_with_build_phase() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "with-build",
        r#"
let ctx = #{
    name: "with-build",
    built: false,
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn is_built(ctx) {
    if !ctx.built { throw "not built"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    ctx.built = true;
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);
    assert!(result.is_ok());

    let content = std::fs::read_to_string(&recipe_path).unwrap();
    assert!(content.contains("built: true"));
    assert!(content.contains("installed: true"));
}

#[test]
fn test_skip_build_when_already_built() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "skip-build",
        r#"
let ctx = #{
    name: "skip-build",
    built: true,
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn is_built(ctx) { ctx }  // Doesn't throw = already built

fn acquire(ctx) {
    throw "acquire should not be called";
}

fn build(ctx) {
    throw "build should not be called";
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);
    assert!(result.is_ok());
}

// =============================================================================
// Error Handling Tests
// =============================================================================

#[test]
fn test_acquire_failure() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "acquire-fail",
        r#"
let ctx = #{
    name: "acquire-fail",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    throw "Acquire failed!";
}

fn install(ctx) { ctx }
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("acquire"));
}

#[cheat_aware(
    protects = "Build failure prevents install phase from running",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = ["Continue to install() after build() fails", "Catch and ignore build errors"],
    consequence = "Broken build artifacts get installed, user gets cryptic runtime errors",
    legitimate_change = "Phase ordering is sacred: acquire -> build -> install. \
        If build fails, we must not proceed to install."
)]
#[test]
fn test_build_failure_prevents_install() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "build-fail",
        r#"
let ctx = #{
    name: "build-fail",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    throw "Build failed!";
}

fn install(ctx) {
    throw "Install should not be reached!";
}
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("build"));
}

#[test]
fn test_install_failure() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "install-fail",
        r#"
let ctx = #{
    name: "install-fail",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn install(ctx) {
    throw "Install failed!";
}
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("install"));
}

// =============================================================================
// Context Persistence Tests
// =============================================================================

#[test]
fn test_ctx_persisted_with_paths() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "persist-paths",
        r#"
let ctx = #{
    name: "persist-paths",
    artifact_path: "",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    ctx.artifact_path = "/tmp/artifact.tar.gz";
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    engine.execute(&recipe_path).unwrap();

    let content = std::fs::read_to_string(&recipe_path).unwrap();
    assert!(content.contains("artifact_path: \"/tmp/artifact.tar.gz\""));
    assert!(content.contains("installed: true"));
}

#[test]
fn test_original_content_preserved() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "preserve-content",
        r#"// Header comment
let ctx = #{
    name: "preserve-content",
    installed: false,
};

// This comment should be preserved
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
    );

    let engine = RecipeEngine::new(build_dir);
    engine.execute(&recipe_path).unwrap();

    let content = std::fs::read_to_string(&recipe_path).unwrap();
    assert!(content.contains("// Header comment"));
    assert!(content.contains("// This comment should be preserved"));
    assert!(content.contains("fn is_installed(ctx)"));
}

// =============================================================================
// Remove Tests
// =============================================================================

#[test]
fn test_remove_basic() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "removable",
        r#"
let ctx = #{
    name: "removable",
    installed: true,
};

fn remove(ctx) {
    ctx.installed = false;
    ctx
}
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    let result = engine.remove(&recipe_path);
    assert!(result.is_ok());

    let content = std::fs::read_to_string(&recipe_path).unwrap();
    assert!(content.contains("installed: false"));
}

#[test]
fn test_remove_no_function() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "no-remove",
        r#"
let ctx = #{
    name: "no-remove",
};
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    let result = engine.remove(&recipe_path);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("no remove function")
    );
}

// =============================================================================
// Cleanup Tests
// =============================================================================

#[test]
fn test_cleanup_basic() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe_path = write_recipe(
        &recipes_dir,
        "cleanable",
        r#"
let ctx = #{
    name: "cleanable",
    cache_path: "/tmp/cache",
    cleanup_reason: "",
};

fn cleanup(ctx, reason) {
    ctx.cache_path = "";
    ctx.cleanup_reason = reason;
    ctx
}
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    let result = engine.cleanup(&recipe_path);
    assert!(result.is_ok());

    let content = std::fs::read_to_string(&recipe_path).unwrap();
    assert!(content.contains("cache_path: \"\""));
    assert!(content.contains("cleanup_reason: \"manual\""));
}

// =============================================================================
// Phase Check Logic Tests
// =============================================================================

#[test]
fn test_no_is_installed_means_always_install() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    // Recipe without is_installed - should always run install
    let recipe_path = write_recipe(
        &recipes_dir,
        "no-check",
        r#"
let ctx = #{
    name: "no-check",
    run_count: 0,
};

fn acquire(ctx) {
    ctx.run_count = ctx.run_count + 1;
    ctx
}

fn install(ctx) {
    ctx.run_count = ctx.run_count + 1;
    ctx
}
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    engine.execute(&recipe_path).unwrap();

    let content = std::fs::read_to_string(&recipe_path).unwrap();
    assert!(content.contains("run_count: 2")); // acquire + install both ran
}

#[test]
fn test_multiple_recipes_independent() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe1 = write_recipe(
        &recipes_dir,
        "pkg1",
        r#"
let ctx = #{
    name: "pkg1",
    value: "one",
};

fn acquire(ctx) { ctx }
fn install(ctx) {
    ctx.value = "installed-one";
    ctx
}
"#,
    );

    let recipe2 = write_recipe(
        &recipes_dir,
        "pkg2",
        r#"
let ctx = #{
    name: "pkg2",
    value: "two",
};

fn acquire(ctx) { ctx }
fn install(ctx) {
    ctx.value = "installed-two";
    ctx
}
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    engine.execute(&recipe1).unwrap();
    engine.execute(&recipe2).unwrap();

    let content1 = std::fs::read_to_string(&recipe1).unwrap();
    let content2 = std::fs::read_to_string(&recipe2).unwrap();

    assert!(content1.contains("installed-one"));
    assert!(content2.contains("installed-two"));
}
