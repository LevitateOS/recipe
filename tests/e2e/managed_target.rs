//! Actual supervised CLI checks of an explicitly selected installed target.
use super::*;
use std::path::PathBuf;

const SOURCE: &str = r#"
const RECIPE_API = 1;
const NEEDS = #{ inputs: #{ target: "directory" }, outputs: #{ tree: "directory" } };
fn build(src, out) {
    print("BUILD-RAN");
    let program = join_path(out, "usr/bin/target-tool");
    if !is_file(program) { throw "selected target product missing"; }
    write_file(join_path(out, "observed"), read_file(program));
    let denied = false;
    try { write_file(join_path(INPUTS.target, "usr/bin/target-tool"), "changed"); } catch (error) { denied = true; }
    if !denied { throw "the target input accepted a change"; }
}
"#;

fn target_command(recipes: &Path, operation: &str, target: &Path) -> Command {
    let root = recipes.parent().unwrap();
    let mut command = recipe_command(recipes);
    command
        .env(
            "GINUX_BIN",
            std::env::var_os("GINUX_BIN").expect("real Ginux launcher required"),
        )
        .env_remove("RECIPE_WORKER")
        .env_remove("RECIPE_INVOCATION")
        .args([operation, "check", "--recipes-path"])
        .arg(recipes)
        .arg("--state-dir")
        .arg(root.join("retained"))
        .arg("--build-dir")
        .arg(root.join("published"))
        .args(["--input", &format!("target={}", target.display())]);
    command
}

fn invoke(command: &mut Command, root: &Path) -> (std::process::Output, Option<serde_json::Value>) {
    let result = tempfile::NamedTempFile::new_in(root).unwrap();
    let output = command
        .arg("--json-output")
        .arg(result.path())
        .output()
        .unwrap();
    let ctx = if output.status.success() {
        Some(serde_json::from_slice(&std::fs::read(result.path()).unwrap()).unwrap())
    } else {
        None
    };
    (output, ctx)
}

fn selected_target(root: &Path, bytes: &str) -> (PathBuf, PathBuf) {
    let target = root.join("selected target");
    let program = target.join("usr/bin/target-tool");
    std::fs::create_dir_all(program.parent().unwrap()).unwrap();
    std::fs::write(&program, bytes).unwrap();
    (target, program)
}

#[test]
fn managed_target_seeds_the_output_without_changing_inputs() {
    let (dir, recipes) = create_test_env();
    write_recipe(&recipes, "check", SOURCE);
    let (target, original) = selected_target(dir.path(), "selected product bytes");
    let (output, result) = invoke(
        &mut target_command(&recipes, "install", &target),
        dir.path(),
    );
    assert_success(&output);
    let result = result.unwrap();
    let tree = PathBuf::from(result["outputs"]["tree"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(tree.join("observed")).unwrap(),
        "selected product bytes"
    );
    assert_eq!(
        std::fs::read_to_string(tree.join("usr/bin/target-tool")).unwrap(),
        "selected product bytes"
    );
    assert_ne!(tree.join("usr/bin/target-tool"), original);
    assert_eq!(
        std::fs::read_to_string(&original).unwrap(),
        "selected product bytes"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        SOURCE
    );
}

#[test]
fn managed_target_changed_input_content_gets_fresh_identity_and_seed() {
    let (dir, recipes) = create_test_env();
    write_recipe(&recipes, "check", SOURCE);
    let (target, original) = selected_target(dir.path(), "alpha");
    let (output, first) = invoke(
        target_command(&recipes, "install", &target).arg("--keep"),
        dir.path(),
    );
    assert_success(&output);
    let first = first.unwrap();
    let first_tree = PathBuf::from(first["outputs"]["tree"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(first_tree.join("observed")).unwrap(),
        "alpha"
    );
    // Recipe, paths, options and state directory stay identical. Only input
    // content changes, so a kept result of the first would show an identity bug.
    std::fs::write(&original, "beta").unwrap();
    let (output, second) = invoke(
        target_command(&recipes, "install", &target).arg("--keep"),
        dir.path(),
    );
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("BUILD-RAN"));
    let second = second.unwrap();
    assert_eq!(
        std::fs::read_to_string(
            Path::new(second["outputs"]["tree"].as_str().unwrap()).join("observed")
        )
        .unwrap(),
        "beta"
    );
    assert_eq!(
        std::fs::read_to_string(first_tree.join("observed")).unwrap(),
        "alpha"
    );
}

#[test]
fn managed_target_missing_product_is_not_satisfied_by_compiler_image() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 1;
const NEEDS = #{ inputs: #{ target: "directory" }, outputs: #{ tree: "directory" } };
fn build(src, out) {
    if !is_file("/usr/bin/bash") { throw "compiler-image fixture is missing bash"; }
    if is_file(join_path(out, "usr/bin/bash")) { throw "compiler image satisfied the selected target"; }
    throw "selected target has no bash";
}
"#,
    );
    let target = dir.path().join("empty target");
    std::fs::create_dir(&target).unwrap();
    let (output, _) = invoke(
        &mut target_command(&recipes, "install", &target),
        dir.path(),
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("selected target has no bash"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(std::fs::read_dir(target).unwrap().next().is_none());
}

#[test]
fn managed_target_rejects_file_input_as_installed_target() {
    let (dir, recipes) = create_test_env();
    write_recipe(&recipes, "check", SOURCE);
    let target = dir.path().join("file instead of target tree");
    std::fs::write(&target, "preserve this input").unwrap();
    let (output, _) = invoke(
        &mut target_command(&recipes, "install", &target),
        dir.path(),
    );
    assert!(!output.status.success());
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostic.contains("target")
            && (diagnostic.contains("directory") || diagnostic.contains("tree")),
        "{diagnostic}"
    );
    assert_eq!(
        std::fs::read_to_string(target).unwrap(),
        "preserve this input"
    );
}
