//! Current readiness semantics through the actual managed CLI.
use super::*;
#[test]
fn cli_current_readiness_errors_do_not_authorize_installation() {
    let (_dir, recipes) = create_test_env();
    for (name, check, diagnostic) in [
        ("thrown", "throw \"CHECK_ERROR\";", "CHECK_ERROR"),
        ("raw-map", "ctx", "ready(ctx)"),
        (
            "empty-reason",
            "#{status:\"needs_work\",ctx:ctx,reason:\"\"}",
            "nonempty reason",
        ),
        (
            "missing-function",
            "missing_readiness_function(ctx)",
            "missing_readiness_function",
        ),
        (
            "native-io",
            "read_file(\"/missing-readiness-input\"); ready(ctx)",
            "No such file",
        ),
        (
            "native-process",
            "exec_checked(\"sh\",[\"-c\",\"exit 23\"]); ready(ctx)",
            "23",
        ),
        ("wrong-type", "true", "ready(ctx)"),
    ] {
        let source = format!(
            r#"const RECIPE_API = 2;
let ctx=#{{name:{name:?}}};
fn is_installed(ctx) {{ {check} }}
fn is_built(ctx) {{ ready(ctx) }}
fn install(ctx) {{ log("UNAUTHORIZED_INSTALL"); ctx }}
"#
        );
        write_recipe(&recipes, name, &source);
        let output = run_recipe(&["install", name], &recipes);
        assert!(!output.status.success(), "{name}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(diagnostic), "{name}: {error}");
        assert!(!error.contains("UNAUTHORIZED_INSTALL"), "{error}");
        assert_eq!(
            std::fs::read_to_string(recipes.join(format!("{name}.rhai"))).unwrap(),
            source
        );
    }
}
#[test]
fn cli_unsupported_api_precedes_setup_and_incomplete_producers_fail() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "unsupported",
        r#"const RECIPE_API = 99;
print("UNEXPECTED_UNSUPPORTED_SETUP"); let ctx=#{name:"unsupported"};
fn is_installed(ctx) { ready(ctx) }
"#,
    );
    let output = run_recipe(&["install", "unsupported"], &recipes);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("RECIPE_API"), "{error}");
    assert!(!error.contains("UNEXPECTED_UNSUPPORTED_SETUP"));
    write_recipe(
        &recipes,
        "incomplete",
        r#"const RECIPE_API = 2;
let ctx=#{name:"incomplete"};
fn is_installed(ctx) { needs_work(ctx,"complete product is missing") }
fn is_built(ctx) { ready(ctx) }
fn install(ctx) { write_file("/out/partial","partial"); ctx.outputs.product="/out/partial"; ctx }
fn cleanup(ctx,reason) { log("FAILURE_CLEANUP " + reason); ctx }
"#,
    );
    let output = run_recipe(&["install", "incomplete"], &recipes);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("complete product is missing"), "{error}");
    assert!(
        error.contains("FAILURE_CLEANUP auto.install.failure"),
        "{error}"
    );
    assert!(!dir.path().join(".artifacts/ginux-export").exists());
}
#[test]
fn cli_resource_exhaustion_runs_failure_cleanup_and_bad_cleanup_cannot_pass() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "budget",
        r#"const RECIPE_API = 2;
let ctx=#{name:"budget"};
fn is_installed(ctx) { needs_work(ctx,"not installed") }
fn is_built(ctx) { needs_work(ctx,"not built") }
fn is_acquired(ctx) { ready(ctx) }
fn build(ctx) { while true {} ctx }
fn install(ctx) { log("UNEXPECTED_INSTALL"); ctx }
fn cleanup(ctx,reason) { log("BUDGET_CLEANUP " + reason); ctx }
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
    assert!(!error.contains("UNEXPECTED_INSTALL"));
    write_recipe(
        &recipes,
        "bad-cleanup",
        r#"const RECIPE_API = 2;
let ctx=#{name:"bad-cleanup"};
fn is_installed(ctx) { if !is_file("/out/product") { return needs_work(ctx,"installed product missing"); } ready(ctx) }
fn is_built(ctx) { ready(ctx) }
fn install(ctx) { write_file("/out/product","installed"); ctx }
fn cleanup(ctx,reason) { if reason=="auto.install.success" { rm("/out/product"); } log("PRODUCT_CLEANUP " + reason); ctx }
"#,
    );
    let output = run_recipe(&["install", "bad-cleanup"], &recipes);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("installed product missing"), "{error}");
    assert!(
        error.contains("PRODUCT_CLEANUP auto.install.failure"),
        "{error}"
    );
}
