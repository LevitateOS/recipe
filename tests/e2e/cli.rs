use super::*;
use leviso_cheat_test::cheat_aware;
// =============================================================================
// CLI Help and Version Tests
// =============================================================================

#[test]
fn test_cli_help() {
    let output = Command::new(recipe_bin())
        .arg("--help")
        .output()
        .expect("Failed to run recipe --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("recipe"));
    assert!(stdout.contains("install"));
    assert!(stdout.contains("remove"));
}

#[test]
fn test_cli_help_includes_machine_events_flag() {
    let output = Command::new(recipe_bin())
        .arg("--help")
        .output()
        .expect("Failed to run recipe --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("--machine-events"));
}

#[test]
fn test_cli_version() {
    let output = Command::new(recipe_bin())
        .arg("--version")
        .output()
        .expect("Failed to run recipe --version");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("recipe"));
}

// =============================================================================
// Install Command Tests
// =============================================================================

#[cheat_aware(
    protects = "Nonexistent package produces clear error, not silent success",
    severity = "HIGH",
    ease = "EASY",
    cheats = ["Return success for missing packages", "Create empty package on the fly"],
    consequence = "User thinks package installed but nothing happened, confusion when binary missing",
    legitimate_change = "Missing recipes must always fail with clear error message"
)]
#[test]
fn test_cli_install_nonexistent_package() {
    let (_dir, recipes) = create_test_env();

    let output = run_recipe(&["install", "nonexistent"], &recipes);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found") || stderr.contains("Recipe not found"));
}

#[cheat_aware(
    protects = "Package installation completes and persists state correctly",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = ["Return success without running install()", "Skip state persistence"],
    consequence = "User thinks package is installed but it isn't, re-runs fail or behave unexpectedly",
    legitimate_change = "If install semantics change, update the lifecycle in src/core/lifecycle.rs"
)]
#[test]
fn test_cli_install_success() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
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

    let output = run_recipe(&["install", "simple"], &recipes);

    assert!(
        output.status.success(),
        "Install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Verify ctx was persisted
    let content = std::fs::read_to_string(recipes.join("simple.rhai")).unwrap();
    assert!(content.contains("installed: true"));
}

#[test]
fn test_cli_install_already_installed() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "already",
        r#"
let ctx = #{
    name: "already",
    version: "1.0.0",
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

    let output = run_recipe(&["install", "already"], &recipes);
    assert!(output.status.success());
}

#[test]
fn test_cli_accepts_explicit_rhai_path() {
    let (_dir, recipes) = create_test_env();

    // Write recipe outside the recipes directory
    let recipe_path = recipes.parent().unwrap().join("external.rhai");
    std::fs::write(
        &recipe_path,
        r#"
let ctx = #{
    name: "external",
    installed: false,
};

fn acquire(ctx) { ctx }
fn install(ctx) {
    ctx.installed = true;
    ctx
}

fn cleanup(ctx, reason) { ctx }
"#,
    )
    .unwrap();

    let output = Command::new(recipe_bin())
        .args(["install", recipe_path.to_str().unwrap()])
        .output()
        .expect("Failed to execute recipe command");

    assert!(
        output.status.success(),
        "Install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// =============================================================================
// Remove Command Tests
// =============================================================================

#[cheat_aware(
    protects = "Package removal actually deletes installed files and updates state",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = ["Return success without calling remove()", "Update state but leave files"],
    consequence = "User thinks package is removed but files remain, disk fills up or conflicts occur",
    legitimate_change = "If remove semantics change, update the lifecycle in src/core/lifecycle.rs"
)]
#[test]
fn test_cli_remove_success() {
    let (_dir, recipes) = create_test_env();

    // Create a file that will be "removed" in the recipe's output dir
    let output_dir = recipes.join("output");
    std::fs::create_dir_all(&output_dir).unwrap();
    let test_file = output_dir.join("test-binary");
    std::fs::write(&test_file, "binary content").unwrap();

    write_recipe(
        &recipes,
        "removable",
        &format!(
            r#"
let ctx = #{{
    name: "removable",
    version: "1.0.0",
    installed: true,
    installed_file: "{}",
}};

fn remove(ctx) {{
    // Remove the file
    rm(ctx.installed_file);
    ctx.installed = false;
    ctx.installed_file = "";
    ctx
}}
"#,
            test_file.display()
        ),
    );

    assert!(test_file.exists());

    let output = run_recipe(&["remove", "removable"], &recipes);

    assert!(
        output.status.success(),
        "Remove failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // File should be deleted
    assert!(!test_file.exists());

    // State should be updated
    let content = std::fs::read_to_string(recipes.join("removable.rhai")).unwrap();
    assert!(content.contains("installed: false"));
}

#[test]
fn test_cli_remove_no_function() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "noremove",
        r#"
let ctx = #{
    name: "noremove",
    installed: true,
};
"#,
    );

    let output = run_recipe(&["remove", "noremove"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no remove function"));
}

#[test]
fn test_cli_remove_define_injected() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "remove-define",
        r#"
let ctx = #{
    name: "remove-define",
    removed_by: "",
    installed: true,
};

fn remove(ctx) {
    ctx.removed_by = REMOVE_REASON;
    ctx.installed = false;
    ctx
}
"#,
    );

    let output = Command::new(recipe_bin())
        .args([
            "remove",
            "remove-define",
            "--define",
            "REMOVE_REASON=policy",
        ])
        .args(["--recipes-path", recipes.to_str().unwrap()])
        .output()
        .expect("Failed to execute recipe command");

    assert!(
        output.status.success(),
        "Remove failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(recipes.join("remove-define.rhai")).unwrap();
    assert!(content.contains("removed_by: \"policy\""));
    assert!(content.contains("installed: false"));
}

// =============================================================================
