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
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")), "{stdout}");
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
    assert!(stderr.contains("not found"), "{stderr}");
}

const LIFECYCLE: &str = r#"
const RECIPE_API = 2;
let ctx = #{name:"cli-product", state:#{installs:0,cleaned:false},
    source:join_path(RUNTIME.paths.build,"source"),built:join_path(RUNTIME.paths.build,"built"),
    product:join_path(RUNTIME.paths.output,"product")};
fn is_acquired(ctx) { if is_file(ctx.source) { return ready(ctx); } needs_work(ctx,"acquire") }
fn is_built(ctx) { if is_file(ctx.built) { return ready(ctx); } needs_work(ctx,"build") }
fn is_installed(ctx) {
    if !is_file(ctx.product) { return needs_work(ctx,"install"); }
    if read_file(ctx.product)!="actual CLI product" { throw "wrong product"; }
    ctx.outputs.product=ctx.product; ready(ctx)
}
fn acquire(ctx) { write_file(ctx.source,"actual CLI product"); ctx }
fn build(ctx) { copy_file(ctx.source,ctx.built); ctx }
fn install(ctx) { copy_file(ctx.built,ctx.product); ctx.state.installs+=1; ctx }
fn remove(ctx) { rm(ctx.product); ctx.outputs=#{}; ctx.state.removed=true; ctx }
fn cleanup(ctx,reason) { if reason=="manual" { rm(ctx.built); ctx.state.cleaned=true; } ctx }
"#;

#[test]
fn test_cli_current_lifecycle_reuses_checks_removes_and_preserves_source() {
    let (_dir, recipes) = create_test_env();
    write_recipe(&recipes, "simple", LIFECYCLE);
    let mut first = None;
    for operation in ["install", "install", "isinstalled", "cleanup", "remove"] {
        let output = run_recipe(&[operation, "simple"], &recipes);
        assert_success(&output);
        let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(ctx["state"]["installs"], 1);
        if operation == "install" {
            let path = std::path::PathBuf::from(ctx["outputs"]["product"].as_str().unwrap());
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "actual CLI product"
            );
            first.get_or_insert(path);
        }
        if operation == "cleanup" {
            assert_eq!(ctx["state"]["cleaned"], true);
        }
        if operation == "remove" {
            assert_eq!(ctx["state"]["removed"], true);
        }
    }
    let output = run_recipe(&["isinstalled", "simple"], &recipes);
    assert!(!output.status.success());
    assert_eq!(
        std::fs::read_to_string(first.unwrap()).unwrap(),
        "actual CLI product"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("simple.rhai")).unwrap(),
        LIFECYCLE
    );
}

#[test]
fn test_cli_accepts_explicit_recipe_path_without_rewriting_it() {
    let (dir, recipes) = create_test_env();
    let path = dir.path().join("external.rhai");
    std::fs::write(&path, LIFECYCLE).unwrap();
    let output = run_recipe(&["install", path.to_str().unwrap()], &recipes);
    assert_success(&output);
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(ctx["outputs"]["product"].as_str().unwrap()).unwrap(),
        "actual CLI product"
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), LIFECYCLE);
}

#[test]
fn test_cli_remove_no_function_and_explicit_defines() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "noremove",
        r#"const RECIPE_API = 2; let ctx = #{name:"no-remove"};"#,
    );
    let output = run_recipe(&["remove", "noremove"], &recipes);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no remove function"));
    let source = r#"const RECIPE_API = 2; let ctx = #{name:"remove-define"}; fn remove(ctx) { ctx.state.reason=REMOVE_REASON; ctx }"#;
    write_recipe(&recipes, "defined", source);
    let output = run_recipe(
        &["remove", "defined", "--define", "REMOVE_REASON=policy"],
        &recipes,
    );
    assert_success(&output);
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(ctx["state"]["reason"], "policy");
    assert_eq!(
        std::fs::read_to_string(recipes.join("defined.rhai")).unwrap(),
        source
    );
}

#[test]
fn test_cli_inherited_recipe_preserves_base_and_publishes_child_facts() {
    let (_dir, recipes) = create_test_env();
    let base = r#"const RECIPE_API = 2;
let ctx = #{name:"base",message:"base"};
fn is_built(ctx) { ready(ctx) }
fn is_installed(ctx) { if !is_file("/out/product") { return needs_work(ctx,"install"); } ctx.outputs.product="/out/product"; ready(ctx) }
fn install(ctx) { write_file("/out/product",ctx.message); ctx }
"#;
    write_recipe(&recipes, "base", base);
    let child = r#"//! extends: base.rhai
const RECIPE_API = 2; let ctx = #{name:"child",message:"child payload"};"#;
    write_recipe(&recipes, "child", child);
    let output = run_recipe(&["install", "child"], &recipes);
    assert_success(&output);
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(ctx["name"], "child");
    assert_eq!(
        std::fs::read_to_string(ctx["outputs"]["product"].as_str().unwrap()).unwrap(),
        "child payload"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("base.rhai")).unwrap(),
        base
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("child.rhai")).unwrap(),
        child
    );
}

#[test]
fn test_cli_recipe_directly_in_temp_root_does_not_confuse_domain_tmp_with_host_path() {
    use std::io::Write;
    let (_dir, recipes) = create_test_env();
    let mut recipe = tempfile::Builder::new()
        .prefix("recipe-root-")
        .suffix(".rhai")
        .tempfile()
        .unwrap();
    recipe.write_all(LIFECYCLE.as_bytes()).unwrap();
    recipe.as_file().sync_all().unwrap();
    let output = run_recipe(
        &[
            "install",
            recipe.path().to_str().unwrap(),
            "--no-persist-state",
        ],
        &recipes,
    );
    assert_success(&output);
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(ctx["outputs"]["product"].as_str().unwrap()).unwrap(),
        "actual CLI product"
    );
    assert_eq!(std::fs::read_to_string(recipe.path()).unwrap(), LIFECYCLE);
}
