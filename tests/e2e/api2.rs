use super::*;

#[test]
fn cli_api2_initializes_once_and_rechecks_real_acquire_build_install_outputs() {
    let (dir, recipes) = create_test_env();
    std::fs::write(recipes.join("fact"), "current package fact").unwrap();
    std::fs::write(
        recipes.join("facts.rhai"),
        r##"
append_file(join_path(RECIPE_DIR, "module-setup"), "module\n");
fn fact() { "imported fact" }
"##,
    )
    .unwrap();
    std::fs::write(
        recipes.join("base.rhai"),
        r##"
const RECIPE_API = 2;
append_file(join_path(RECIPE_DIR, "base-setup"), "base\n");
import { append_file(join_path(RECIPE_DIR, "resolve"), "resolve\n"); "facts.rhai" } as facts;
const SNAPSHOT = read_file(join_path(RECIPE_DIR, "fact"));
let hook_counter = 0;
let ctx = #{ name: "api2", source: join_path(BUILD_DIR, "source.sh"),
             built: join_path(BUILD_DIR, "built.sh"), installed: join_path(BUILD_DIR, "bin/tool") };
fn inherited(ctx) { ctx.fact = facts::fact(); ctx.snapshot = global::SNAPSHOT; ctx }
fn is_acquired(ctx) {
    append_file(join_path(RECIPE_DIR, "acquire-checks"), "check\n");
    if !is_file(ctx.source) { return needs_work(ctx, "source is missing"); }
    ready(inherited(ctx))
}
fn is_built(ctx) {
    hook_counter += 100;
    append_file(join_path(RECIPE_DIR, "build-checks"), "check\n");
    if !is_file(ctx.built) { return needs_work(ctx, "tool has not been built"); }
    ready(inherited(ctx))
}
fn is_installed(ctx) {
    ctx.discovered = "carried from readiness";
    if !is_file(ctx.installed) { return needs_work(ctx, "tool has not been installed"); }
    ready(inherited(ctx))
}
fn acquire(ctx) {
    mkdir(BUILD_DIR);
    write_file(ctx.source, "#!/bin/sh\nprintf 'usable tool\\n'\n");
    ctx
}
fn build(ctx) {
    if hook_counter != 0 { throw "readiness leaked variable changes"; }
    hook_counter += 1;
    exec_checked("sh", ["-n", ctx.source]);
    copy_file(ctx.source, ctx.built);
    ctx
}
fn install(ctx) {
    if hook_counter != 1 { throw "initialized globals were lost"; }
    mkdir(dirname(ctx.installed));
    copy_file(ctx.built, ctx.installed);
    chmod(ctx.installed, 0o755);
    write_file(join_path(BUILD_DIR, "tool-result"), exec_output(ctx.installed, []));
    inherited(ctx)
}
fn cleanup(ctx, reason) {
    append_file(join_path(RECIPE_DIR, "cleanup-reasons"), reason + "\n");
    inherited(ctx)
}
fn remove(ctx) { rm(ctx.installed); inherited(ctx) }
"##,
    )
    .unwrap();
    write_recipe(
        &recipes,
        "api2",
        r##"//! extends: base.rhai
append_file(join_path(RECIPE_DIR, "child-setup"), "child\n");
"##,
    );
    // write_recipe's default cleanup would override the inherited implementation.
    let child = std::fs::read_to_string(recipes.join("api2.rhai"))
        .unwrap()
        .replace("\nfn cleanup(ctx, reason) { ctx }\n", "");
    std::fs::write(recipes.join("api2.rhai"), child).unwrap();
    let output = run_recipe(&["install", "api2", "--no-persist-ctx"], &recipes);
    assert_success(&output);
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(ctx["fact"], "imported fact");
    assert_eq!(ctx["snapshot"], "current package fact");
    assert_eq!(ctx["discovered"], "carried from readiness");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("build/tool-result")).unwrap(),
        "usable tool\n"
    );
    for file in ["base-setup", "child-setup", "resolve", "module-setup"] {
        assert_eq!(
            std::fs::read_to_string(recipes.join(file))
                .unwrap()
                .lines()
                .count(),
            1,
            "{file}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(recipes.join("acquire-checks"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("build-checks"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("cleanup-reasons")).unwrap(),
        "auto.acquire.success\nauto.build.success\nauto.install.success\n"
    );
    for action in [
        "install",
        "isacquired",
        "isbuilt",
        "isinstalled",
        "cleanup",
        "remove",
    ] {
        // Each command gets fresh setup; none reruns setup for its hook calls.
        assert_success(&run_recipe(&[action, "api2", "--no-persist-ctx"], &recipes));
    }
    for file in ["base-setup", "child-setup", "resolve", "module-setup"] {
        assert_eq!(
            std::fs::read_to_string(recipes.join(file))
                .unwrap()
                .lines()
                .count(),
            7,
            "{file}"
        );
    }
    assert!(!dir.path().join("build/bin/tool").exists());
}

