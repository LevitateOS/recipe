//! Full CLI dependency graphs. A prepared Ginux environment must provide `cc`.
use super::*;

#[test]
fn cli_transitive_dependencies_compile_tools_through_the_shared_lifecycle() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "dep-helpers",
        r#"fn expected() { "real leaf\n" }"#,
    );
    write_recipe(
        &recipes,
        "leaf",
        r##"
const RECIPE_API = 2;
let ctx = #{name: "leaf", source: "", built: join_path(RUNTIME.paths.build, "leaf"),
            installed: join_path(RUNTIME.paths.tools, "bin/leaf")};
fn is_installed(ctx) {
    if !is_file(ctx.installed) { return needs_work(ctx, "leaf tool absent"); }
    ready(ctx)
}
fn is_built(ctx) {
    ctx.discovered = "leaf checks carried context";
    if !is_file(ctx.built) { return needs_work(ctx, "leaf not compiled"); }
    ready(ctx)
}
fn is_acquired(ctx) {
    ctx.source = join_path(ctx.runtime.paths.build, "leaf.c");
    if !is_file(ctx.source) { return needs_work(ctx, "leaf source absent"); }
    ready(ctx)
}
fn acquire(ctx) {
    write_file(ctx.source, "#include <stdio.h>\nint main(void) { puts(\"real leaf\"); return 0; }\n"); ctx
}
fn build(ctx) {
    if ctx.discovered != "leaf checks carried context" { throw "lost leaf check context"; }
    exec_checked("cc", [ctx.source, "-o", ctx.built]); ctx
}
fn install(ctx) { mkdir(dirname(ctx.installed)); copy_file(ctx.built, ctx.installed); chmod(ctx.installed, 0o755); ctx }
"##,
    );
    write_recipe(
        &recipes,
        "middle-base",
        r##"
const RECIPE_API = 2;
import "dep-helpers.rhai" as helper;
let ctx = #{name: "middle", source: join_path(RUNTIME.paths.build, "middle.c"),
            built: join_path(RUNTIME.paths.build, "middle"), installed: join_path(RUNTIME.paths.tools, "bin/middle")};
fn is_installed(ctx) {
    if !is_file(ctx.installed) { return needs_work(ctx, "middle absent"); }
    ready(ctx)
}
fn is_built(ctx) {
    ctx.discovered = helper::expected();
    if !is_file(ctx.built) { return needs_work(ctx, "middle not compiled"); }
    ready(ctx)
}
fn is_acquired(ctx) {
    if !is_file(ctx.source) { return needs_work(ctx, "middle source absent"); }
    ready(ctx)
}
fn acquire(ctx) {
    write_file(ctx.source, "#include <stdio.h>\nint main(void) { puts(\"middle built with leaf\"); return 0; }\n"); ctx
}
fn build(ctx) {
    if exec_output("leaf", []) != ctx.discovered { throw "transitive leaf unavailable"; }
    exec_checked("cc", [ctx.source, "-o", ctx.built]); ctx
}
fn install(ctx) { mkdir(dirname(ctx.installed)); copy_file(ctx.built, ctx.installed); chmod(ctx.installed, 0o755); ctx }
fn cleanup(ctx, reason) { append_file(join_path(ctx.runtime.paths.build, "cleanup-reasons"), reason + "\n"); log("DEP_CLEANUP " + reason); ctx }
"##,
    );
    write_recipe(
        &recipes,
        "middle",
        "//! extends: middle-base.rhai\nlet build_deps = [\"leaf\"];\n",
    );
    write_recipe(
        &recipes,
        "parent",
        r##"
const RECIPE_API = 2;
let deps = ["middle", "leaf"];
let ctx = #{name: "parent"};
fn is_installed(ctx) {
    if !ctx.state.contains("done") { return needs_work(ctx, "parent not prepared"); }
    ready(ctx)
}
fn is_built(ctx) { ready(ctx) }
fn install(ctx) {
    ctx.state.leaf = exec_output("leaf", []);
    ctx.state.middle = exec_output("middle", []);
    if ctx.state.leaf != "real leaf\n" || ctx.state.middle != "middle built with leaf\n" { throw "wrong dependency tools"; }
    ctx.state.done = true; ctx
}
"##,
    );
    let output = run_recipe(&["install", "parent", "--no-persist-ctx"], &recipes);
    assert_success(&output);
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(ctx["state"]["leaf"], "real leaf\n");
    assert_eq!(ctx["state"]["middle"], "middle built with leaf\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("leaf already installed"), "{stderr}");
    assert!(stderr.contains("auto.build.success"), "{stderr}");
}

