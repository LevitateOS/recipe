//! Real public-library execution on the public Ginux lifecycle.
//!
//! Preconditions: a real Ginux, privileges and the imported compiler image, as
//! in the header of `tests/e2e.rs`: RECIPE_GINUX, RECIPE_IMAGE,
//! RECIPE_IMAGE_LAYOUT, RECIPE_WORKER_BIN, the Ginux files root-owned (or
//! GINUX_INSTANCE_HELPER), root or the sudoers rule with
//! RECIPE_GINUX_STATE_ROOT/RECIPE_GINUX_RUN_ROOT naming its roots, and none of
//! the old GINUX_BIN/GINUX_ROOT* variables. This test process sets
//! RECIPE_WORKER=1 and RECIPE_INVOCATION=spoofed-public-call itself.
use levitate_recipe::{ExecutionMode, ExecutionOptions, RecipeEngine};

#[test]
fn library_ginux_run_ignores_spoofed_worker_environment() {
    // Configure RECIPE_GINUX, RECIPE_IMAGE[_LAYOUT], RECIPE_WORKER_BIN and
    // deliberately set RECIPE_WORKER=1/RECIPE_INVOCATION=spoofed-public-call in
    // the test process.
    assert_eq!(std::env::var("RECIPE_WORKER").unwrap(), "1");
    let directory = tempfile::tempdir().unwrap();
    let recipe = directory.path().join("library.rhai");
    let source = "const RECIPE_API = 1; const NEEDS = #{ outputs: #{ report: \"file\" } }; fn build(src, out) { write_file(out, WORK); }\n";
    std::fs::write(&recipe, source).unwrap();
    // The recipe starts no program, so `auto` would run it on the host. This
    // test is about the Ginux domain: a caller that looks like a worker must
    // still get one.
    let mut options = ExecutionOptions::default();
    options.execution = ExecutionMode::Ginux;
    let engine = RecipeEngine::new(directory.path().join("host-build"))
        .with_state_persistence(false)
        .with_execution_options(options);
    let result = engine.install(&recipe).unwrap();
    // The recipe ran in the Ginux domain: its work folder is the domain's.
    let report = result["outputs"]["report"].as_str().unwrap();
    assert_eq!(std::fs::read_to_string(report).unwrap(), "/work/build");
    assert_eq!(
        std::path::Path::new(engine.ginux().unwrap().cli()),
        std::path::Path::new(&std::env::var_os("RECIPE_GINUX").unwrap())
    );
    assert_eq!(std::fs::read_to_string(&recipe).unwrap(), source);
}
