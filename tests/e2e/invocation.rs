use super::*;

#[test]
fn cli_operations_share_globals_imports_and_keep_builtin_output_off_stdout() {
    let (dir, recipes) = create_test_env();
    std::fs::write(
        recipes.join("facts.rhai"),
        r##"
print("IMPORTED_PRINT");
fn value() { "imported package fact" }
"##,
    )
    .unwrap();
    std::fs::write(
        recipes.join("invocation-base.rhai"),
        r##"
import "facts.rhai" as facts;
let ctx = #{name: "invocation", arch: ARCH, jobs: NPROC, rpm: RPM_PATH,
            build: BUILD_DIR, tools: TOOLS_PREFIX, base: BASE_RECIPE_DIR};
print("BUILTIN_PRINT");
debug("BUILTIN_DEBUG");
fn inherited(ctx) { ctx.fact = facts::value(); ctx }
"##,
    )
    .unwrap();
    write_recipe(
        &recipes,
        "invocation",
        r##"//! extends: invocation-base.rhai
ctx.input = CUSTOM_INPUT;
fn is_acquired(ctx) { inherited(ctx) }
fn is_built(ctx) { inherited(ctx) }
fn is_installed(ctx) {
    if !is_file(join_path(BUILD_DIR, "payload")) { throw "installation missing"; }
    inherited(ctx)
}
fn install(ctx) {
    mkdir(BUILD_DIR);
    ctx = inherited(ctx);
    write_file(join_path(BUILD_DIR, "payload"), ctx.fact);
    ctx
}
fn cleanup(ctx, reason) {
    if reason == "manual" { write_file(join_path(BUILD_DIR, "manual-cleanup"), facts::value()); }
    inherited(ctx)
}
fn remove(ctx) { rm(join_path(BUILD_DIR, "payload")); inherited(ctx) }
"##,
    );
    for action in [
        "install",
        "isacquired",
        "isbuilt",
        "isinstalled",
        "cleanup",
        "remove",
    ] {
        let output = recipe_command(&recipes)
            .args([
                action,
                "invocation",
                "--no-persist-ctx",
                "--define",
                "CUSTOM_INPUT=caller input",
            ])
            .arg("--recipes-path")
            .arg(&recipes)
            .arg("--build-dir")
            .arg(dir.path().join("build"))
            .env("RPM_PATH", "/fixture/rpm-input")
            .output()
            .unwrap();
        assert_success(&output);
        let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(ctx["arch"], std::env::consts::ARCH);
        assert!(ctx["jobs"].as_u64().unwrap() > 0);
        assert_eq!(ctx["rpm"], "/fixture/rpm-input");
        assert_eq!(ctx["build"], dir.path().join("build").to_str().unwrap());
        assert_eq!(
            ctx["tools"],
            dir.path().join("build/.tools").to_str().unwrap()
        );
        assert_eq!(ctx["base"], recipes.to_str().unwrap());
        assert_eq!(ctx["input"], "caller input");
        assert_eq!(ctx["fact"], "imported package fact");
        let stderr = String::from_utf8_lossy(&output.stderr);
        for diagnostic in ["BUILTIN_PRINT", "BUILTIN_DEBUG", "IMPORTED_PRINT"] {
            assert!(
                stderr.contains(diagnostic),
                "missing {diagnostic}: {stderr}"
            );
        }
    }
    assert!(!dir.path().join("build/payload").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("build/manual-cleanup")).unwrap(),
        "imported package fact"
    );
}

#[test]
fn cli_dependency_invocation_receives_the_same_standard_globals() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "invocation-tool",
        r##"
let ctx = #{ name: "invocation-tool", arch: ARCH, jobs: NPROC, rpm: RPM_PATH };
print("DEPENDENCY_PRINT");
fn is_installed(ctx) {
    if !is_file(join_path(TOOLS_PREFIX, "bin/invocation-tool")) { throw "tool missing"; }
    ctx
}
fn is_acquired(ctx) { throw "prepare local tool"; }
fn acquire(ctx) {
    mkdir(BUILD_DIR);
    write_file(join_path(BUILD_DIR, "tool.sh"), "#!/bin/sh\nprintf '" + ctx.rpm + "\\n'\n");
    ctx
}
fn install(ctx) {
    let bin = join_path(TOOLS_PREFIX, "bin");
    mkdir(bin);
    copy_file(join_path(BUILD_DIR, "tool.sh"), join_path(bin, "invocation-tool"));
    chmod(join_path(bin, "invocation-tool"), 0o755);
    ctx
}
"##,
    );
    write_recipe(
        &recipes,
        "invocation-parent",
        r##"
let deps = ["invocation-tool"];
let ctx = #{ name: "invocation-parent" };
fn is_installed(ctx) { throw "consume tool"; }
fn is_built(ctx) { ctx }
fn install(ctx) {
    mkdir(BUILD_DIR);
    write_file(join_path(BUILD_DIR, "tool-result"), exec_output("invocation-tool", []));
    ctx
}
"##,
    );
    let output = recipe_command(&recipes)
        .args(["install", "invocation-parent", "--no-persist-ctx"])
        .arg("--recipes-path")
        .arg(&recipes)
        .arg("--build-dir")
        .arg(dir.path().join("build"))
        .env("RPM_PATH", "dependency invocation value")
        .output()
        .unwrap();
    assert_success(&output);
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("build/tool-result")).unwrap(),
        "dependency invocation value\n"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("DEPENDENCY_PRINT"));
}

