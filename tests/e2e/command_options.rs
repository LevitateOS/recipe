use super::*;

fn command_recipe(recipes: &Path, body: &str) {
    write_recipe(
        recipes,
        "command-options",
        &format!(
            r#"
let ctx = #{{ name: "command-options" }};
fn is_installed(ctx) {{ throw "run requested"; }}
fn is_acquired(ctx) {{ ctx }}
fn install(ctx) {{
    mkdir(BUILD_DIR);
    {body}
    ctx
}}
"#
        ),
    );
}

#[test]
fn cli_checked_argv_preserves_arguments_cwd_environment_and_control_input() {
    use std::io::Write;
    use std::process::Stdio;

    let (dir, recipes) = create_test_env();
    command_recipe(
        &recipes,
        r#"
    ctx.before = env("RECIPE_COMMAND_VALUE");
    write_file(join_path(BUILD_DIR, "source payload.txt"), "literal argv payload");
    exec_checked("cp", ["source payload.txt", "copied 'literal' $(touch INJECTED).txt"], #{cwd: BUILD_DIR});
    exec_checked("test", ["-f", join_path(BUILD_DIR, "copied 'literal' $(touch INJECTED).txt")]);
    exec_checked("sh", ["-c", "if read -r value; then exit 41; fi; printf 'argv stdout\\n'; printf 'argv stderr\\n' >&2"]);
    ctx.cwd = exec_output("pwd", [], #{cwd: BUILD_DIR});
    ctx.child_value = exec_output("sh", ["-c", "printf '%s' \"$RECIPE_COMMAND_VALUE\""], #{env: #{RECIPE_COMMAND_VALUE: "child-only"}});
    ctx.status = exec("sh", ["-c", "if read -r value; then exit 41; fi; exit 19"], #{cwd: BUILD_DIR});
    ctx.after = env("RECIPE_COMMAND_VALUE");
"#,
    );
    let mut child = recipe_command(&recipes)
        .args(["install", "command-options", "--no-persist-ctx"])
        .arg("--recipes-path")
        .arg(&recipes)
        .arg("--build-dir")
        .arg(dir.path().join("build"))
        .env("RECIPE_COMMAND_VALUE", "parent-value")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"coordinator input\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_success(&output);
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(ctx["before"], "parent-value");
    assert_eq!(ctx["after"], "parent-value");
    assert_eq!(ctx["child_value"], "child-only");
    assert_eq!(ctx["status"], 19);
    assert_eq!(
        ctx["cwd"].as_str().unwrap().trim(),
        dir.path().join("build").to_str().unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(
            dir.path()
                .join("build/copied 'literal' $(touch INJECTED).txt")
        )
        .unwrap(),
        "literal argv payload"
    );
    assert!(!dir.path().join("build/INJECTED").exists());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("argv stdout"));
    assert!(stderr.contains("argv stderr"));
}

#[test]
fn cli_checked_command_failure_stops_install_and_retains_diagnostics() {
    let (dir, recipes) = create_test_env();
    command_recipe(
        &recipes,
        r#"
    exec_checked("sh", ["-c", "printf 'COMMAND_FAILED_DETAIL\\n' >&2; exit 17"], #{cwd: BUILD_DIR, env: #{PRIVATE_OPTION: "must-not-appear-in-errors"}});
    write_file(join_path(BUILD_DIR, "should-not-exist"), "unexpected install continuation");
"#,
    );
    let output = run_recipe(
        &["install", "command-options", "--no-persist-ctx"],
        &recipes,
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!dir.path().join("build/should-not-exist").exists());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("COMMAND_FAILED_DETAIL"));
    assert!(stderr.contains("17"));
    assert!(!stderr.contains("must-not-appear-in-errors"));
}

#[test]
fn cli_captured_shell_failure_preserves_stderr_and_working_directory() {
    let (dir, recipes) = create_test_env();
    command_recipe(
        &recipes,
        r#"
    ctx.output = shell_output_in(BUILD_DIR, "printf 'CAPTURED_FAILURE_DETAIL\\n' >&2; exit 29");
    write_file(join_path(BUILD_DIR, "should-not-exist"), "unexpected continuation");
"#,
    );
    let output = run_recipe(
        &["install", "command-options", "--no-persist-ctx"],
        &recipes,
    );
    assert!(!output.status.success());
    assert!(!dir.path().join("build/should-not-exist").exists());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("CAPTURED_FAILURE_DETAIL"));
    assert!(stderr.contains("29"));
    assert!(stderr.contains(dir.path().join("build").to_str().unwrap()));
}

#[test]
fn cli_legacy_shell_helpers_preserve_status_capture_and_payloads() {
    let (dir, recipes) = create_test_env();
    command_recipe(
        &recipes,
        r#"
    shell("printf 'legacy shell diagnostic\\n'");
    shell_in(BUILD_DIR, "printf 'shell payload' > 'legacy payload.txt'");
    ctx.status = shell_status("exit 7");
    ctx.missing = shell_status_in(BUILD_DIR, "test -f missing");
    ctx.payload = shell_output_in(BUILD_DIR, "cat 'legacy payload.txt'");
    ctx.capture = shell_output("printf 'captured value'");
"#,
    );
    let output = run_recipe(
        &["install", "command-options", "--no-persist-ctx"],
        &recipes,
    );
    assert_success(&output);
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(ctx["status"], 7);
    assert_eq!(ctx["missing"], 1);
    assert_eq!(ctx["payload"], "shell payload");
    assert_eq!(ctx["capture"], "captured value");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("build/legacy payload.txt")).unwrap(),
        "shell payload"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("legacy shell diagnostic"));
}

#[test]
fn cli_command_option_errors_prevent_child_execution() {
    for (options, message) in [
        ("#{cwd: 12}", "cwd must be a string"),
        ("#{cwd: \"\"}", "cwd must not be empty"),
        ("#{env: 12}", "env must be a map"),
        ("#{env: #{TASK_VALUE: 12}}", "TASK_VALUE must be a string"),
        ("#{directory: BUILD_DIR}", "unknown command option"),
    ] {
        let (dir, recipes) = create_test_env();
        command_recipe(
            &recipes,
            &format!(r#"exec_checked("sh", ["-c", "printf ran > should-not-exist"], {options});"#),
        );
        let output = run_recipe(
            &["install", "command-options", "--no-persist-ctx"],
            &recipes,
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        assert!(!dir.path().join("should-not-exist").exists());
    }
}
