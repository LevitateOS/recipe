//! Actual phase failures, recovery and human/machine diagnostics.
use super::*;
#[test]
fn managed_phase_failures_run_cleanup_without_publishing_or_continuing() {
    let (dir, recipes) = create_test_env();
    for phase in ["acquire", "build", "install"] {
        let built = if phase == "install" {
            "ready(ctx)"
        } else {
            "needs_work(ctx,\"build\")"
        };
        let acquired = if phase == "acquire" {
            "needs_work(ctx,\"acquire\")"
        } else {
            "ready(ctx)"
        };
        write_recipe(
            &recipes,
            phase,
            &format!(
                r#"const RECIPE_API = 2;
const NEEDS = #{{}};
let ctx=#{{name:{phase:?}}};
fn is_installed(ctx) {{ needs_work(ctx,"install") }}
fn is_built(ctx) {{ {built} }}
fn is_acquired(ctx) {{ {acquired} }}
fn {phase}(ctx) {{ throw "PRIMARY_PHASE_FAILURE"; ctx }}
fn cleanup(ctx,reason) {{ log("CLEANED " + reason); ctx }}
"#
            ),
        );
        let output = run_recipe(&["--machine-events", "install", phase], &recipes);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("PRIMARY_PHASE_FAILURE"), "{error}");
        assert!(
            error.contains(&format!("CLEANED auto.{phase}.failure")),
            "{error}"
        );
        let events = error
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .collect::<Vec<_>>();
        assert!(
            events.iter().any(|event| event["event"] == "recipe-hook"
                && event["status"] == "failed"
                && event["msg"]
                    .as_str()
                    .is_some_and(|msg| msg.contains("PRIMARY_PHASE_FAILURE"))),
            "no structured failure: {error}"
        );
    }
    assert!(!dir.path().join("state/recipe/exports").exists());
}
#[test]
fn managed_failed_build_resumes_acquisition_and_failure_cleanup_state() {
    let (_dir, recipes) = create_test_env();
    let source = r#"const RECIPE_API = 2;
const NEEDS = #{ outputs: #{ product: "file" } };
let ctx=#{name:"retry",state:#{acquisitions:0}};
fn is_acquired(ctx) { if is_file("/work/build/source") { return ready(ctx); } needs_work(ctx,"acquire") }
fn acquire(ctx) { write_file("/work/build/source","payload"); ctx.state.acquisitions+=1; ctx }
fn is_built(ctx) { if is_file("/work/build/built") { return ready(ctx); } needs_work(ctx,"build") }
fn build(ctx) {
    if !is_file("/work/build/allow") { throw "BUILD_BLOCKED_FOR_RETRY"; }
    copy_file("/work/build/source","/work/build/built"); ctx
}
fn is_installed(ctx) { if !is_file("/out/product") { return needs_work(ctx,"install"); } ctx.outputs.product="/out/product"; ready(ctx) }
fn install(ctx) { copy_file("/work/build/built","/out/product"); ctx }
fn cleanup(ctx,reason) {
    if reason=="auto.build.failure" { write_file("/work/build/allow","failure cleaned"); ctx.state.recovery=reason; }
    ctx
}
"#;
    write_recipe(&recipes, "retry", source);
    let first = run_recipe(&["install", "retry"], &recipes);
    assert!(!first.status.success());
    assert!(String::from_utf8_lossy(&first.stderr).contains("BUILD_BLOCKED_FOR_RETRY"));
    let second = run_recipe(&["install", "retry"], &recipes);
    assert_success(&second);
    let ctx: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(ctx["state"]["acquisitions"], 1);
    assert_eq!(ctx["state"]["recovery"], "auto.build.failure");
    assert_eq!(
        std::fs::read_to_string(ctx["outputs"]["product"].as_str().unwrap()).unwrap(),
        "payload"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("retry.rhai")).unwrap(),
        source
    );
}
#[test]
fn managed_machine_events_report_actual_phase_order_without_corrupting_json() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "events",
        r#"const RECIPE_API = 2;
const NEEDS = #{ outputs: #{ product: "file" } };
let ctx=#{name:"events"};
fn is_acquired(ctx) { if is_file("/work/build/source") { return ready(ctx); } needs_work(ctx,"acquire") }
fn acquire(ctx) { write_file("/work/build/source","event payload"); ctx }
fn is_built(ctx) { if is_file("/work/build/built") { return ready(ctx); } needs_work(ctx,"build") }
fn build(ctx) { copy_file("/work/build/source","/work/build/built"); ctx }
fn is_installed(ctx) { if !is_file("/out/product") { return needs_work(ctx,"install"); } ctx.outputs.product="/out/product"; ready(ctx) }
fn install(ctx) { copy_file("/work/build/built","/out/product"); ctx }
"#,
    );
    let output = run_recipe(&["--machine-events", "install", "events"], &recipes);
    assert_success(&output);
    let error = String::from_utf8_lossy(&output.stderr);
    let events = error
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["event"] == "recipe-hook")
        .collect::<Vec<_>>();
    let phases = events
        .iter()
        .filter(|event| event["status"] == "running")
        .filter_map(|event| event["hook"].as_str())
        .filter(|hook| ["acquire", "build", "install"].contains(hook))
        .collect::<Vec<_>>();
    assert_eq!(phases, vec!["acquire", "build", "install"], "{error}");
    assert!(
        events.iter().any(|event| event["hook"] == "install"
            && event["status"] == "success"
            && event["msg"] == "recipe completed"),
        "{error}"
    );
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(ctx["outputs"]["product"].as_str().unwrap()).unwrap(),
        "event payload"
    );
}
