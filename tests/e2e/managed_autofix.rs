//! Actual coordinator patch/retry around managed execution; provider text is controlled.
use super::*;
const SOURCE: &str = r#"
const RECIPE_API = 2;
const BUILD_VALUE = "broken";
let ctx = #{name:"outer-repair", state:#{acquired:false}};
fn is_acquired(ctx) {
    if ctx.state.acquired { return ready(ctx); }
    needs_work(ctx,"acquire")
}
fn acquire(ctx) {
    ctx.state.acquired=true;
    if BUILD_VALUE == "broken" { ctx.state.failed_identity=true; }
    else if ctx.state.contains("failed_identity") { throw "old identity state crossed patch"; }
    ctx
}
fn is_built(ctx) {
    if is_file("/work/build/product") {
        if read_file("/work/build/product")=="fixed" { return ready(ctx); }
    }
    return needs_work(ctx,"produce fixed");
}
fn build(ctx) { write_file("/work/build/product",BUILD_VALUE); ctx }
fn is_installed(ctx) {
    if !is_file("/out/product") { return needs_work(ctx,"install"); }
    if read_file("/out/product")!="fixed" { throw "bad installed bytes"; }
    ctx.outputs.product="/out/product"; ready(ctx)
}
fn install(ctx) { copy_file("/work/build/product","/out/product"); ctx }
"#;
fn prepare(case: &str) -> (TempDir, std::path::PathBuf) {
    let (dir, recipes) = create_test_env();
    write_recipe(&recipes, "check", SOURCE);
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .arg(dir.path())
            .status()
            .unwrap()
            .success()
    );
    let provider = dir.path().join("controlled-provider.py");
    std::fs::write(&provider,r#"import difflib, os, pathlib, sys
sys.stdin.read()
p=pathlib.Path('recipes/check.rhai')
s=p.read_text()
pathlib.Path('provider.calls').open('a').write('called\n')
if os.environ['REPAIR_CASE']=='build':
 n=s.replace('const BUILD_VALUE = "broken";', 'const BUILD_VALUE = "fixed";')
else:
 n=s.replace('return needs_work(ctx,"produce fixed");', 'return ready(ctx);')
sys.stdout.write(''.join(difflib.unified_diff(s.splitlines(True),n.splitlines(True),fromfile='a/recipes/check.rhai',tofile='b/recipes/check.rhai')))
"#).unwrap();
    let config = dir.path().join("config/recipe");
    std::fs::create_dir_all(&config).unwrap();
    let text = format!(
        r#"version = 1
default_provider = "claude"
timeout_secs = 10
[providers.codex]
bin = "/unused-provider"
[providers.claude]
bin = "/not-the-selected-profile"
[profiles.controlled.providers.claude]
bin = "/usr/bin/python3"
args = [{provider:?}]
[profiles.controlled.providers.claude.env]
REPAIR_CASE = {case:?}
"#,
        provider = provider.to_str().unwrap()
    );
    std::fs::write(config.join("llm.toml"), text).unwrap();
    (dir, recipes)
}
fn invoke(recipes: &Path, autofix: bool) -> std::process::Output {
    let root = recipes.parent().unwrap();
    let mut command = recipe_command(recipes);
    command
        .args(["install", "check", "--recipes-path"])
        .arg(recipes)
        .arg("--build-dir")
        .arg(root.join("destination"))
        .arg("--state-dir")
        .arg(root.join("retained"));
    if autofix {
        command
            .args([
                "--autofix",
                "--autofix-attempts",
                "1",
                "--llm-profile",
                "controlled",
                "--autofix-cwd",
            ])
            .arg(root)
            .arg("--autofix-allow-path")
            .arg(recipes);
    }
    command.output().unwrap()
}
#[test]
fn managed_autofix_retries_outside_domain_with_new_identity_and_unchanged_checks() {
    let (dir, recipes) = prepare("build");
    let output = invoke(&recipes, true);
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(result["outputs"]["product"].as_str().unwrap()).unwrap(),
        "fixed"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("provider.calls")).unwrap(),
        "called\n"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        SOURCE.replace(
            "const BUILD_VALUE = \"broken\";",
            "const BUILD_VALUE = \"fixed\";"
        )
    );
    let records = walkdir::WalkDir::new(dir.path().join("retained"))
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() == "current.json")
        .map(|entry| {
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(entry.path()).unwrap())
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        records.len(),
        2,
        "patch did not receive a fresh state identity"
    );
    assert!(
        records
            .iter()
            .any(|record| record["states"]["/inputs/main.rhai"]["failed_identity"] == true)
    );
    assert!(
        !result["state"]
            .as_object()
            .unwrap()
            .contains_key("failed_identity")
    );
}
#[test]
fn managed_autofix_rejects_protected_check_patch_without_modifying_source() {
    let (dir, recipes) = prepare("protected");
    let output = invoke(&recipes, true);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("protected lifecycle function"), "{error}");
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        SOURCE
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("provider.calls")).unwrap(),
        "called\n"
    );
    assert!(!dir.path().join(".artifacts/ginux-export").exists());
}
#[test]
fn managed_execution_without_autofix_never_invokes_provider() {
    let (dir, recipes) = prepare("build");
    let output = invoke(&recipes, false);
    assert!(!output.status.success());
    assert!(!dir.path().join("provider.calls").exists());
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        SOURCE
    );
}
