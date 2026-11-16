use super::*;

fn command_recipe(recipes: &Path, body: &str) {
    write_recipe(
        recipes,
        "command-options",
        &format!(
            r#"
const RECIPE_API = 1;
const NEEDS = #{{ outputs: #{{ work: "directory" }} }};
fn record(out, name, value) {{ write_file(join_path(out, name), value.to_string()); }}
fn build(src, out) {{
    {body}
}}
fn cleanup(reason) {{
    if is_file(join_path(WORK, "should-not-exist")) || is_file("/should-not-exist") {{ throw "failed command continued"; }}
    log("COMMAND_FAILURE_PRESERVED");
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
    record(out, "before", env("RECIPE_COMMAND_VALUE"));
    write_file(join_path(out, "source payload.txt"), "literal argv payload");
    exec_checked("cp", ["source payload.txt", "copied 'literal' $(touch INJECTED).txt"], #{cwd: out});
    exec_checked("test", ["-f", join_path(out, "copied 'literal' $(touch INJECTED).txt")]);
    exec_checked("sh", ["-c", "if read -r value; then exit 41; fi; printf 'argv stdout\\n'; printf 'argv stderr\\n' >&2"]);
    record(out, "cwd", exec_output("pwd", [], #{cwd: WORK}));
    record(out, "child_value", exec_output("sh", ["-c", "printf '%s' \"$RECIPE_COMMAND_VALUE\""], #{env: #{RECIPE_COMMAND_VALUE: "child-only"}}));
    record(out, "status", exec("sh", ["-c", "if read -r value; then exit 41; fi; exit 19"], #{cwd: WORK}));
    record(out, "after", env("RECIPE_COMMAND_VALUE"));
"#,
    );
    let mut child = recipe_command(&recipes)
        .args(["install", "command-options", "--no-persist-state"])
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
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let work = Path::new(result["outputs"]["work"].as_str().unwrap());
    let recorded = |name: &str| std::fs::read_to_string(work.join(name)).unwrap();
    assert_eq!(recorded("before"), "");
    assert_eq!(recorded("after"), "");
    assert_eq!(recorded("child_value"), "child-only");
    assert_eq!(recorded("status"), "19");
    assert_eq!(recorded("cwd").trim(), "/work/build");
    assert_eq!(
        recorded("copied 'literal' $(touch INJECTED).txt"),
        "literal argv payload"
    );
    assert!(!work.join("INJECTED").exists());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("argv stdout"));
    assert!(stderr.contains("argv stderr"));
}

#[test]
fn cli_checked_command_failure_stops_install_and_retains_diagnostics() {
    let (_dir, recipes) = create_test_env();
    command_recipe(
        &recipes,
        r#"
    exec_checked("sh", ["-c", "printf 'COMMAND_FAILED_DETAIL\\n' >&2; exit 17"], #{cwd: WORK, env: #{PRIVATE_OPTION: "must-not-appear-in-errors"}});
    write_file(join_path(WORK, "should-not-exist"), "unexpected install continuation");
"#,
    );
    let output = run_recipe(
        &["install", "command-options", "--no-persist-state"],
        &recipes,
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("COMMAND_FAILURE_PRESERVED"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("COMMAND_FAILED_DETAIL"));
    assert!(stderr.contains("exit status: 17"), "{stderr}");
    assert!(!stderr.contains("must-not-appear-in-errors"));
}

#[test]
fn cli_captured_shell_failure_preserves_stderr_and_working_directory() {
    let (_dir, recipes) = create_test_env();
    command_recipe(
        &recipes,
        r#"
    record(out, "output", shell_output_in(WORK, "printf 'CAPTURED_FAILURE_DETAIL\\n' >&2; exit 29"));
    write_file(join_path(WORK, "should-not-exist"), "unexpected continuation");
"#,
    );
    let output = run_recipe(
        &["install", "command-options", "--no-persist-state"],
        &recipes,
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("COMMAND_FAILURE_PRESERVED"));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("CAPTURED_FAILURE_DETAIL"));
    assert!(stderr.contains("exit status: 29"), "{stderr}");
    assert!(stderr.contains("/work/build"));
}

#[test]
fn cli_shell_helpers_preserve_status_capture_and_payloads() {
    let (_dir, recipes) = create_test_env();
    command_recipe(
        &recipes,
        r#"
    shell("printf 'shell diagnostic\\n'");
    shell_in(out, "printf 'shell payload' > 'shell payload.txt'");
    record(out, "status", shell_status("exit 7"));
    record(out, "missing", shell_status_in(WORK, "test -f missing"));
    record(out, "payload", shell_output_in(out, "cat 'shell payload.txt'"));
    record(out, "capture", shell_output("printf 'captured value'"));
"#,
    );
    let output = run_recipe(
        &["install", "command-options", "--no-persist-state"],
        &recipes,
    );
    assert_success(&output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let work = Path::new(result["outputs"]["work"].as_str().unwrap());
    let recorded = |name: &str| std::fs::read_to_string(work.join(name)).unwrap();
    assert_eq!(recorded("status"), "7");
    assert_eq!(recorded("missing"), "1");
    assert_eq!(recorded("payload"), "shell payload");
    assert_eq!(recorded("capture"), "captured value");
    assert_eq!(recorded("shell payload.txt"), "shell payload");
    assert!(String::from_utf8_lossy(&output.stderr).contains("shell diagnostic"));
}

#[test]
fn cli_command_option_errors_prevent_child_execution() {
    for (options, message) in [
        ("#{cwd: 12}", "cwd must be a string"),
        ("#{cwd: \"\"}", "cwd must not be empty"),
        ("#{env: 12}", "env must be a map"),
        ("#{env: #{TASK_VALUE: 12}}", "TASK_VALUE must be a string"),
        ("#{directory: WORK}", "unknown command option"),
    ] {
        let (_dir, recipes) = create_test_env();
        command_recipe(
            &recipes,
            &format!(r#"exec_checked("sh", ["-c", "printf ran > should-not-exist"], {options});"#),
        );
        let output = run_recipe(
            &["install", "command-options", "--no-persist-state"],
            &recipes,
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        assert!(String::from_utf8_lossy(&output.stderr).contains("COMMAND_FAILURE_PRESERVED"));
    }
}
