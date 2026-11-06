use super::*;

fn query_context(
    output: &std::process::Output,
    recipes: &Path,
    name: &str,
    source: &str,
) -> serde_json::Value {
    assert_success(output);
    assert_eq!(
        std::fs::read_to_string(recipes.join(format!("{name}.rhai"))).unwrap(),
        source,
        "query/cleanup must not rewrite the recipe source"
    );
    serde_json::from_slice(&output.stdout).expect("CLI must return the resulting context as JSON")
}

#[test]
fn test_cli_cleanup_success() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "cleanable", state: #{cache_path: "cache", cleanup_reason: ""}};
fn cleanup(ctx, reason) {
    ctx.state.cache_path = "";
    ctx.state.cleanup_reason = reason;
    ctx
}
"#;
    write_recipe(&recipes, "cleanable", source);
    let output = run_recipe(&["cleanup", "cleanable"], &recipes);
    let ctx = query_context(&output, &recipes, "cleanable", source);
    assert_eq!(ctx["state"]["cache_path"], "");
    assert_eq!(ctx["state"]["cleanup_reason"], "manual");
}

#[test]
fn test_cli_cleanup_define_injected() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "cleanup-define", state: #{cleanup_note: ""}};
fn cleanup(ctx, reason) {
    ctx.state.cleanup_note = `${reason}:${CLEAN_TAG}`;
    ctx
}
"#;
    write_recipe(&recipes, "cleanup-define", source);
    let output = run_recipe(
        &[
            "cleanup",
            "cleanup-define",
            "--reason",
            "manual.debug",
            "--define",
            "CLEAN_TAG=nightly",
        ],
        &recipes,
    );
    let ctx = query_context(&output, &recipes, "cleanup-define", source);
    assert_eq!(ctx["state"]["cleanup_note"], "manual.debug:nightly");
}

#[test]
fn test_cli_cleanup_custom_reason() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "cleanable-custom", state: #{cleanup_reason: ""}};
fn cleanup(ctx, reason) { ctx.state.cleanup_reason = reason; ctx }
"#;
    write_recipe(&recipes, "cleanable-custom", source);
    let output = run_recipe(
        &["cleanup", "cleanable-custom", "--reason", "manual.debug"],
        &recipes,
    );
    let ctx = query_context(&output, &recipes, "cleanable-custom", source);
    assert_eq!(ctx["state"]["cleanup_reason"], "manual.debug");
}

#[test]
fn test_cli_cleanup_empty_reason_defaults_to_manual() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "cleanable-empty-reason", state: #{cleanup_reason: ""}};
fn cleanup(ctx, reason) { ctx.state.cleanup_reason = reason; ctx }
"#;
    write_recipe(&recipes, "cleanable-empty-reason", source);
    let output = run_recipe(
        &["cleanup", "cleanable-empty-reason", "--reason", "   "],
        &recipes,
    );
    let ctx = query_context(&output, &recipes, "cleanable-empty-reason", source);
    assert_eq!(ctx["state"]["cleanup_reason"], "manual");
}

#[test]
fn test_cli_isinstalled_success() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "installed-ok", state: #{installed: true}};
fn is_installed(ctx) {
    if ctx.state.installed { return ready(ctx); }
    needs_work(ctx, "not installed")
}
"#;
    write_recipe(&recipes, "installed-ok", source);
    let output = run_recipe(&["isinstalled", "installed-ok"], &recipes);
    let ctx = query_context(&output, &recipes, "installed-ok", source);
    assert_eq!(ctx["state"]["installed"], true);
}

#[test]
fn test_cli_isinstalled_failure_when_not_installed() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "installed-fail", state: #{installed: false}};
fn is_installed(ctx) {
    if ctx.state.installed { return ready(ctx); }
    needs_work(ctx, "not installed")
}
"#;
    write_recipe(&recipes, "installed-fail", source);
    let output = run_recipe(&["isinstalled", "installed-fail"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is_installed is not ready: not installed"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("installed-fail.rhai")).unwrap(),
        source
    );
}

