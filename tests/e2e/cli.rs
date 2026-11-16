use super::*;
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
    assert!(stdout.contains("state"));
    assert!(stdout.contains("cleanup"));
    assert!(!stdout.contains("isinstalled"));
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

#[test]
fn test_cli_install_nonexistent_package() {
    let (_dir, recipes) = create_test_env();

    let output = run_recipe(&["install", "nonexistent"], &recipes);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found"), "{stderr}");
}

const LIFECYCLE: &str = r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ "package": "directory" } };
const PACKAGE = #{ version: "1.0.0", description: "CLI lifecycle product" };
fn build(src, out) {
    mkdir(join_path(out, "share"));
    write_file(join_path(out, "share/product"), "actual CLI product");
}
"#;

fn json_of(output: &std::process::Output) -> serde_json::Value {
    assert_success(output);
    serde_json::from_slice(&output.stdout).expect("the CLI prints its result as JSON")
}

fn step_status<'a>(result: &'a serde_json::Value, step: &str) -> &'a str {
    result["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["step"] == step)
        .and_then(|entry| entry["status"].as_str())
        .unwrap_or_else(|| panic!("no {step} step in {result}"))
}

#[test]
fn test_cli_current_lifecycle_reuses_records_removes_and_preserves_source() {
    let (_dir, recipes) = create_test_env();
    write_recipe(&recipes, "simple", LIFECYCLE);

    let first = json_of(&run_recipe(&["install", "simple"], &recipes));
    assert_eq!(first["schema_version"], 1);
    assert_eq!(first["operation"], "install");
    assert_eq!(first["name"], "simple");
    assert_eq!(first["version"], "1.0.0");
    assert_eq!(first["state"], "installed");
    assert_eq!(step_status(&first, "download"), "none");
    assert_eq!(step_status(&first, "build"), "done");
    assert_eq!(step_status(&first, "install"), "done");
    assert_eq!(first["kept"], false);
    let package = std::path::PathBuf::from(first["outputs"]["package"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(package.join("share/product")).unwrap(),
        "actual CLI product"
    );
    assert!(first["activation"]["profile"].is_string());

    let second = json_of(&run_recipe(&["install", "simple"], &recipes));
    assert_eq!(second["state"], "installed");
    assert_eq!(step_status(&second, "build"), "already");
    assert_eq!(step_status(&second, "install"), "already");

    let state = json_of(&run_recipe(&["state", "simple"], &recipes));
    assert_eq!(state["schema_version"], 1);
    assert_eq!(state["state"], "installed");
    assert_eq!(state["installed_version"], "1.0.0");

    let cleaned = json_of(&run_recipe(&["cleanup", "simple"], &recipes));
    assert_eq!(cleaned["operation"], "cleanup");
    assert_eq!(cleaned["state"], "installed");
    assert!(package.join("share/product").is_file());

    let removed = json_of(&run_recipe(&["remove", "simple"], &recipes));
    assert_eq!(removed["operation"], "remove");
    assert_eq!(removed["state"], "none");
    assert!(!package.join("share/product").exists());
    let state = json_of(&run_recipe(&["state", "simple"], &recipes));
    assert_eq!(state["state"], "none");
    assert_eq!(state["installed_version"], serde_json::Value::Null);
    let installed = json_of(&run_recipe(&["installed"], &recipes));
    assert_eq!(installed["packages"], serde_json::json!([]));

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
    let result = json_of(&output);
    assert_eq!(result["name"], "external");
    assert_eq!(
        std::fs::read_to_string(
            Path::new(result["outputs"]["package"].as_str().unwrap()).join("share/product")
        )
        .unwrap(),
        "actual CLI product"
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), LIFECYCLE);
}

#[test]
fn test_cli_remove_of_a_program_that_is_not_installed_fails() {
    let (_dir, recipes) = create_test_env();
    write_recipe(&recipes, "simple", LIFECYCLE);
    let output = run_recipe(&["remove", "simple"], &recipes);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("is not installed"), "{error}");
    assert_eq!(
        std::fs::read_to_string(recipes.join("simple.rhai")).unwrap(),
        LIFECYCLE
    );
}

#[test]
fn test_cli_inherited_recipe_preserves_base_and_publishes_child_facts() {
    let (_dir, recipes) = create_test_env();
    let base = r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
fn build(src, out) { write_file(out, payload()); }
"#;
    write_recipe(&recipes, "base", base);
    let child = r#"//! extends: base.rhai
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
fn payload() { "child payload" }
"#;
    write_recipe(&recipes, "child", child);
    let result = json_of(&run_recipe(&["install", "child"], &recipes));
    assert_eq!(result["name"], "child");
    assert_eq!(
        std::fs::read_to_string(result["outputs"]["product"].as_str().unwrap()).unwrap(),
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
    let result = json_of(&output);
    assert_eq!(
        std::fs::read_to_string(
            Path::new(result["outputs"]["package"].as_str().unwrap()).join("share/product")
        )
        .unwrap(),
        "actual CLI product"
    );
    assert_eq!(std::fs::read_to_string(recipe.path()).unwrap(), LIFECYCLE);
}

#[test]
fn test_cli_other_api_numbers_are_refused_with_the_line_before_any_setup() {
    let (_dir, recipes) = create_test_env();
    for number in [2, 3, 99] {
        let name = format!("api{number}");
        write_recipe(
            &recipes,
            &name,
            &format!(
                "const RECIPE_API = {number};\nconst NEEDS = #{{}};\nprint(\"UNEXPECTED_OLD_SETUP\");\n"
            ),
        );
        let output = run_recipe(&["install", &name], &recipes);
        assert!(!output.status.success(), "{name}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("Recipe accepts only RECIPE_API = 1"),
            "{name}: {error}"
        );
        assert!(error.contains("line 1:"), "{name}: {error}");
        assert!(!error.contains("UNEXPECTED_OLD_SETUP"), "{name}: {error}");
    }
}
