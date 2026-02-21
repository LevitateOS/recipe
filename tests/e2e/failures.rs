use super::*;

#[test]
fn test_cli_install_acquire_failure() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "fail-acquire",
        r#"
let ctx = #{
    name: "fail-acquire",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    throw "Download failed!";
}

fn install(ctx) { ctx }
"#,
    );

    let output = run_recipe(&["install", "fail-acquire"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("acquire") || stderr.contains("Download failed"));
}

#[test]
fn test_cli_install_build_failure() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "fail-build",
        r#"
let ctx = #{
    name: "fail-build",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    throw "Compilation failed!";
}

fn install(ctx) { ctx }
"#,
    );

    let output = run_recipe(&["install", "fail-build"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("build") || stderr.contains("Compilation failed"));
}

#[test]
fn test_cli_install_install_failure() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "fail-install",
        r#"
let ctx = #{
    name: "fail-install",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn install(ctx) {
    throw "Install failed!";
}
"#,
    );

    let output = run_recipe(&["install", "fail-install"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("install") || stderr.contains("Install failed"));
}

#[test]
fn test_cli_install_emits_legacy_recipe_hook_logs() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "hooklog",
        r#"
let ctx = #{
    name: "hooklog",
    version: "1.0.0",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn install(ctx) { ctx }
"#,
    );

    let output = run_recipe(&["install", "hooklog"], &recipes);
    assert!(
        output.status.success(),
        "install failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(stderr.contains("[prepare]"));
    assert!(stderr.contains("[acquire]"));
    assert!(stderr.contains("[install]"));
    assert!(stderr.contains("is_installed check says recipe still needs this step"));
    assert!(stderr.contains("install step"));
}

#[test]
fn test_cli_install_emits_machine_recipe_hook_events_json() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "hooklog-json",
        r#"
let ctx = #{
    name: "hooklog-json",
    version: "1.0.0",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) { ctx }

fn install(ctx) { ctx }
"#,
    );

    let output = run_recipe(&["--machine-events", "install", "hooklog-json"], &recipes);
    assert!(
        output.status.success(),
        "install failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut saw_hook_events = 0;
    let mut saw_success = false;
    let mut saw_running = false;
    let mut saw_human = false;
    let mut saw_machine = false;

    for line in stderr.lines() {
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(line) {
            if event.get("event").and_then(|v| v.as_str()) == Some("recipe-hook") {
                saw_machine = true;
                saw_hook_events += 1;
                assert_eq!(
                    event.get("recipe").and_then(|v| v.as_str()),
                    Some("hooklog-json")
                );
                match event.get("status").and_then(|v| v.as_str()) {
                    Some("running") => saw_running = true,
                    Some("success") => saw_success = true,
                    _ => {}
                }
            }
            continue;
        }
        if line.contains('[')
            && line.contains(']')
            && (line.contains("Working on")
                || line.contains("is now running")
                || line.contains("is queued")
                || line.contains("Queued")
                || line.contains("finished")
                || line.contains("still needs"))
        {
            saw_human = true;
        }
    }

    assert!(saw_hook_events > 0, "no machine recipe-hook events found");
    assert!(saw_running, "expected at least one running hook event");
    assert!(saw_success, "expected at least one success hook event");
    assert!(saw_machine, "expected machine-readable hook events");
    assert!(saw_human, "expected human-readable hook events");
}

#[test]
fn test_cli_install_machine_events_failure_contains_reason() {
    let (_dir, recipes) = create_test_env();

    write_recipe(
        &recipes,
        "hooklog-fail",
        r#"
let ctx = #{
    name: "hooklog-fail",
    version: "1.0.0",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    throw "Download failed!";
}

fn install(ctx) { ctx }
"#,
    );

    let output = run_recipe(&["--machine-events", "install", "hooklog-fail"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);

    let mut saw_failed = false;
    let mut saw_reason = false;

    for line in stderr.lines() {
        if let Ok(event) = serde_json::from_str::<serde_json::Value>(line)
            && event.get("event").and_then(|v| v.as_str()) == Some("recipe-hook")
            && event.get("status").and_then(|v| v.as_str()) == Some("failed")
        {
            saw_failed = true;
            if event
                .get("msg")
                .and_then(|v| v.as_str())
                .is_some_and(|msg| msg.contains("Download failed!"))
            {
                saw_reason = true;
            }
        }
    }

    assert!(saw_failed, "expected machine failed hook event");
    assert!(
        saw_reason,
        "expected failed hook msg to include actionable reason"
    );
}
