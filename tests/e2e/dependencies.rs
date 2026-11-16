//! Full CLI dependency graphs. A prepared Ginux environment must provide `cc`.
use super::*;

#[test]
fn cli_transitive_dependencies_compile_tools_through_the_shared_lifecycle() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "dep-helpers",
        r#"fn expected() { "real leaf\n" }"#,
    );
    write_recipe(
        &recipes,
        "leaf",
        r##"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ tools: "directory" } };
fn build(src, out) {
    let source = join_path(WORK, "leaf.c");
    write_file(source, "#include <stdio.h>\nint main(void) { puts(\"real leaf\"); return 0; }\n");
    mkdir(join_path(out, "bin"));
    exec_checked("cc", [source, "-o", join_path(out, "bin/leaf")]);
}
"##,
    );
    write_recipe(
        &recipes,
        "middle-base",
        r##"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ tools: "directory" } };
import "dep-helpers.rhai" as helper;
fn build(src, out) {
    if exec_output("leaf", []) != helper::expected() { throw "transitive leaf unavailable"; }
    let source = join_path(WORK, "middle.c");
    write_file(source, "#include <stdio.h>\nint main(void) { puts(\"middle built with leaf\"); return 0; }\n");
    mkdir(join_path(out, "bin"));
    exec_checked("cc", [source, "-o", join_path(out, "bin/middle")]);
}
fn cleanup(reason) { log("DEP_CLEANUP " + reason); }
"##,
    );
    write_recipe(
        &recipes,
        "middle",
        "//! extends: middle-base.rhai\nconst RECIPE_API = 1;\nconst NEEDS = #{ outputs: #{ tools: \"directory\" } };\nlet build_deps = [\"leaf\"];\n",
    );
    write_recipe(
        &recipes,
        "parent",
        r##"
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ report: "file" } };
let deps = ["middle", "leaf"];
fn build(src, out) {
    let leaf = exec_output("leaf", []);
    let middle = exec_output("middle", []);
    if leaf != "real leaf\n" || middle != "middle built with leaf\n" { throw "wrong dependency tools"; }
    write_file(out, leaf + middle);
}
"##,
    );
    let output = run_recipe(&["install", "parent"], &recipes);
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["state"], "installed");
    assert_eq!(
        std::fs::read_to_string(result["outputs"]["report"].as_str().unwrap()).unwrap(),
        "real leaf\nmiddle built with leaf\n"
    );
    let ran: Vec<&str> = result["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|dependency| dependency["name"].as_str().unwrap())
        .collect();
    assert!(ran.contains(&"middle") && ran.contains(&"leaf"), "{ran:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("DEP_CLEANUP auto.build.success"),
        "{stderr}"
    );
    // Installing again finds everything installed already.
    let output = run_recipe(&["install", "parent"], &recipes);
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["state"], "installed");
}

fn write_graph_node(recipes: &Path, name: &str, declaration: &str) {
    write_recipe(
        recipes,
        name,
        &format!(
            r##"
const RECIPE_API = 1;
const NEEDS = #{{}};
{declaration}
fn build(src, out) {{ log("UNEXPECTED_GRAPH_INSTALL"); }}
"##
        ),
    );
}

#[test]
fn cli_dependency_cycles_missing_recipes_and_bad_declarations_fail_usefully() {
    for (declaration, diagnostic) in [
        (
            "let deps = [\"absent\"];",
            "dependency recipe \"absent\" not found",
        ),
        ("let deps = [\"a\"];", "Circular recipe dependency detected"),
        (
            "let deps = [42];",
            "dependency names must be literal strings",
        ),
        (
            "let build_deps = false;",
            "dependency declarations must be literal arrays",
        ),
        ("let deps = [\"../escape\"];", "escapes with `..`"),
    ] {
        let (_dir, recipes) = create_test_env();
        write_graph_node(&recipes, "parent", declaration);
        write_graph_node(&recipes, "a", "let deps = [\"b\"];");
        write_graph_node(&recipes, "b", "let deps = [\"a\"];");
        let output = run_recipe(&["install", "parent", "--no-persist-state"], &recipes);
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(diagnostic), "{diagnostic}: {stderr}");
        assert!(!stderr.contains("UNEXPECTED_GRAPH_INSTALL"), "{stderr}");
    }
}

#[test]
fn cli_dependency_failure_stops_before_parent_installation() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "broken",
        r##"
const RECIPE_API = 1;
const NEEDS = #{};
fn build(src, out) { missing_dependency_helper(); }
"##,
    );
    write_graph_node(&recipes, "parent", "let deps = [\"broken\"];");
    let output = run_recipe(&["install", "parent", "--no-persist-state"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("missing_dependency_helper"), "{stderr}");
    assert!(!stderr.contains("UNEXPECTED_GRAPH_INSTALL"), "{stderr}");
}

#[test]
fn cli_unversioned_dependency_is_rejected_before_setup_effects() {
    let (_dir, recipes) = create_test_env();
    write_graph_node(&recipes, "parent", "let deps = [\"tool\"];");
    write_recipe(
        &recipes,
        "tool",
        r##"
log("UNEXPECTED_DEPENDENCY_SETUP");
fn build(src, out) {}
"##,
    );
    let output = run_recipe(&["install", "parent"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("a recipe starts with `const RECIPE_API = 1;`"),
        "{stderr}"
    );
    assert!(!stderr.contains("UNEXPECTED_DEPENDENCY_SETUP"), "{stderr}");
}
