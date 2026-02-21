use super::*;

#[test]
fn test_no_is_installed_means_always_install() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    // Recipe without is_installed - should always run install
    let recipe_path = write_recipe(
        &recipes_dir,
        "no-check",
        r#"
let ctx = #{
    name: "no-check",
    run_count: 0,
};

fn acquire(ctx) {
    ctx.run_count = ctx.run_count + 1;
    ctx
}

fn install(ctx) {
    ctx.run_count = ctx.run_count + 1;
    ctx
}
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    engine.execute(&recipe_path).unwrap();

    let content = std::fs::read_to_string(&recipe_path).unwrap();
    assert!(content.contains("run_count: 2")); // acquire + install both ran
}

#[test]
fn test_multiple_recipes_independent() {
    let (_dir, build_dir, recipes_dir) = create_test_env();

    let recipe1 = write_recipe(
        &recipes_dir,
        "pkg1",
        r#"
let ctx = #{
    name: "pkg1",
    value: "one",
};

fn acquire(ctx) { ctx }
fn install(ctx) {
    ctx.value = "installed-one";
    ctx
}
"#,
    );

    let recipe2 = write_recipe(
        &recipes_dir,
        "pkg2",
        r#"
let ctx = #{
    name: "pkg2",
    value: "two",
};

fn acquire(ctx) { ctx }
fn install(ctx) {
    ctx.value = "installed-two";
    ctx
}
"#,
    );

    let engine = RecipeEngine::new(build_dir);
    engine.execute(&recipe1).unwrap();
    engine.execute(&recipe2).unwrap();

    let content1 = std::fs::read_to_string(&recipe1).unwrap();
    let content2 = std::fs::read_to_string(&recipe2).unwrap();

    assert!(content1.contains("installed-one"));
    assert!(content2.contains("installed-two"));
}
