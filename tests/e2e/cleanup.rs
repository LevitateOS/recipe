use super::*;

#[test]
fn cli_optional_cleanup_leaves_installed_products_intact() {
    let (dir, recipes) = create_test_env();
    for (name, api, not_ready, ready) in [
        ("cleanup-legacy", "", "throw \"not installed\"", "ctx"),
        (
            "cleanup-api2",
            "const RECIPE_API = 2;",
            "return needs_work(ctx, \"not installed\")",
            "ready(ctx)",
        ),
    ] {
        write_recipe(
            &recipes,
            name,
            &format!(
                r##"
{api}
let ctx = #{{name: "{name}", binary: join_path(BUILD_DIR, "{name}"),
             result: join_path(BUILD_DIR, "{name}.result")}};
fn is_installed(ctx) {{
    if !is_file(ctx.binary) {{ {not_ready}; }}
    {ready}
}}
fn is_built(ctx) {{ {ready} }}
fn install(ctx) {{
    mkdir(BUILD_DIR);
    write_file(ctx.binary, "#!/bin/sh\nprintf 'cleanup is optional\\n'\n");
    chmod(ctx.binary, 0o755);
    write_file(ctx.result, exec_output(ctx.binary, []));
    ctx
}}
"##
            ),
        );
        let before = run_recipe(&["cleanup", name, "--no-persist-state"], &recipes);
        assert_success(&before);
        let before_ctx: serde_json::Value = serde_json::from_slice(&before.stdout).unwrap();
        assert_eq!(before_ctx["name"], name);
        assert_success(&run_recipe(
            &["install", name, "--no-persist-state"],
            &recipes,
        ));
        let binary = dir.path().join("build").join(name);
        let binary_contents = std::fs::read(&binary).unwrap();
        let cleanup = run_recipe(&["cleanup", name, "--no-persist-state"], &recipes);
        assert_success(&cleanup);
        serde_json::from_slice::<serde_json::Value>(&cleanup.stdout).unwrap();
        assert_eq!(std::fs::read(&binary).unwrap(), binary_contents);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("build").join(format!("{name}.result")))
                .unwrap(),
            "cleanup is optional\n"
        );
        assert!(String::from_utf8_lossy(&cleanup.stderr).contains("no cleanup hook"));
    }
}

#[test]
fn cli_cleanup_preserves_reasons_and_returned_context_through_real_phases() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "cleanup-reasons",
        r##"
const RECIPE_API = 2;
let ctx = #{name: "cleanup-reasons", cleanups: 0,
    source: join_path(BUILD_DIR, "source.sh"), built: join_path(BUILD_DIR, "built.sh"),
    binary: join_path(BUILD_DIR, "bin/tool")};
fn is_installed(ctx) {
    if !is_file(ctx.binary) { return needs_work(ctx, "binary missing"); }
    ready(ctx)
}
fn is_built(ctx) {
    if !is_file(ctx.built) { return needs_work(ctx, "build missing"); }
    ready(ctx)
}
fn is_acquired(ctx) {
    if !is_file(ctx.source) { return needs_work(ctx, "source missing"); }
    ready(ctx)
}
fn acquire(ctx) { mkdir(BUILD_DIR); write_file(ctx.source, "#!/bin/sh\nprintf 'real tool\\n'\n"); ctx }
fn build(ctx) { exec_checked("sh", ["-n", ctx.source]); copy_file(ctx.source, ctx.built); ctx }
fn install(ctx) {
    mkdir(dirname(ctx.binary)); copy_file(ctx.built, ctx.binary); chmod(ctx.binary, 0o755);
    write_file(join_path(BUILD_DIR, "tool-output"), exec_output(ctx.binary, [])); ctx
}
fn cleanup(ctx, reason) {
    append_file(join_path(BUILD_DIR, "cleanup-reasons"), reason + "\n");
    ctx.cleanups += 1;
    ctx.last_cleanup = reason;
    ctx
}
"##,
    );
    let installed = run_recipe(
        &["install", "cleanup-reasons", "--no-persist-state"],
        &recipes,
    );
    assert_success(&installed);
    let ctx: serde_json::Value = serde_json::from_slice(&installed.stdout).unwrap();
    assert_eq!(ctx["cleanups"], 3);
    assert_eq!(ctx["last_cleanup"], "auto.install.success");
    let manual = run_recipe(
        &[
            "cleanup",
            "cleanup-reasons",
            "--reason",
            "manual.trim",
            "--no-persist-state",
        ],
        &recipes,
    );
    assert_success(&manual);
    let ctx: serde_json::Value = serde_json::from_slice(&manual.stdout).unwrap();
    assert_eq!(ctx["last_cleanup"], "manual.trim");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("build/cleanup-reasons")).unwrap(),
        "auto.acquire.success\nauto.build.success\nauto.install.success\nmanual.trim\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("build/tool-output")).unwrap(),
        "real tool\n"
    );
}

