use super::*;
use levitate_recipe::worker::protocol::{FramePayload, FrameReceiver, FrameSender, RequestPayload};
use std::io::Write;
use std::process::Stdio;

#[test]
fn public_cli_worker_environment_never_selects_host_execution() {
    let (dir, recipes) = create_test_env();
    let sentinel = dir.path().join("host-effect");
    write_recipe(
        &recipes,
        "check",
        &format!(
            "write_file({:?}, \"unexpected host execution\"); let ctx = #{{name: \"guard\"}}; fn is_installed(ctx) {{ ctx }}",
            sentinel.to_string_lossy()
        ),
    );
    for invocation in [None, Some("spoofed-public-call")] {
        let mut command = recipe_command(&recipes);
        command
            .env("RECIPE_WORKER", "1")
            .env_remove("RECIPE_BACKEND")
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
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("backend startup failure") && error.contains("launcher"),
            "{error}"
        );
        assert!(!sentinel.exists());
    }
    let output = recipe_command(&recipes)
        .env("RECIPE_WORKER", "1")
        .env("RECIPE_INVOCATION", "spoofed-public-call")
        .args([
            "isinstalled",
            "check",
            "--backend",
            "host",
            "--recipes-path",
        ])
        .arg(&recipes)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("public host execution was removed"));
    assert!(!sentinel.exists());
}

#[test]
fn private_worker_rejects_missing_bootstrap_and_mismatched_request_before_setup() {
    let (dir, recipes) = create_test_env();
    let sentinel = dir.path().join("host-effect");
    write_recipe(
        &recipes,
        "check",
        &format!(
            "write_file({:?}, \"unexpected setup\"); let ctx = #{{name: \"guard\"}}; fn is_installed(ctx) {{ ctx }}",
            sentinel.to_string_lossy()
        ),
    );
    let recipe = recipes.join("check.rhai");
    let output = recipe_command(&recipes)
        .env_remove("RECIPE_WORKER")
        .env_remove("RECIPE_INVOCATION")
        .arg(levitate_recipe::worker::WORKER_ENTRY_ARG)
        .arg(&recipe)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("private worker entry requires"));
    assert!(!sentinel.exists());

    let build = dir.path().join("prepared-build");
    std::fs::create_dir(&build).unwrap();
    let mut child = recipe_command(&recipes)
        .env("RECIPE_WORKER", "1")
        .env("RECIPE_INVOCATION", "bootstrap-id")
        .env("BUILD_DIR", &build)
        .arg(levitate_recipe::worker::WORKER_ENTRY_ARG)
        .arg(&recipe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut request = Vec::new();
    FrameSender::new()
        .send(
            &mut request,
            &FramePayload::Request(RequestPayload {
                invocation_id: "other-id".into(),
                operation: "is_installed".into(),
                persist_ctx: false,
                recipe_digest: "0".repeat(64),
            }),
        )
        .unwrap();
    child.stdin.take().unwrap().write_all(&request).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success()); // A protocol failure is reported as frames.
    let mut stream = std::io::Cursor::new(&output.stdout);
    let mut receiver = FrameReceiver::new();
    let FramePayload::Failure(failure) = receiver.recv(&mut stream).unwrap().payload else {
        panic!("expected failure frame")
    };
    assert!(
        failure
            .message
            .contains("request invocation id does not match worker bootstrap")
    );
    let FramePayload::Result(result) = receiver.recv(&mut stream).unwrap().payload else {
        panic!("expected result frame")
    };
    assert!(!result.ok);
    assert_eq!(stream.position(), output.stdout.len() as u64);
    assert!(!sentinel.exists());
}
