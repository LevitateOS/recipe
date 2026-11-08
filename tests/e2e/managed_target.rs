//! Actual supervised CLI checks of an explicitly selected installed target.
use super::*;
use std::path::PathBuf;

const SOURCE: &str = r#"
const RECIPE_API = 2;
const NEEDS = #{ inputs: #{ target: "directory" }, outputs: #{ program: "file" } };
let ctx = #{name: "selected-target", state: #{removed: false, observed: ""},
    program: join_path(RUNTIME.paths.target, "usr/bin/target-tool")};
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) { ready(ctx) }
fn is_installed(ctx) {
    if ctx.runtime.paths.target != "/out" { throw "target path changed"; }
    if !is_file(ctx.program) { return needs_work(ctx, "selected target product missing"); }
    if ctx.state.removed { throw "target was reseeded after retained removal"; }
    ctx.state.observed = read_file(ctx.program);
    ctx.outputs.program = ctx.program;
    ready(ctx)
}
fn install(ctx) { throw "existing selected product should have satisfied custom readiness"; }
fn remove(ctx) {
    if is_file(ctx.program) { rm(ctx.program); }
    ctx.state.removed = true;
    ctx.outputs = #{};
    ctx
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
fn managed_target_imports_readiness_and_retains_removal_without_changing_inputs() {
    let (dir, recipes) = create_test_env();
    write_recipe(&recipes, "check", SOURCE);
    let (target, original) = selected_target(dir.path(), "selected product bytes");
    let (output, ctx) = invoke(
        &mut target_command(&recipes, "install", &target),
        dir.path(),
    );
    assert_success(&output);
    let ctx = ctx.unwrap();
    assert_eq!(ctx["state"]["observed"], "selected product bytes");
    assert_eq!(ctx["runtime"]["paths"]["target"], "/out");
    let prior_publication = PathBuf::from(ctx["outputs"]["program"].as_str().unwrap());
    assert_ne!(prior_publication, original);
    assert_eq!(
        std::fs::read_to_string(&prior_publication).unwrap(),
        "selected product bytes"
    );

    let (output, ctx) = invoke(&mut target_command(&recipes, "remove", &target), dir.path());
    assert_success(&output);
    assert_eq!(ctx.unwrap()["state"]["removed"], true);
    let (output, _) = invoke(
        &mut target_command(&recipes, "isinstalled", &target),
        dir.path(),
    );
    assert!(
        !output.status.success(),
        "retained removal was replaced by a fresh import"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("selected target product missing"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&original).unwrap(),
        "selected product bytes"
    );
    assert_eq!(
        std::fs::read_to_string(&prior_publication).unwrap(),
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
        &mut target_command(&recipes, "isinstalled", &target),
        dir.path(),
    );
    assert_success(&output);
    let first = first.unwrap();
    assert_eq!(first["state"]["observed"], "alpha");
    let first_publication = PathBuf::from(first["outputs"]["program"].as_str().unwrap());
    // Recipe, paths, options and state directory stay identical. Only input
    // content changes, so stale files/state would demonstrate an identity bug.
    std::fs::write(&original, "beta").unwrap();
    let (output, second) = invoke(
        &mut target_command(&recipes, "isinstalled", &target),
        dir.path(),
    );
    assert_success(&output);
    let second = second.unwrap();
    assert_eq!(second["state"]["observed"], "beta");
    assert_eq!(
        std::fs::read_to_string(second["outputs"]["program"].as_str().unwrap()).unwrap(),
        "beta"
    );
    assert_eq!(std::fs::read_to_string(first_publication).unwrap(), "alpha");
}

#[test]
fn managed_target_missing_product_is_not_satisfied_by_compiler_image() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "check",
        r#"
const RECIPE_API = 2;
const NEEDS = #{ inputs: #{ target: "directory" } };
let ctx = #{name: "target-image-boundary"};
fn is_installed(ctx) {
    if !is_file("/usr/bin/bash") { throw "compiler-image fixture is missing bash"; }
    let selected = join_path(ctx.runtime.paths.target, "usr/bin/bash");
    if !is_file(selected) { return needs_work(ctx, "selected target has no bash"); }
    throw "compiler image satisfied selected target readiness";
}
"#,
    );
    let target = dir.path().join("empty target");
    std::fs::create_dir(&target).unwrap();
    let (output, _) = invoke(
        &mut target_command(&recipes, "isinstalled", &target),
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
        &mut target_command(&recipes, "isinstalled", &target),
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