#[test]
fn test_cli_isbuilt_success() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "built-ok", state: #{built: true}};
fn is_built(ctx) { ready(ctx) }
"#;
    write_recipe(&recipes, "built-ok", source);
    let output = run_recipe(&["isbuilt", "built-ok"], &recipes);
    let ctx = query_context(&output, &recipes, "built-ok", source);
    assert_eq!(ctx["state"]["built"], true);
}

#[test]
fn test_cli_isbuilt_missing_function() {
    let (_dir, recipes) = create_test_env();
    let source = "const RECIPE_API = 2; let ctx = #{name: \"built-missing\"};";
    write_recipe(&recipes, "built-missing", source);
    let output = run_recipe(&["isbuilt", "built-missing"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("built-missing has no is_built function"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("built-missing.rhai")).unwrap(),
        source
    );
}

#[test]
fn test_cli_isacquired_success() {
    let (_dir, recipes) = create_test_env();
    let source = r#"
const RECIPE_API = 2;
let ctx = #{name: "acquired-ok", state: #{acquired: true}};
fn is_acquired(ctx) { ready(ctx) }
"#;
    write_recipe(&recipes, "acquired-ok", source);
    let output = run_recipe(&["isacquired", "acquired-ok"], &recipes);
    let ctx = query_context(&output, &recipes, "acquired-ok", source);
    assert_eq!(ctx["state"]["acquired"], true);
}

#[test]
fn test_cli_isacquired_missing_function() {
    let (_dir, recipes) = create_test_env();
    let source = "const RECIPE_API = 2; let ctx = #{name: \"acquired-missing\"};";
    write_recipe(&recipes, "acquired-missing", source);
    let output = run_recipe(&["isacquired", "acquired-missing"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("acquired-missing has no is_acquired function"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("acquired-missing.rhai")).unwrap(),
        source
    );
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
    assert!(stdout.contains("pkg1"), "pkg1 missing from list: {stdout}");
    assert!(stdout.contains("pkg2"), "pkg2 missing from list: {stdout}");
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

/// Metadata queries must use the same declarations as execution while never
/// evaluating setup on the host, even when its side effect would be dangerous.
#[test]
fn test_cli_metadata_inherits_without_evaluating_setup() {
    let (dir, recipes) = create_test_env();
    let marker = dir.path().join("metadata-setup-ran");
    write_recipe(
        &recipes,
        "base",
        &format!(
            r#"
const RECIPE_API = 2;
let ctx = #{{name: "base", version: "3.2.1", description: "Inherited description"}};
write_file("{}", "must never run");
"#,
            marker.display()
        ),
    );
    write_recipe(
        &recipes,
        "child",
        r#"//! extends: base.rhai
ctx.name = "child";
"#,
    );
    let output = run_recipe(&["info", "child"], &recipes);
    assert_success(&output);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("child") && text.contains("3.2.1") && text.contains("Inherited description"),
        "{text}"
    );
    assert!(!marker.exists(), "metadata query executed recipe setup");
}

#[test]
fn test_cli_metadata_reports_dynamic_values_without_execution() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "dynamic",
        r#"
const RECIPE_API = 2;
let ctx = #{name: "dynamic", version: exec_output("definitely-unavailable", [])};
"#,
    );
    let output = run_recipe(&["info", "dynamic"], &recipes);
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("<dynamic>"));
}

#[test]
fn test_cli_metadata_rejects_malformed_declarations() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "wrong-type",
        "const RECIPE_API = 2; let ctx = #{name: 5};",
    );
    let output = run_recipe(&["info", "wrong-type"], &recipes);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("metadata name must be a string"));
    write_recipe(&recipes, "broken", "let ctx = #{name:");
    let output = run_recipe(&["info", "broken"], &recipes);
    assert!(!output.status.success());
}
