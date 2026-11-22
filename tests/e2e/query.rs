use super::*;

fn query_json(
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
    serde_json::from_slice(&output.stdout).expect("the CLI must print its result as JSON")
}

#[test]
fn test_cli_cleanup_success() {
    let (_dir, recipes) = create_test_env();
    let source = r#"const RECIPE_API = 1;
const NEEDS = #{};
fn cleanup(reason) { log("CLEANUP_REASON " + reason); }
"#;
    write_recipe(&recipes, "cleanable", source);
    let output = run_recipe(&["cleanup", "cleanable"], &recipes);
    let result = query_json(&output, &recipes, "cleanable", source);
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["operation"], "cleanup");
    assert_eq!(result["name"], "cleanable");
    assert_eq!(
        result["steps"],
        serde_json::json!([{"step": "cleanup", "status": "done"}])
    );
    assert_eq!(result["outputs"], serde_json::json!({}));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("CLEANUP_REASON manual"), "{error}");
}

#[test]
fn test_cli_cleanup_define_injected() {
    let (_dir, recipes) = create_test_env();
    let source = r#"const RECIPE_API = 1;
const NEEDS = #{ defines: #{ CLEAN_TAG: "none" } };
fn cleanup(reason) { log("CLEANUP_NOTE " + reason + ":" + DEFINES.CLEAN_TAG); }
"#;
    write_recipe(&recipes, "cleanup-define", source);
    let output = run_recipe(
        &["cleanup", "cleanup-define", "--define", "CLEAN_TAG=nightly"],
        &recipes,
    );
    query_json(&output, &recipes, "cleanup-define", source);
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("CLEANUP_NOTE manual:nightly"), "{error}");
}

#[test]
fn test_cli_state_of_a_program_nothing_was_done_for_is_none() {
    let (_dir, recipes) = create_test_env();
    let source = r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
fn build(src, out) { write_file(out, "built"); }
"#;
    write_recipe(&recipes, "untouched", source);
    let output = run_recipe(&["state", "untouched"], &recipes);
    let state = query_json(&output, &recipes, "untouched", source);
    assert_eq!(state["schema_version"], 1);
    assert_eq!(state["name"], "untouched");
    assert_eq!(state["state"], "none");
    assert_eq!(state["installed_version"], serde_json::Value::Null);
    assert_eq!(state["keep"], false);
    assert_eq!(
        state["steps"],
        serde_json::json!(["build", "install", "cleanup"])
    );
    assert_eq!(
        state["on_disk"],
        serde_json::json!({"downloads": 0, "build": 0})
    );
}

#[test]
fn test_cli_state_after_build_is_built_and_after_install_is_installed() {
    let (_dir, recipes) = create_test_env();
    let source = r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ "package": "directory" } };
