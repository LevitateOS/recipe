//! Actual step failures, recovery and human/machine diagnostics.
use super::*;

fn events_of(stderr: &str) -> Vec<serde_json::Value> {
    stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect()
}

#[test]
fn managed_step_failures_run_cleanup_without_publishing_or_continuing() {
    let (dir, recipes) = create_test_env();
    // A recipe with `build` fails in its build step; one without it does its
    // work, and fails its `verify`, in the install step.
    for (step, functions) in [
        (
            "build",
            "fn build(src, out) { throw \"PRIMARY_STEP_FAILURE\"; }\n",
        ),
        (
            "install",
            "fn verify(out) { throw \"PRIMARY_STEP_FAILURE\"; }\n",
        ),
    ] {
        write_recipe(
            &recipes,
            step,
            &format!(
                r#"const RECIPE_API = 1;
const NEEDS = #{{ outputs: #{{ product: "directory" }} }};
{functions}fn cleanup(reason) {{ log("CLEANED " + reason); }}
"#
            ),
        );
        let output = run_recipe(&["--machine-events", "install", step], &recipes);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("PRIMARY_STEP_FAILURE"), "{error}");
        assert!(
            error.contains(&format!("CLEANED auto.{step}.failure")),
            "{error}"
        );
        let events = events_of(&error);
        assert!(
            events.iter().any(|event| event["event"] == "step"
                && event["step"] == step
                && event["status"] == "failed"
                && event["msg"]
                    .as_str()
                    .is_some_and(|msg| msg.contains("PRIMARY_STEP_FAILURE"))),
            "no structured failure: {error}"
        );
        assert!(
            !events.iter().any(|event| event.get("hook").is_some()),
            "events carry `step`, not `hook`: {error}"
        );
    }
    assert!(!dir.path().join("state/recipe/exports").exists());
}

#[test]
fn managed_failed_build_resumes_from_its_checkpoint() {
    let (_dir, recipes) = create_test_env();
    let source = r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
fn build(src, out) {
    let source = join_path(WORK, "source");
    if !reached("sourced") {
        write_file(source, "payload");
        append_file(join_path(WORK, "sourced-count"), "x");
        save("sourced", true);
        checkpoint("sourced");
    }
    if saved("blocked") == () {
        save("blocked", true);
        checkpoint("blocked");
        throw "BUILD_BLOCKED_FOR_RETRY";
    }
    write_file(out, read_file(source) + ":" + read_file(join_path(WORK, "sourced-count")));
}
fn cleanup(reason) { log("CLEANED " + reason); }
"#;
    write_recipe(&recipes, "retry", source);
    let first = run_recipe(&["install", "retry"], &recipes);
    assert!(!first.status.success());
    let error = String::from_utf8_lossy(&first.stderr);
    assert!(error.contains("BUILD_BLOCKED_FOR_RETRY"), "{error}");
    assert!(error.contains("CLEANED auto.build.failure"), "{error}");
    let second = run_recipe(&["install", "retry"], &recipes);
    assert_success(&second);
    let result: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(result["outputs"]["product"].as_str().unwrap()).unwrap(),
        "payload:x"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("retry.rhai")).unwrap(),
        source
    );
}

#[test]
fn managed_machine_events_report_actual_step_order_without_corrupting_json() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "events",
        r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
fn build(src, out) { write_file(out, "event payload"); }
"#,
    );
    let output = run_recipe(&["--machine-events", "install", "events"], &recipes);
    assert_success(&output);
    let error = String::from_utf8_lossy(&output.stderr);
    let events = events_of(&error);
    let plan = events
        .iter()
        .find(|event| event["event"] == "plan")
        .unwrap_or_else(|| panic!("no plan event: {error}"));
    assert_eq!(plan["recipe"], "events");
    assert_eq!(
        plan["steps"],
        serde_json::json!(["build", "install", "cleanup"])
    );
    let steps = events
        .iter()
        .filter(|event| event["event"] == "step")
        .collect::<Vec<_>>();
    let running = steps
        .iter()
        .filter(|event| event["status"] == "running")
        .filter_map(|event| event["step"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(running, vec!["build"], "{error}");
    for (step, status) in [("build", "done"), ("install", "done"), ("cleanup", "done")] {
        assert!(
            steps
                .iter()
                .any(|event| event["step"] == step && event["status"] == status),
            "no {step} {status} event: {error}"
        );
    }
    assert!(
        steps.iter().all(
            |event| ["running", "done", "already", "none", "skipped", "failed"]
                .contains(&event["status"].as_str().unwrap())
        ),
        "{error}"
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(result["outputs"]["product"].as_str().unwrap()).unwrap(),
        "event payload"
    );
}

#[test]
fn cli_resource_exhaustion_runs_failure_cleanup() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "budget",
        r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
fn build(src, out) { while true {} }
fn cleanup(reason) { log("BUDGET_CLEANUP " + reason); }
"#,
    );
    let output = run_recipe(&["install", "budget"], &recipes);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("operations"), "{error}");
    assert!(
        error.contains("BUDGET_CLEANUP auto.build.failure"),
        "{error}"
    );
}