#[test]
fn cli_readiness_errors_do_not_authorize_installation() {
    let (_dir, recipes) = create_test_env();
    for (name, version, check, diagnostic) in [
        (
            "api2-throw",
            "const RECIPE_API = 2;",
            "throw \"CHECK_ERROR\";",
            "CHECK_ERROR",
        ),
        ("api2-map", "const RECIPE_API = 2;", "ctx", "ready(ctx)"),
        (
            "api2-invalid",
            "const RECIPE_API = 2;",
            "#{status: \"needs_work\", ctx: ctx, reason: \"\"}",
            "nonempty reason",
        ),
        (
            "api2-budget",
            "const RECIPE_API = 2;",
            "while true {} ctx",
            "operations",
        ),
        (
            "legacy-missing-function",
            "",
            "missing_readiness_function(ctx)",
            "missing_readiness_function",
        ),
        (
            "legacy-io",
            "",
            "read_file(\"/nonexistent/recipe-readiness-input\"); ctx",
            "read failed",
        ),
        (
            "legacy-process",
            "",
            "exec_checked(\"sh\", [\"-c\", \"exit 23\"]); ctx",
            "23",
        ),
        ("legacy-type", "", "true", "must return ctx"),
    ] {
        write_recipe(
            &recipes,
            name,
            &format!(
                r##"
{version}
let ctx = #{{name: "{name}"}};
fn is_installed(ctx) {{ {check} }}
fn is_built(ctx) {{ ctx }}
fn install(ctx) {{ write_file(join_path(RECIPE_DIR, "unauthorized-install"), "bad"); ctx }}
"##
            ),
        );
        let output = run_recipe(&["install", name, "--no-persist-ctx"], &recipes);
        assert!(
            !output.status.success(),
            "{name}: check failure was suppressed"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(diagnostic), "{name}: {stderr}");
        assert!(!recipes.join("unauthorized-install").exists());
    }
}

#[test]
fn cli_api2_rejects_unsupported_api_before_setup_and_failed_producer_after_hook() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "future",
        r##"
const RECIPE_API = 999;
write_file(join_path(RECIPE_DIR, "unsupported-setup"), "bad");
let ctx = #{name: "future"};
"##,
    );
    let output = run_recipe(&["install", "future", "--no-persist-ctx"], &recipes);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Unsupported recipe API"));
    assert!(!recipes.join("unsupported-setup").exists());
    write_recipe(
        &recipes,
        "broken-producer",
        r##"
const RECIPE_API = 2;
let ctx = #{name: "broken-producer"};
fn is_installed(ctx) { needs_work(ctx, "real product is missing") }
fn is_built(ctx) { ready(ctx) }
fn install(ctx) { write_file(join_path(RECIPE_DIR, "incomplete-product"), "partial"); ctx }
fn cleanup(ctx, reason) { write_file(join_path(RECIPE_DIR, "failure-cleanup"), reason); ctx }
"##,
    );
    let output = run_recipe(
        &["install", "broken-producer", "--no-persist-ctx"],
        &recipes,
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("real product is missing"));
    assert_eq!(
        std::fs::read_to_string(recipes.join("failure-cleanup")).unwrap(),
        "auto.install.failure"
    );
}

#[test]
fn cli_legacy_retains_top_level_refresh_and_can_handle_native_errors_explicitly() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "legacy-refresh",
        r##"
append_file(join_path(RECIPE_DIR, "refreshes"), "refresh\n");
let ctx = #{name: "legacy-refresh"};
fn is_installed(ctx) {
    try { read_file(join_path(RECIPE_DIR, "missing-product")); }
    catch(error) { throw "product must be installed"; }
    ctx
}
fn is_built(ctx) { ctx }
fn install(ctx) {
    mkdir(BUILD_DIR);
    write_file(join_path(BUILD_DIR, "observed-refreshes"), read_file(join_path(RECIPE_DIR, "refreshes")));
    ctx
}
"##,
    );
    assert_success(&run_recipe(
        &["install", "legacy-refresh", "--no-persist-ctx"],
        &recipes,
    ));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("build/observed-refreshes"))
            .unwrap()
            .lines()
            .count(),
        4
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("refreshes"))
            .unwrap()
            .lines()
            .count(),
        5
    );
}

