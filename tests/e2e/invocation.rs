use super::*;

#[test]
fn cli_api2_context_supplies_fresh_paths_and_carries_state_and_named_outputs() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "context-tool",
        r##"
const RECIPE_API = 2;
const NEEDS = #{ outputs: #{ tool: "file", transcript: "file" } };
let ctx = #{name: "context-tool", product: join_path(RUNTIME.paths.output, "bin/context-tool"),
    runtime: #{paths: #{build: "stale source context"}}, state: #{runs: 0}, outputs: #{}};
let captured_build_dir = || RUNTIME.paths.build;
fn is_installed(ctx) {
    ctx.state.observed_operation = ctx.runtime.operation;
    if !is_file(ctx.product) { return needs_work(ctx, "tool missing"); }
    ctx.outputs.tool = ctx.product;
    ctx.outputs.transcript = join_path(ctx.runtime.paths.output, "tool-transcript");
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
        let output = run_recipe(&[action, "context-tool"], &recipes);
        assert_success(&output);
        let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(ctx["runtime"]["operation"], operation);
        assert_eq!(ctx["runtime"]["host"]["os"], "linux");
        assert_eq!(ctx["runtime"]["target"], ctx["runtime"]["host"]);
        assert_eq!(ctx["runtime"]["host"]["arch"], std::env::consts::ARCH);
        assert!(ctx["runtime"]["options"]["jobs"].as_u64().unwrap() > 0);
        assert_eq!(ctx["runtime"]["options"]["persist_state"], true);
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
            assert!(Path::new(ctx["outputs"]["tool"].as_str().unwrap()).is_file());
            assert_ne!(ctx["outputs"]["tool"], ctx["product"]);
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
const NEEDS = #{{}};
let ctx = #{{name: "{name}", {declaration}}};
fn is_installed(ctx) {{ needs_work(ctx, "exercise installation") }}
fn is_built(ctx) {{ ready(ctx) }}
fn install(ctx) {{ {install} }}
"##
            ),
        );
        let output = run_recipe(&["install", name, "--no-persist-state"], &recipes);
        assert!(!output.status.success(), "{name} unexpectedly succeeded");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(diagnostic), "{name}: {stderr}");
    }
    write_recipe(
        &recipes,
        "reserved-runtime",
        r##"
const RECIPE_API = 2;
const NEEDS = #{};
const RUNTIME = #{};
write_file("/out/unexpected-runtime-setup", "bad");
let ctx = #{name: "reserved-runtime"};
"##,
    );
    let output = run_recipe(
        &["install", "reserved-runtime", "--no-persist-state"],
        &recipes,
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("RUNTIME is an engine-owned"), "{stderr}");
}

#[test]
fn managed_current_contract_initializes_once_and_has_no_legacy_globals() {
    let (_dir, recipes) = create_test_env();
    std::fs::write(
        recipes.join("facts.rhai"),
        "fn value() { \"module fact\" }\n",
    )
    .unwrap();
    let source = r#"
const RECIPE_API = 2;
const NEEDS = #{ outputs: #{ product: "file" } };
import "facts.rhai" as facts;
append_file("/work/setup", "once\n");
let ctx = #{name: "current", state: #{}, outputs: #{}};
fn is_acquired(ctx) { ready(ctx) }
fn is_installed(ctx) {
    if is_file("/out/product") { ctx.outputs.product = "/out/product"; ready(ctx) }
    else { needs_work(ctx, "product missing") }
}
fn install(ctx) {
    if read_file("/work/setup") != "once\n" { throw "setup repeated"; }
    if is_def_var("ARCH") || is_def_var("BUILD_DIR") || is_def_var("NPROC") { throw "legacy globals remain"; }
    write_file("/out/product", facts::value());
    ctx.state.setup = read_file("/work/setup");
    ctx.outputs.product = "/out/product";
    ctx
}
"#;
    write_recipe(&recipes, "current", source);
    let output = run_recipe(&["install", "current"], &recipes);
    assert_success(&output);
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(ctx["state"]["setup"], "once\n");
    assert_eq!(
        std::fs::read_to_string(ctx["outputs"]["product"].as_str().unwrap()).unwrap(),
        "module fact"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("current.rhai")).unwrap(),
        source
    );
}