fn write_graph_node(recipes: &Path, name: &str, declaration: &str) {
    write_recipe(
        recipes,
        name,
        &format!(
            r##"
const RECIPE_API = 2;
{declaration}
let ctx = #{{name: "{name}"}};
fn is_installed(ctx) {{ needs_work(ctx, "node needs installation") }}
fn is_built(ctx) {{ ready(ctx) }}
fn install(ctx) {{ log("UNEXPECTED_GRAPH_INSTALL"); ctx }}
"##
        ),
    );
}

#[test]
fn cli_dependency_cycles_missing_recipes_and_bad_declarations_fail_usefully() {
    for (declaration, diagnostic) in [
        ("let deps = [\"absent\"];", "absent.rhai' not found"),
        ("let deps = [\"a\"];", "Circular recipe dependency detected"),
        ("let deps = [42];", "deps[0] must be a recipe name string"),
        ("let build_deps = false;", "build_deps must be an array"),
        ("let deps = [\"../escape\"];", "without parent traversal"),
    ] {
        let (_dir, recipes) = create_test_env();
        write_graph_node(&recipes, "parent", declaration);
        write_graph_node(&recipes, "a", "let deps = [\"b\"];");
        write_graph_node(&recipes, "b", "let deps = [\"a\"];");
        let output = run_recipe(&["install", "parent", "--no-persist-ctx"], &recipes);
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(diagnostic), "{diagnostic}: {stderr}");
        assert!(!stderr.contains("UNEXPECTED_GRAPH_INSTALL"), "{stderr}");
    }
}

#[test]
fn cli_dependency_check_errors_stop_before_acquisition_or_parent_installation() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "broken",
        r##"
const RECIPE_API = 2;
let ctx = #{name: "broken"};
fn is_installed(ctx) { needs_work(ctx, "not prepared") }
fn is_built(ctx) { missing_dependency_check_helper(ctx) }
fn acquire(ctx) { log("UNEXPECTED_DEPENDENCY_ACQUIRE"); ctx }
fn install(ctx) { log("UNEXPECTED_GRAPH_INSTALL"); ctx }
"##,
    );
    write_graph_node(&recipes, "parent", "let deps = [\"broken\"];");
    let output = run_recipe(&["install", "parent", "--no-persist-ctx"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("missing_dependency_check_helper"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("UNEXPECTED_DEPENDENCY_ACQUIRE"),
        "{stderr}"
    );
    assert!(!stderr.contains("UNEXPECTED_GRAPH_INSTALL"), "{stderr}");
}

#[test]
fn cli_v1_persistent_dependencies_fail_before_dependency_setup_effects() {
    let (_dir, recipes) = create_test_env();
    write_graph_node(&recipes, "parent", "let deps = [\"tool\"];");
    write_recipe(
        &recipes,
        "tool",
        r##"
log("UNEXPECTED_DEPENDENCY_SETUP");
let ctx = #{name: "tool"};
fn install(ctx) { ctx }
"##,
    );
    let output = run_recipe(&["install", "parent"], &recipes);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("transport v1 cannot checkpoint dependency state"),
        "{stderr}"
    );
    assert!(!stderr.contains("UNEXPECTED_DEPENDENCY_SETUP"), "{stderr}");
}
