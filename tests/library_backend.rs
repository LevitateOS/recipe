//! Real public-library execution against the configured Ginux supervisor.
use levitate_recipe::{ExecutionMode, ExecutionOptions, RecipeEngine};

#[test]
fn library_ginux_run_ignores_spoofed_worker_environment() {
    // Configure GINUX_BIN, GINUX_ROOTFS, RECIPE_WORKER_BIN and deliberately set
    // RECIPE_WORKER=1/RECIPE_INVOCATION=spoofed-public-call in the test process.
    assert_eq!(std::env::var("RECIPE_WORKER").unwrap(), "1");
    let directory = tempfile::tempdir().unwrap();
    let recipe = directory.path().join("library.rhai");
    let source = "const RECIPE_API = 2; const NEEDS = #{}; let ctx = #{name: \"library\"}; fn is_installed(ctx) { ready(ctx) }\n";
    std::fs::write(&recipe, source).unwrap();
    // The recipe starts no program, so `auto` would run it on the host. This
    // test is about the Ginux domain: a caller that looks like a worker must
    // still get one.
    let mut options = ExecutionOptions::default();
    options.execution = ExecutionMode::Ginux;
    let engine = RecipeEngine::new(directory.path().join("host-build"))
        .with_state_persistence(false)
        .with_execution_options(options);
    let context = engine.is_installed(&recipe).unwrap();
    let paths = &context["runtime"]["paths"];
    assert_eq!(paths["recipe_dir"], "/inputs");
    assert_eq!(paths["build"], "/work/build");
    assert_eq!(paths["tools"], "/work/tools");
    assert_eq!(
        engine.launcher().unwrap().path(),
        std::path::Path::new(&std::env::var_os("GINUX_BIN").unwrap())
    );
    assert_eq!(std::fs::read_to_string(&recipe).unwrap(), source);
}