#[test]
fn cli_api2_context_supplies_fresh_paths_and_carries_state_and_named_outputs() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "context-tool",
        r##"
const RECIPE_API = 2;
let ctx = #{name: "context-tool", product: join_path(RUNTIME.paths.output, "bin/context-tool"),
    runtime: #{paths: #{build: "stale source context"}}, state: #{runs: 0}, outputs: #{}};
let captured_build_dir = || ctx.runtime.paths.build;
fn is_installed(ctx) {
    ctx.state.observed_operation = ctx.runtime.operation;
    if !is_file(ctx.product) { return needs_work(ctx, "tool missing"); }
    ctx.outputs.tool = ctx.product;
    ready(ctx)
}
fn is_built(ctx) { ready(ctx) }
fn is_acquired(ctx) { ready(ctx) }
fn install(ctx) {
    if call(captured_build_dir) != ctx.runtime.paths.build { throw "captured ctx is stale"; }
    mkdir(dirname(ctx.product));
    write_file(ctx.product, "#!/bin/sh\nprintf 'explicit context works\\n'\n");
    chmod(ctx.product, 0o755);
    let transcript = join_path(ctx.runtime.paths.output, "tool-transcript");
    ctx.state.tool_result = exec_output(ctx.product, []);
    write_file(transcript, ctx.state.tool_result);
    ctx.state.runs += 1;
    ctx.outputs.tool = ctx.product;
    ctx.outputs.transcript = transcript;
    ctx
}
fn remove(ctx) { rm(ctx.product); ctx.state.removed = !is_file(ctx.product); ctx }
"##,
    );
    for (action, operation) in [
        ("install", "install"),
        ("isacquired", "is_acquired"),
        ("isbuilt", "is_built"),
        ("isinstalled", "is_installed"),
        ("cleanup", "cleanup"),
        ("remove", "remove"),
    ] {
        let output = run_recipe(&[action, "context-tool", "--no-persist-ctx"], &recipes);
        assert_success(&output);
        let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(ctx["runtime"]["operation"], operation);
        assert_eq!(ctx["runtime"]["host"]["os"], "linux");
        assert_eq!(ctx["runtime"]["target"], ctx["runtime"]["host"]);
        assert_eq!(ctx["runtime"]["host"]["arch"], std::env::consts::ARCH);
        assert!(ctx["runtime"]["options"]["jobs"].as_u64().unwrap() > 0);
        assert_eq!(ctx["runtime"]["options"]["persist_state"], false);
        assert_eq!(ctx["runtime"]["paths"]["inputs"], "/inputs");
        assert_eq!(ctx["runtime"]["paths"]["output"], "/out");
        assert_ne!(ctx["runtime"]["paths"]["build"], "stale source context");
        assert!(ctx["state"].is_object());
        assert!(ctx["outputs"].is_object());
        if action == "remove" {
            assert_eq!(ctx["state"]["removed"], true);
        }
        if action == "install" {
            assert_eq!(ctx["state"]["runs"], 1);
            assert_eq!(ctx["state"]["observed_operation"], "install");
            assert_eq!(ctx["outputs"]["tool"], ctx["product"]);
            assert_eq!(ctx["state"]["tool_result"], "explicit context works\n");
        }
    }
}

#[test]
fn cli_api2_rejects_invalid_context_namespaces_and_changed_runtime() {
    let (_dir, recipes) = create_test_env();
    for (name, declaration, install, diagnostic) in [
        (
            "bad-state",
            "state: \"encoded legacy map\",",
            "ctx",
            "ctx.state",
        ),
        ("bad-outputs", "outputs: false,", "ctx", "ctx.outputs"),
        (
            "changed-runtime",
            "",
            "#{runtime: #{}, state: ctx.state, outputs: ctx.outputs}",
            "ctx.runtime",
        ),
    ] {
        write_recipe(
            &recipes,
            name,
            &format!(
                r##"
const RECIPE_API = 2;
let ctx = #{{name: "{name}", {declaration}}};
fn is_installed(ctx) {{ needs_work(ctx, "exercise installation") }}
fn is_built(ctx) {{ ready(ctx) }}
fn install(ctx) {{ {install} }}
"##
            ),
        );
        let output = run_recipe(&["install", name, "--no-persist-ctx"], &recipes);
        assert!(!output.status.success(), "{name} unexpectedly succeeded");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(diagnostic), "{name}: {stderr}");
    }
    write_recipe(
        &recipes,
        "reserved-runtime",
        r##"
const RECIPE_API = 2;
const RUNTIME = #{};
write_file(join_path(RECIPE_DIR, "unexpected-runtime-setup"), "bad");
let ctx = #{name: "reserved-runtime"};
"##,
    );
    let output = run_recipe(
        &["install", "reserved-runtime", "--no-persist-ctx"],
        &recipes,
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("RUNTIME is an engine-owned"), "{stderr}");
    assert!(!recipes.join("unexpected-runtime-setup").exists());
}
