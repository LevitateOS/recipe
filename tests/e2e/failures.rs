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
fn test_cli_install_build_failure_resumes_without_reacquiring() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "fail-build",
        r#"
let ctx = #{
    name: "fail-build",
    acquired: false,
    installed: false,
    source: join_path(RECIPE_DIR, "source.txt"),
    built: join_path(BUILD_DIR, "built.txt"),
    installed_file: join_path(RECIPE_DIR, "installed.txt"),
    allow_build: join_path(RECIPE_DIR, "allow-build"),
    trace: join_path(RECIPE_DIR, "lifecycle.log"),
    failure_cleanup: join_path(RECIPE_DIR, "cleanup-reason.txt"),
};
fn is_acquired(ctx) { if !is_file(ctx.source) { throw "source missing"; } ctx }
fn is_built(ctx) { if !is_file(ctx.built) { throw "build missing"; } ctx }
fn is_installed(ctx) { if !is_file(ctx.installed_file) { throw "install missing"; } ctx }
fn acquire(ctx) {
    write_file(ctx.source, "resumable-payload");
    append_file(ctx.trace, "acquire\n");
    ctx.acquired = true;
    ctx
}
fn build(ctx) {
    append_file(ctx.trace, "build\n");
    if !is_file(ctx.allow_build) { throw "BUILD_BLOCKED_FOR_RETRY"; }
    copy_file(ctx.source, ctx.built);
    ctx
}
fn install(ctx) {
    copy_file(ctx.built, ctx.installed_file);
    append_file(ctx.trace, "install\n");
    ctx.installed = true;
    ctx
}
fn cleanup(ctx, reason) {
    if reason == "auto.build.failure" { write_file(ctx.failure_cleanup, reason); }
    ctx
}
"#,
    );
    let output = run_recipe(&["install", "fail-build"], &recipes);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("BUILD_BLOCKED_FOR_RETRY"));
    assert!(
        output.stdout.is_empty(),
        "failed install emitted a success ctx"
    );
    assert!(
        !recipes.join("installed.txt").exists(),
        "install ran after build failure"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("lifecycle.log")).unwrap(),
        "acquire\nbuild\n"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("cleanup-reason.txt")).unwrap(),
        "auto.build.failure"
    );
    assert!(
        std::fs::read_to_string(recipes.join("fail-build.rhai"))
            .unwrap()
            .contains("acquired: true"),
        "completed acquire state was lost on failure"
    );

    std::fs::write(recipes.join("allow-build"), "ready").unwrap();
    let output = run_recipe(&["install", "fail-build"], &recipes);
    assert_success(&output);
    let installed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(installed["installed"], true);
    assert_eq!(
        std::fs::read_to_string(recipes.join("installed.txt")).unwrap(),
        "resumable-payload"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("lifecycle.log")).unwrap(),
        "acquire\nbuild\nbuild\ninstall\n",
        "retry must resume at build without downloading again"
    );
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

    let mut phase_events = Vec::new();
    let mut recipe_completions = 0;
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
                let hook = event["hook"]
                    .as_str()
                    .expect("machine event hook must be a string");
                let status = event["status"]
                    .as_str()
                    .expect("machine event status must be a string");
                assert!(
                    event["msg"].as_str().is_some_and(|msg| !msg.is_empty()),
                    "event is missing its diagnostic message: {event}"
                );
                if hook == "install" && status == "success" && event["msg"] == "recipe completed" {
                    recipe_completions += 1;
                }
                if ["acquire", "build", "install"].contains(&hook)
                    && ["running", "success"].contains(&status)
                {
                    phase_events.push((hook.to_owned(), status.to_owned()));
                }
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
    let mut expected: Vec<_> = ["acquire", "build", "install"]
        .into_iter()
        .flat_map(|hook| {
            ["running", "success"]
                .into_iter()
                .map(move |status| (hook.to_owned(), status.to_owned()))
        })
        .collect();
    // The CLI reports both the install phase and completion of the whole recipe.
    expected.push(("install".to_owned(), "success".to_owned()));
    assert_eq!(
        recipe_completions, 1,
        "expected one recipe completion event"
    );
    assert_eq!(
        phase_events, expected,
        "missing, duplicated, or reordered lifecycle events: {stderr}"
    );
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("machine event logs must not corrupt ctx JSON on stdout");
    assert_eq!(ctx["name"], "hooklog-json");
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
