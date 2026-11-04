use super::*;

#[test]
fn test_extract_tarball() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "extract-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    // Create a test tarball
    mkdir(`${BUILD_DIR}/tartest`);
    shell(`echo "content" > ${BUILD_DIR}/tartest/file.txt`);
    shell(`tar czf ${BUILD_DIR}/test.tar.gz -C ${BUILD_DIR}/tartest .`);
    ctx
}

fn build(ctx) {
    // Extract the tarball
    mkdir(`${BUILD_DIR}/extracted`);
    extract(`${BUILD_DIR}/test.tar.gz`, `${BUILD_DIR}/extracted`);

    // Verify extraction
    if !file_exists(`${BUILD_DIR}/extracted/file.txt`) {
        throw "extract did not create expected file";
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("extract-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(
        result.is_ok(),
        "extract tarball test failed: {:?}",
        result.err()
    );
}

// =============================================================================
// Network Helper Tests (ignored by default)
// =============================================================================

#[test]
#[ignore]
fn test_download_helper() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "download-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    // Download a small file
    download("https://httpbin.org/robots.txt", `${BUILD_DIR}/robots.txt`);

    if !file_exists(`${BUILD_DIR}/robots.txt`) {
        throw "download did not create file";
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("download-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "download test failed: {:?}", result.err());
}

#[test]
#[ignore]
fn test_http_get_helper() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "http-get-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    // Fetch content without saving to file
    let content = http_get("https://httpbin.org/robots.txt");

    if content.len() == 0 {
        throw "http_get returned empty content";
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("http-get-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "http_get test failed: {:?}", result.err());
}

#[test]
#[ignore]
fn test_git_clone_helper() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "git-clone-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) {
    // Clone a small repo with depth=1
    git_clone("https://github.com/octocat/Hello-World.git", `${BUILD_DIR}/hello-world`, 1);

    if !dir_exists(`${BUILD_DIR}/hello-world/.git`) {
        throw "git_clone did not create .git directory";
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("git-clone-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "git_clone test failed: {:?}", result.err());
}
