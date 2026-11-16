use super::*;

#[test]
fn cli_engine_constants_reach_build_and_the_functions_it_calls() {
    let (dir, recipes) = create_test_env();
    let selected = dir.path().join("selected-input");
    std::fs::write(&selected, "input bytes").unwrap();
    write_recipe(
        &recipes,
        "constants-tool",
        r##"const RECIPE_API = 1;
const NEEDS = #{ inputs: #{ chosen: "file" }, defines: #{ FLAVOUR: "plain" }, outputs: #{ report: "file" } };
fn describe() {
    // A function the engine does not call sees the constants too.
    "jobs=" + JOBS.to_string() + "\n"
        + "libc=" + HOST_LIBC + "\n"
        + "flavour=" + DEFINES.FLAVOUR + "\n"
        + "input=" + read_file(INPUTS.chosen) + "\n"
        + "prefix=" + (if PREFIX == () { "none" } else { PREFIX }) + "\n"
        + "format=" + (if PACKAGE_FORMAT == () { "none" } else { PACKAGE_FORMAT }) + "\n"
        + "work=" + (if is_dir(WORK) { "dir" } else { "missing" }) + "\n"
}
fn build(src, out) {
    if src.len() != 0 { throw "a recipe without SOURCES has an empty src"; }
    write_file(out, describe());
}
"##,
    );
    let binding = format!("chosen={}", selected.display());
    let output = run_recipe(
        &[
            "install",
            "constants-tool",
            "--input",
            &binding,
            "--define",
            "FLAVOUR=spicy",
            "--jobs",
            "3",
        ],
        &recipes,
    );
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let report = std::fs::read_to_string(result["outputs"]["report"].as_str().unwrap()).unwrap();
    let lines = report.lines().collect::<Vec<_>>();
    assert_eq!(lines[0], "jobs=3", "{report}");
    assert!(
        ["glibc", "musl", "unknown"].contains(&&lines[1]["libc=".len()..]),
        "{report}"
    );
    assert_eq!(
        &lines[2..],
        [
            "flavour=spicy",
            "input=input bytes",
            "prefix=none",
            "format=none",
            "work=dir"
        ],
        "{report}"
    );
}

#[test]
fn cli_old_recipe_forms_are_refused_with_the_line_to_change() {
    let (_dir, recipes) = create_test_env();
    for (name, body, diagnostic) in [
        ("old-hook", "fn install(ctx) { ctx }", "line 3:"),
        (
            "old-check",
            "fn is_installed(arg) { arg }",
            "is an API 2 hook",
        ),
        ("old-context", "fn build(src, ctx) { }", "takes ctx"),
        (
            "old-ctx-variable",
            "let ctx = #{name: \"old-ctx-variable\"};",
            "ctx is the API 2 context",
        ),
        (
            "old-runtime",
            "const RUNTIME = #{};",
            "RUNTIME is the API 2 runtime map",
        ),
    ] {
        write_recipe(
            &recipes,
            name,
            &format!(
                "const RECIPE_API = 1;\nconst NEEDS = #{{}};\n{body}\nprint(\"UNEXPECTED_OLD_SETUP\");\n"
            ),
        );
        let output = run_recipe(&["install", name, "--no-persist-state"], &recipes);
        assert!(!output.status.success(), "{name} unexpectedly succeeded");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(diagnostic), "{name}: {stderr}");
        assert!(stderr.contains("line "), "{name}: {stderr}");
        assert!(!stderr.contains("UNEXPECTED_OLD_SETUP"), "{name}: {stderr}");
    }
}

#[test]
fn managed_top_level_code_runs_once_and_modules_are_imported() {
    let (_dir, recipes) = create_test_env();
    std::fs::write(
        recipes.join("facts.rhai"),
        "fn value() { \"module fact\" }\n",
    )
    .unwrap();
    let source = r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
import "facts.rhai" as facts;
append_file(join_path(WORK, "setup"), "once\n");
fn build(src, out) {
    let setup = read_file(join_path(WORK, "setup"));
    if setup != "once\n" { throw "top-level code repeated"; }
    write_file(out, facts::value() + ":" + setup);
}
"#;
    write_recipe(&recipes, "current", source);
    let output = run_recipe(&["install", "current"], &recipes);
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(result["outputs"]["product"].as_str().unwrap()).unwrap(),
        "module fact:once\n"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("current.rhai")).unwrap(),
        source
    );
}