#[test]
fn cli_api2_dependency_tool_uses_explicit_checks_and_runs_in_parent() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "api2-tool",
        r##"
const RECIPE_API = 2;
append_file(join_path(RECIPE_DIR, "dependency-setup"), "setup\n");
let ctx = #{name: "api2-tool", source: join_path(BUILD_DIR, "tool.sh"),
            binary: join_path(TOOLS_PREFIX, "bin/api2-tool")};
fn is_installed(ctx) {
    if !is_file(ctx.binary) { return needs_work(ctx, "tool not installed"); }
    ready(ctx)
}
fn is_acquired(ctx) {
    if !is_file(ctx.source) { return needs_work(ctx, "tool source missing"); }
    ready(ctx)
}
fn acquire(ctx) { mkdir(BUILD_DIR); write_file(ctx.source, "#!/bin/sh\nprintf 'dependency works\\n'\n"); ctx }
fn install(ctx) { mkdir(dirname(ctx.binary)); copy_file(ctx.source, ctx.binary); chmod(ctx.binary, 0o755); ctx }
"##,
    );
    write_recipe(
        &recipes,
        "api2-parent",
        r##"
const RECIPE_API = 2;
let deps = ["api2-tool"];
let ctx = #{name: "api2-parent", output: join_path(BUILD_DIR, "dependency-result")};
fn is_installed(ctx) {
    if !is_file(ctx.output) { return needs_work(ctx, "dependency not consumed"); }
    ready(ctx)
}
fn is_built(ctx) { ready(ctx) }
fn install(ctx) { mkdir(BUILD_DIR); write_file(ctx.output, exec_output("api2-tool", [])); ctx }
"##,
    );
    assert_success(&run_recipe(
        &["install", "api2-parent", "--no-persist-ctx"],
        &recipes,
    ));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("build/dependency-result")).unwrap(),
        "dependency works\n"
    );
    assert_eq!(
        std::fs::read_to_string(recipes.join("dependency-setup")).unwrap(),
        "setup\n"
    );
}

#[test]
fn cli_api2_preserves_failure_cleanup_after_resource_exhaustion_and_bad_success_cleanup() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "budget-build",
        r##"
const RECIPE_API = 2;
let ctx = #{name: "budget-build"};
fn is_installed(ctx) { needs_work(ctx, "not installed") }
fn is_built(ctx) { needs_work(ctx, "not built") }
fn is_acquired(ctx) { ready(ctx) }
fn build(ctx) { while true {} ctx }
fn install(ctx) { write_file(join_path(RECIPE_DIR, "unexpected-install"), "bad"); ctx }
fn cleanup(ctx, reason) { write_file(join_path(RECIPE_DIR, "budget-cleanup"), reason); ctx }
"##,
    );
    let output = run_recipe(&["install", "budget-build", "--no-persist-ctx"], &recipes);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("operations"));
    assert!(!recipes.join("unexpected-install").exists());
    assert_eq!(
        std::fs::read_to_string(recipes.join("budget-cleanup")).unwrap(),
        "auto.build.failure"
    );

    write_recipe(
        &recipes,
        "cleanup-product",
        r##"
const RECIPE_API = 2;
let ctx = #{name: "cleanup-product", product: join_path(RECIPE_DIR, "cleanup-product-file")};
fn is_installed(ctx) {
    if !is_file(ctx.product) { return needs_work(ctx, "installed product missing"); }
    ready(ctx)
}
fn is_built(ctx) { ready(ctx) }
fn install(ctx) { write_file(ctx.product, "installed"); ctx }
fn cleanup(ctx, reason) {
    if reason == "auto.install.success" { rm(ctx.product); }
    write_file(join_path(RECIPE_DIR, "product-cleanup"), reason);
    ctx
}
"##,
    );
    let output = run_recipe(
        &["install", "cleanup-product", "--no-persist-ctx"],
        &recipes,
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("installed product missing"));
    assert_eq!(
        std::fs::read_to_string(recipes.join("product-cleanup")).unwrap(),
        "auto.install.failure"
    );
}