const PACKAGE = #{ version: "2.0.0" };
fn build(src, out) {
    mkdir(join_path(out, "share"));
    write_file(join_path(out, "share/marker"), "built");
}
"#;
    write_recipe(&recipes, "stateful", source);
    let built = run_recipe(&["build", "stateful"], &recipes);
    let result = query_json(&built, &recipes, "stateful", source);
    assert_eq!(result["operation"], "build");
    assert_eq!(result["state"], "built");
    let state = query_json(
        &run_recipe(&["state", "stateful"], &recipes),
        &recipes,
        "stateful",
        source,
    );
    assert_eq!(state["state"], "built");
    assert_eq!(state["version"], "2.0.0");
    assert_eq!(state["installed_version"], serde_json::Value::Null);

    let installed = run_recipe(&["install", "stateful"], &recipes);
    let result = query_json(&installed, &recipes, "stateful", source);
    assert_eq!(result["state"], "installed");
    let state = query_json(
        &run_recipe(&["state", "stateful"], &recipes),
        &recipes,
        "stateful",
        source,
    );
    assert_eq!(state["state"], "installed");
    assert_eq!(state["installed_version"], "2.0.0");
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

    for (name, version) in [("pkg1", "1.0.0"), ("pkg2", "2.0.0")] {
        write_recipe(
            &recipes,
            name,
            &format!(
                r#"const RECIPE_API = 1;
const NEEDS = #{{ outputs: #{{ "package": "directory" }} }};
const PACKAGE = #{{ name: "{name}", version: "{version}", description: "Package {name}" }};
"#
            ),
        );
    }

    let output = run_recipe(&["list"], &recipes);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("pkg1"), "pkg1 missing from list: {stdout}");
    assert!(stdout.contains("pkg2"), "pkg2 missing from list: {stdout}");

    let output = run_recipe(&["list", "--json"], &recipes);
    assert_success(&output);
    let list: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(list["schema_version"], 1);
    assert_eq!(list["problems"], serde_json::json!([]));
    let recipes_listed = list["recipes"].as_array().unwrap();
    assert_eq!(recipes_listed.len(), 2);
    for entry in recipes_listed {
        let name = entry["name"].as_str().unwrap();
        assert_eq!(
            entry["version"],
            if name == "pkg1" { "1.0.0" } else { "2.0.0" }
        );
        assert_eq!(entry["description"], format!("Package {name}"));
        assert_eq!(entry["state"], "none");
        assert_eq!(entry["installed_version"], serde_json::Value::Null);
        assert_eq!(entry["keep"], false);
        assert_eq!(entry["steps"], serde_json::json!(["install", "cleanup"]));
        assert_eq!(entry["functions"], serde_json::json!([]));
        assert!(entry.get("hooks").is_none());
    }
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
        r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ "package": "directory" } };
const PACKAGE = #{ name: "mypackage", version: "1.2.3", description: "A test package" };
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
    assert!(stdout.contains("A test package"));
}

/// Metadata queries must use the same declarations as execution while never
/// evaluating top-level code on the host, even when its side effect would be
/// dangerous.
#[test]
fn test_cli_metadata_inherits_without_evaluating_top_level_code() {
    let (dir, recipes) = create_test_env();
    let marker = dir.path().join("metadata-setup-ran");
    write_recipe(
        &recipes,
        "base",
        &format!(
            r#"const RECIPE_API = 1;
const NEEDS = #{{}};
const PACKAGE = #{{ version: "3.2.1", description: "Inherited description" }};
write_file("{}", "must never run");
"#,
            marker.display()
        ),
    );
    write_recipe(
        &recipes,
        "child",
        r#"//! extends: base.rhai
const RECIPE_API = 1;
const NEEDS = #{};
"#,
    );
    let output = run_recipe(&["info", "child"], &recipes);
    assert_success(&output);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains("child") && text.contains("3.2.1") && text.contains("Inherited description"),
        "{text}"
    );
    assert!(!marker.exists(), "metadata query executed top-level code");
}

#[test]
fn test_cli_metadata_refuses_values_that_are_not_literal() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "dynamic",
        r#"const RECIPE_API = 1;
const NEEDS = #{};
const PACKAGE = #{ version: exec_output("definitely-unavailable", []) };
"#,
    );
    let output = run_recipe(&["info", "dynamic"], &recipes);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must be literal"));
}

#[test]
fn test_cli_metadata_rejects_malformed_declarations() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "wrong-type",
        "const RECIPE_API = 1;\nconst NEEDS = #{};\nconst PACKAGE = #{ name: 5 };\n",
    );
    let output = run_recipe(&["info", "wrong-type"], &recipes);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("PACKAGE.name must be a literal string")
    );
    write_recipe(
        &recipes,
        "broken",
        "const RECIPE_API = 1;\nconst PACKAGE = #{name:",
    );
    let output = run_recipe(&["info", "broken"], &recipes);
    assert!(!output.status.success());
}

#[test]
fn test_cli_info_refuses_an_old_recipe_form_with_the_line_to_change() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "old-form",
        "const RECIPE_API = 1;\nconst NEEDS = #{};\nlet ctx = #{name: \"old-form\"};\n",
    );
    let output = run_recipe(&["info", "old-form"], &recipes);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("line 3:") && error.contains("ctx"),
        "{error}"
    );
}
