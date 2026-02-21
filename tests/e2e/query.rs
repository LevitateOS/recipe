use super::*;
// =============================================================================

#[test]
fn test_cli_cleanup_success() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
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

    let output = run_recipe(&["cleanup", "cleanable"], &recipes);

    assert!(
        output.status.success(),
        "Cleanup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(recipes.join("cleanable.rhai")).unwrap();
    assert!(content.contains("cache_path: \"\""));
    assert!(content.contains("cleanup_reason: \"manual\""));
}

#[test]
fn test_cli_cleanup_define_injected() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "cleanup-define",
        r#"
let ctx = #{
    name: "cleanup-define",
    cleanup_note: "",
};

fn cleanup(ctx, reason) {
    ctx.cleanup_note = `${reason}:${CLEAN_TAG}`;
    ctx
}
"#,
    );

    let output = Command::new(recipe_bin())
        .args([
            "cleanup",
            "cleanup-define",
            "--reason",
            "manual.debug",
            "--define",
            "CLEAN_TAG=nightly",
        ])
        .args(["--recipes-path", recipes.to_str().unwrap()])
        .output()
        .expect("Failed to execute recipe command");

    assert!(
        output.status.success(),
        "Cleanup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(recipes.join("cleanup-define.rhai")).unwrap();
    assert!(content.contains("cleanup_note: \"manual.debug:nightly\""));
}

#[test]
fn test_cli_cleanup_custom_reason() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "cleanable-custom",
        r#"
let ctx = #{
    name: "cleanable-custom",
    cleanup_reason: "",
};

fn cleanup(ctx, reason) {
    ctx.cleanup_reason = reason;
    ctx
}
"#,
    );

    let output = run_recipe(
        &["cleanup", "cleanable-custom", "--reason", "manual.debug"],
        &recipes,
    );

    assert!(
        output.status.success(),
        "Cleanup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(recipes.join("cleanable-custom.rhai")).unwrap();
    assert!(content.contains("cleanup_reason: \"manual.debug\""));
}

#[test]
fn test_cli_cleanup_empty_reason_defaults_to_manual() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "cleanable-empty-reason",
        r#"
let ctx = #{
    name: "cleanable-empty-reason",
    cleanup_reason: "",
};

fn cleanup(ctx, reason) {
    ctx.cleanup_reason = reason;
    ctx
}
"#,
    );

    let output = run_recipe(
        &["cleanup", "cleanable-empty-reason", "--reason", "   "],
        &recipes,
    );
    assert!(
        output.status.success(),
        "Cleanup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(recipes.join("cleanable-empty-reason.rhai")).unwrap();
    assert!(content.contains("cleanup_reason: \"manual\""));
}

#[test]
fn test_cli_isinstalled_success() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "installed-ok",
        r#"
let ctx = #{
    name: "installed-ok",
    installed: true,
};

fn is_installed(ctx) { ctx }
"#,
    );

    let output = run_recipe(&["isinstalled", "installed-ok"], &recipes);
    assert!(
        output.status.success(),
        "isinstalled failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_cli_isinstalled_failure_when_not_installed() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "installed-fail",
        r#"
let ctx = #{
    name: "installed-fail",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}
"#,
    );

    let output = run_recipe(&["isinstalled", "installed-fail"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("is_installed failed") || stderr.contains("not installed"));
}

#[test]
fn test_cli_isbuilt_success() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "built-ok",
        r#"
let ctx = #{
    name: "built-ok",
};

fn is_built(ctx) { ctx }
"#,
    );

    let output = run_recipe(&["isbuilt", "built-ok"], &recipes);
    assert!(
        output.status.success(),
        "isbuilt failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_cli_isbuilt_missing_function() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "built-missing",
        r#"
let ctx = #{
    name: "built-missing",
};
"#,
    );

    let output = run_recipe(&["isbuilt", "built-missing"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no is_built function"));
}

#[test]
fn test_cli_isacquired_success() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "acquired-ok",
        r#"
let ctx = #{
    name: "acquired-ok",
};

fn is_acquired(ctx) { ctx }
"#,
    );

    let output = run_recipe(&["isacquired", "acquired-ok"], &recipes);
    assert!(
        output.status.success(),
        "isacquired failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_cli_isacquired_missing_function() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "acquired-missing",
        r#"
let ctx = #{
    name: "acquired-missing",
};
"#,
    );

    let output = run_recipe(&["isacquired", "acquired-missing"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no is_acquired function"));
}

// =============================================================================
// List Command Tests
// =============================================================================

#[test]
fn test_cli_list_empty() {
    let (_dir, recipes) = create_test_env();

    let output = run_recipe(&["list"], &recipes);
    assert!(output.status.success());
}

#[test]
fn test_cli_list_recipes() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "pkg1",
        r#"
let ctx = #{
    name: "pkg1",
    version: "1.0.0",
};
"#,
    );

    write_recipe(
        &recipes,
        "pkg2",
        r#"
let ctx = #{
    name: "pkg2",
    version: "2.0.0",
};
"#,
    );

    let output = run_recipe(&["list"], &recipes);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("pkg1") || stdout.contains("pkg2"));
}

// =============================================================================
// Info Command Tests
// =============================================================================

#[test]
fn test_cli_info_shows_details() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "mypackage",
        r#"
let ctx = #{
    name: "mypackage",
    version: "1.2.3",
    description: "A test package",
};
"#,
    );

    let output = run_recipe(&["info", "mypackage"], &recipes);

    assert!(
        output.status.success(),
        "Info failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mypackage"));
    assert!(stdout.contains("1.2.3"));
}
