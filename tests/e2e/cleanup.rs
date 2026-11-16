//! Cleanup is optional, step-aware and cannot hide failures.
use super::*;
#[test]
fn cli_optional_cleanup_preserves_installed_program() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "tool",
        r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ "package": "directory" } };
const PACKAGE = #{ version: "1.0.0" };
fn build(src, out) {
    mkdir(join_path(out, "share"));
    write_file(join_path(out, "share/tool"), "installed");
}
"#,
    );
    let output = run_recipe(&["install", "tool"], &recipes);
    assert_success(&output);
    let installed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let tool = std::path::PathBuf::from(installed["outputs"]["package"].as_str().unwrap())
        .join("share/tool");
    for flags in [vec![], vec!["--downloads"], vec!["--build"]] {
        let mut args = vec!["cleanup", "tool"];
        args.extend(flags);
        let output = run_recipe(&args, &recipes);
        assert_success(&output);
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["operation"], "cleanup");
        assert_eq!(result["state"], "installed");
        assert_eq!(std::fs::read_to_string(&tool).unwrap(), "installed");
    }
    let output = run_recipe(&["state", "tool"], &recipes);
    assert_success(&output);
    let state: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(state["state"], "installed");
}
#[test]
fn cli_cleanup_errors_fail_success_and_keep_primary_failure() {
    let (_dir, recipes) = create_test_env();
    for (name, body, expected) in [
        (
            "cleanup-after-success",
            "write_file(out, \"installed\");",
            "CLEANUP_FAILED",
        ),
        (
            "cleanup-after-failure",
            "throw \"PRIMARY_BUILD_FAILED\";",
            "PRIMARY_BUILD_FAILED",
        ),
    ] {
        write_recipe(
            &recipes,
            name,
            &format!(
                r#"const RECIPE_API = 1;
const NEEDS = #{{ outputs: #{{ product: "file" }} }};
fn build(src, out) {{ {body} }}
fn cleanup(reason) {{ throw "CLEANUP_FAILED"; }}
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
        r#"const RECIPE_API = 1;
const NEEDS = #{};
print("UNEXPECTED_MALFORMED_SETUP");
fn build(src, out) { log("UNEXPECTED_BUILD"); }
fn cleanup() { }
"#,
    );
    let output = run_recipe(&["install", "malformed"], &recipes);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("cleanup(reason)"), "{error}");
    assert!(!error.contains("UNEXPECTED_MALFORMED_SETUP"));
    assert!(!error.contains("UNEXPECTED_BUILD"));
    write_recipe(
        &recipes,
        "tool",
        r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
fn build(src, out) { write_file(out, "tool"); }
fn cleanup(reason) { throw "TOOL_CLEANUP_FAILED"; }
"#,
    );
    write_recipe(
        &recipes,
        "parent",
        r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
let deps = ["tool"];
fn build(src, out) { log("UNEXPECTED_PARENT_BUILD"); write_file(out, "parent"); }
"#,
    );
    let output = run_recipe(&["install", "parent"], &recipes);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("TOOL_CLEANUP_FAILED"), "{error}");
    assert!(!error.contains("UNEXPECTED_PARENT_BUILD"));
}
