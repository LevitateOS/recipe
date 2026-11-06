//! Cleanup is optional, phase-aware and cannot hide failures.
use super::*;
#[test]
fn cli_optional_cleanup_preserves_installed_product() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "tool",
        r#"const RECIPE_API = 2;
let ctx=#{name:"optional-cleanup"};
fn is_installed(ctx) { if !is_file("/out/tool") { return needs_work(ctx,"install"); } ctx.outputs.tool="/out/tool"; ready(ctx) }
fn is_built(ctx) { ready(ctx) }
fn install(ctx) { write_file("/out/tool","installed"); ctx }
"#,
    );
    for operation in ["install", "cleanup", "isinstalled"] {
        let output = run_recipe(&[operation, "tool"], &recipes);
        assert_success(&output);
        if operation == "isinstalled" {
            let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                std::fs::read_to_string(ctx["outputs"]["tool"].as_str().unwrap()).unwrap(),
                "installed"
            );
        }
    }
}
#[test]
fn cli_cleanup_errors_fail_success_and_keep_primary_failure() {
    let (_dir, recipes) = create_test_env();
    for (name, body, expected) in [
        (
            "cleanup-after-success",
            "write_file(\"/out/product\",\"installed\"); ctx.state.installed=true; ctx",
            "CLEANUP_FAILED",
        ),
        (
            "cleanup-after-failure",
            "throw \"PRIMARY_INSTALL_FAILED\"; ctx",
            "PRIMARY_INSTALL_FAILED",
        ),
    ] {
        write_recipe(
            &recipes,
            name,
            &format!(
                r#"const RECIPE_API = 2;
let ctx=#{{name:{name:?},state:#{{installed:false}}}};
fn is_installed(ctx) {{ if ctx.state.installed {{ return ready(ctx); }} needs_work(ctx,"install") }}
fn is_built(ctx) {{ ready(ctx) }}
fn install(ctx) {{ {body} }}
fn cleanup(ctx,reason) {{ throw "CLEANUP_FAILED"; ctx }}
"#
            ),
        );
        let output = run_recipe(&["install", name], &recipes);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(expected), "{error}");
        assert!(error.contains("CLEANUP_FAILED"), "{error}");
    }
}
#[test]
fn cli_malformed_cleanup_precedes_setup_and_dependency_cleanup_stops_parent() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "malformed",
        r#"const RECIPE_API = 2;
print("UNEXPECTED_MALFORMED_SETUP"); let ctx=#{name:"malformed"};
fn is_installed(ctx) { needs_work(ctx,"install") }
fn install(ctx) { log("UNEXPECTED_INSTALL"); ctx }
fn cleanup(ctx) { ctx }
"#,
    );
    let output = run_recipe(&["install", "malformed"], &recipes);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("cleanup"), "{error}");
    assert!(!error.contains("UNEXPECTED_MALFORMED_SETUP"));
    write_recipe(
        &recipes,
        "tool",
        r#"const RECIPE_API = 2;
let ctx=#{name:"tool",state:#{installed:false}};
fn is_installed(ctx) { if ctx.state.installed { return ready(ctx); } needs_work(ctx,"tool") }
fn is_built(ctx) { ready(ctx) }
fn install(ctx) { ctx.state.installed=true; ctx }
fn cleanup(ctx,reason) { throw "TOOL_CLEANUP_FAILED"; ctx }
"#,
    );
    write_recipe(
        &recipes,
        "parent",
        r#"const RECIPE_API = 2;
let deps=["tool"]; let ctx=#{name:"parent"};
fn is_installed(ctx) { needs_work(ctx,"parent") }
fn is_built(ctx) { ready(ctx) }
fn install(ctx) { log("UNEXPECTED_PARENT_INSTALL"); ctx }
"#,
    );
    let output = run_recipe(&["install", "parent"], &recipes);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("TOOL_CLEANUP_FAILED"), "{error}");
    assert!(!error.contains("UNEXPECTED_PARENT_INSTALL"));
}
