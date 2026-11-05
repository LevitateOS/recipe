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
    installed: false,
    source: join_path(RECIPE_DIR, "source.txt"),
    built: join_path(BUILD_DIR, "built.txt"),
    installed_file: join_path(RECIPE_DIR, "installed.txt"),
    trace: join_path(RECIPE_DIR, "lifecycle.log"),
};
fn is_acquired(ctx) { if !is_file(ctx.source) { throw "source missing"; } ctx }
fn is_built(ctx) { if !is_file(ctx.built) { throw "build missing"; } ctx }
fn is_installed(ctx) { if !is_file(ctx.installed_file) { throw "install missing"; } ctx }
fn acquire(ctx) {
    write_file(ctx.source, "payload-v1");
    append_file(ctx.trace, "acquire\n");
    ctx
}
fn build(ctx) {
    copy_file(ctx.source, ctx.built);
    append_file(ctx.trace, "build\n");
    ctx
}
fn install(ctx) {
    copy_file(ctx.built, ctx.installed_file);
    append_file(ctx.trace, "install\n");
    ctx.installed = true;
    ctx
}
fn remove(ctx) {
    rm(ctx.installed_file);
    append_file(ctx.trace, "remove\n");
    ctx.installed = false;
    ctx
}
fn cleanup(ctx, reason) { ctx }
"#,
    );
    let output = run_recipe(&["install", "simple"], &recipes);
    assert_success(&output);
    let installed: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("install stdout contains only final ctx JSON");
    assert_eq!(installed["installed"], true);
    assert_eq!(
        std::fs::read_to_string(recipes.join("installed.txt")).unwrap(),
        "payload-v1"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("lifecycle.log")).unwrap(),
        "acquire\nbuild\ninstall\n"
    );

    let original = std::fs::read(recipes.join("simple.rhai")).unwrap();
    assert_success(&run_recipe(&["install", "simple"], &recipes));
    assert_eq!(
        std::fs::read(recipes.join("simple.rhai")).unwrap(),
        original,
        "an already-installed recipe should not be rewritten"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("lifecycle.log")).unwrap(),
        "acquire\nbuild\ninstall\n",
        "reinstall reran completed lifecycle phases"
    );

    assert_success(&run_recipe(&["isinstalled", "simple"], &recipes));
    let output = run_recipe(&["remove", "simple"], &recipes);
    assert_success(&output);
    let removed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(removed["installed"], false);
    assert!(
        !recipes.join("installed.txt").exists(),
        "remove left the installed payload behind"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("lifecycle.log")).unwrap(),
        "acquire\nbuild\ninstall\nremove\n"
    );
    let output = run_recipe(&["isinstalled", "simple"], &recipes);
    assert_eq!(
        output.status.code(),
        Some(1),
        "removed package was still reported installed"
    );
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

    let output = recipe_command(&recipes)
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

    let output = recipe_command(&recipes)
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

#[test]
fn test_cli_inherited_recipe_preserves_base_and_installs_child_payload() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "base",
        r#"
let ctx = #{ name: "base", message: "base payload", installed: false };
fn is_installed(ctx) { if !ctx.installed { throw "not installed"; } ctx }
fn acquire(ctx) { ctx }
fn install(ctx) {
    write_file(join_path(RECIPE_DIR, "inherited-payload.txt"), ctx.message);
    ctx.installed = true;
    ctx
}
fn cleanup(ctx, reason) { ctx }
"#,
    );
    std::fs::write(
        recipes.join("child.rhai"),
        r#"//! extends: base.rhai
let ctx = #{ name: "child", message: "child \"quoted\" \\ payload", installed: false };
"#,
    )
    .unwrap();
    let base = std::fs::read(recipes.join("base.rhai")).unwrap();
    let output = run_recipe(&["install", "child"], &recipes);
    assert_success(&output);
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(ctx["name"], "child");
    assert_eq!(ctx["installed"], true);
    assert_eq!(
        std::fs::read_to_string(recipes.join("inherited-payload.txt")).unwrap(),
        "child \"quoted\" \\ payload"
    );
    assert_eq!(
        std::fs::read(recipes.join("base.rhai")).unwrap(),
        base,
        "child state overwrote the shared base recipe"
    );
    // Reparse persisted state in a separate CLI process.
    let output = run_recipe(&["isinstalled", "child"], &recipes);
    assert_success(&output);
    let persisted: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(persisted, ctx);
}
