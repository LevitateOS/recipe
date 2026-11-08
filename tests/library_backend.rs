//! Real public-library execution against the configured Ginux supervisor.
use levitate_recipe::RecipeEngine;

#[test]
fn library_default_uses_ginux_even_with_spoofed_worker_environment() {
    // Configure GINUX_BIN, GINUX_ROOTFS, RECIPE_WORKER_BIN and deliberately set
    // RECIPE_WORKER=1/RECIPE_INVOCATION=spoofed-public-call in the test process.
    assert_eq!(std::env::var("RECIPE_WORKER").unwrap(), "1");
    let directory = tempfile::tempdir().unwrap();
    let recipe = directory.path().join("library.rhai");
    let source = "const RECIPE_API = 2; const NEEDS = #{}; let ctx = #{name: \"library\"}; fn is_installed(ctx) { ready(ctx) }\n";
    std::fs::write(&recipe, source).unwrap();
    let engine =
        RecipeEngine::new(directory.path().join("host-build")).with_state_persistence(false);
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
