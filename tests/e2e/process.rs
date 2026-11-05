use super::*;

#[test]
fn cli_exec_streams_diagnostics_to_stderr_and_preserves_status_result() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "exec-status",
        r#"
let ctx = #{ name: "exec-status" };
fn is_installed(ctx) { throw "run requested"; }
fn is_acquired(ctx) { ctx }
fn install(ctx) {
    ctx.status = exec("sh", ["-c", "printf 'child stdout\\n'; printf 'child stderr\\n' >&2; exit 23"]);
    if ctx.status != 23 { throw "unexpected child status"; }
    write_file(join_path(BUILD_DIR, "handled-status"), "child failure was handled explicitly");
    ctx
}
"#,
    );

    let output = run_recipe(&["install", "exec-status", "--no-persist-ctx"], &recipes);
    assert_success(&output);
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(ctx["status"], 23);
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("child stdout"));
    assert!(diagnostics.contains("child stderr"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("build/handled-status")).unwrap(),
        "child failure was handled explicitly"
    );
}