#[test]
fn cli_cleanup_errors_fail_success_and_preserve_primary_phase_errors() {
    let (dir, recipes) = create_test_env();
    for (name, phase_body, primary, cleanup_body) in [
        (
            "success-cleanup-error",
            "write_file(ctx.partial, \"payload\"); ctx",
            "CLEANUP_FAILED",
            "throw \"CLEANUP_FAILED\";",
        ),
        (
            "phase-and-cleanup-error",
            "write_file(ctx.partial, \"partial\"); throw \"PHASE_FAILED\";",
            "PHASE_FAILED",
            "rm(ctx.partial); throw \"SECONDARY_CLEANUP_FAILED\";",
        ),
    ] {
        write_recipe(
            &recipes,
            name,
            &format!(
                r##"
let ctx = #{{name: "{name}", partial: join_path(BUILD_DIR, "{name}")}};
fn is_installed(ctx) {{ throw "not installed"; }}
fn is_built(ctx) {{ ctx }}
fn install(ctx) {{ mkdir(BUILD_DIR); {phase_body} }}
fn cleanup(ctx, reason) {{ {cleanup_body} ctx }}
"##
            ),
        );
        let output = run_recipe(&["install", name, "--no-persist-state"], &recipes);
        assert!(!output.status.success(), "{name} unexpectedly succeeded");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let final_error = stderr.rsplit("Error:").next().unwrap();
        assert!(final_error.contains(primary), "{stderr}");
        if name == "phase-and-cleanup-error" {
            assert!(stderr.contains("SECONDARY_CLEANUP_FAILED"), "{stderr}");
            assert!(
                !final_error.contains("SECONDARY_CLEANUP_FAILED"),
                "{stderr}"
            );
            assert!(!dir.path().join("build").join(name).exists());
        }
    }
}

#[test]
fn cli_malformed_cleanup_fails_before_recipe_setup_and_installation() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "cleanup-arity",
        r##"
write_file(join_path(RECIPE_DIR, "unexpected-setup"), "bad");
let ctx = #{name: "cleanup-arity"};
fn is_installed(ctx) { throw "not installed"; }
fn is_built(ctx) { ctx }
fn install(ctx) { write_file(join_path(RECIPE_DIR, "unexpected-install"), "bad"); ctx }
fn cleanup(ctx) { ctx }
"##,
    );
    for action in ["install", "cleanup"] {
        let output = run_recipe(&[action, "cleanup-arity", "--no-persist-state"], &recipes);
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("cleanup(ctx, reason)"), "{stderr}");
        assert!(!recipes.join("unexpected-setup").exists());
        assert!(!recipes.join("unexpected-install").exists());
    }
}

#[test]
fn cli_dependency_cleanup_failure_stops_parent_installation() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "cleanup-tool",
        r##"
let ctx = #{name: "cleanup-tool"};
fn is_installed(ctx) { throw "tool missing"; }
fn is_acquired(ctx) { ctx }
fn install(ctx) {
    mkdir(TOOLS_PREFIX); write_file(join_path(TOOLS_PREFIX, "prepared-tool"), "prepared"); ctx
}
fn cleanup(ctx, reason) { throw "TOOL_CLEANUP_FAILED"; ctx }
"##,
    );
    write_recipe(
        &recipes,
        "cleanup-parent",
        r##"
let deps = ["cleanup-tool"];
let ctx = #{name: "cleanup-parent"};
fn is_installed(ctx) { throw "not installed"; }
fn is_built(ctx) { ctx }
fn install(ctx) { write_file(join_path(RECIPE_DIR, "unexpected-parent-install"), "bad"); ctx }
"##,
    );
    let output = run_recipe(
        &["install", "cleanup-parent", "--no-persist-state"],
        &recipes,
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("TOOL_CLEANUP_FAILED"), "{stderr}");
    assert!(!recipes.join("unexpected-parent-install").exists());
}
