//! Public CLI flags/environment cannot select an in-process recipe executor.
use super::*;
#[test]
fn public_cli_worker_environment_never_selects_host_execution() {
    let (dir, recipes) = create_test_env();
    let sentinel = dir.path().join("host-effect");
    let source = format!(
        "const RECIPE_API = 2; write_file({:?},\"host effect\"); let ctx=#{{name:\"guard\"}}; fn is_installed(ctx) {{ ready(ctx) }}",
        sentinel.to_str().unwrap()
    );
    write_recipe(&recipes, "check", &source);
    for invocation in [None, Some("spoofed-public-call")] {
        let mut command = recipe_command(&recipes);
        command
            .env("RECIPE_WORKER", "1")
            .env("GINUX_BIN", dir.path().join("missing-ginux"));
        if let Some(id) = invocation {
            command.env("RECIPE_INVOCATION", id);
        } else {
            command.env_remove("RECIPE_INVOCATION");
        }
        let output = command
            .args(["isinstalled", "check", "--recipes-path"])
            .arg(&recipes)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("backend startup failure"));
        assert!(!sentinel.exists());
    }
    let output = recipe_command(&recipes)
        .args(["isinstalled", "check", "--backend", "host"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
    assert!(!sentinel.exists());
    assert_eq!(
        std::fs::read_to_string(recipes.join("check.rhai")).unwrap(),
        source
    );
}
#[test]
fn private_worker_rejects_missing_bootstrap_before_recipe_setup() {
    let (dir, recipes) = create_test_env();
    let sentinel = dir.path().join("host-effect");
    write_recipe(
        &recipes,
        "check",
        &format!(
            "const RECIPE_API = 2; write_file({:?},\"unexpected setup\"); let ctx=#{{name:\"guard\"}};",
            sentinel.to_str().unwrap()
        ),
    );
    let output = recipe_command(&recipes)
        .env_remove("RECIPE_WORKER")
        .env_remove("RECIPE_INVOCATION")
        .arg(levitate_recipe::worker::WORKER_ENTRY_ARG)
        .arg(recipes.join("check.rhai"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("private worker entry requires"));
    assert!(!sentinel.exists());
}
