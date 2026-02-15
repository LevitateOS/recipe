//! Integration tests for recipe helper functions
//!
//! These tests execute example recipes that exercise helper functions.
//! Network-dependent tests are marked with #[ignore] and can be run with:
//!   cargo test -- --ignored

use levitate_recipe::RecipeEngine;
use tempfile::TempDir;

/// Create a test environment with build_dir
fn create_test_env() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let recipes_dir = dir.path().join("recipes");
    let build_dir = dir.path().join("build");
    std::fs::create_dir_all(&recipes_dir).unwrap();
    std::fs::create_dir_all(&build_dir).unwrap();
    (dir, recipes_dir, build_dir)
}

fn write_recipe(path: &std::path::Path, content: &str) {
    let mut content = content.to_string();
    // Cleanup is required by repo policy; helper tests default to a no-op stub.
    if !content.contains("fn cleanup(") {
        content.push_str("\nfn cleanup(ctx, reason) { ctx }\n");
    }
    std::fs::write(path, content).unwrap();
}

// =============================================================================
// Filesystem Helper Tests
// =============================================================================

#[test]
fn test_mkdir_recursive() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "mkdir-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    // Test recursive directory creation
    mkdir(`${BUILD_DIR}/deep/nested/path/here`);

    if !dir_exists(`${BUILD_DIR}/deep/nested/path/here`) {
        throw "mkdir recursive failed";
    }
    if !dir_exists(`${BUILD_DIR}/deep/nested/path`) {
        throw "mkdir did not create parent";
    }
    if !dir_exists(`${BUILD_DIR}/deep/nested`) {
        throw "mkdir did not create grandparent";
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("mkdir-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(
        result.is_ok(),
        "mkdir recursive test failed: {:?}",
        result.err()
    );
}

#[test]
fn test_glob_list_helper() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "glob-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    // Create test files
    mkdir(`${BUILD_DIR}/globtest`);
    shell(`echo "a" > ${BUILD_DIR}/globtest/file1.txt`);
    shell(`echo "b" > ${BUILD_DIR}/globtest/file2.txt`);
    shell(`echo "c" > ${BUILD_DIR}/globtest/file3.log`);

    // Test glob_list for .txt files
    let txt_files = glob_list(`${BUILD_DIR}/globtest/*.txt`);
    if txt_files.len() != 2 {
        throw `glob_list *.txt expected 2 files, got ${txt_files.len()}`;
    }

    // Test glob_list for .log files
    let log_files = glob_list(`${BUILD_DIR}/globtest/*.log`);
    if log_files.len() != 1 {
        throw `glob_list *.log expected 1 file, got ${log_files.len()}`;
    }

    // Test glob_list for all files
    let all_files = glob_list(`${BUILD_DIR}/globtest/*`);
    if all_files.len() != 3 {
        throw `glob_list * expected 3 files, got ${all_files.len()}`;
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("glob-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "glob_list test failed: {:?}", result.err());
}

#[test]
fn test_mv_and_ln_helpers() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "mv-ln-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    mkdir(`${BUILD_DIR}/mvtest`);

    // Create and move a file
    shell(`echo "content" > ${BUILD_DIR}/mvtest/original.txt`);
    mv(`${BUILD_DIR}/mvtest/original.txt`, `${BUILD_DIR}/mvtest/moved.txt`);

    if file_exists(`${BUILD_DIR}/mvtest/original.txt`) {
        throw "mv did not remove source";
    }
    if !file_exists(`${BUILD_DIR}/mvtest/moved.txt`) {
        throw "mv did not create dest";
    }

    // Create a symlink
    ln(`${BUILD_DIR}/mvtest/moved.txt`, `${BUILD_DIR}/mvtest/link.txt`);
    if !file_exists(`${BUILD_DIR}/mvtest/link.txt`) {
        throw "ln did not create symlink";
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("mv-ln-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "mv/ln test failed: {:?}", result.err());
}

#[test]
fn test_shell_output_and_status() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "shell-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    // Test shell_output
    let output = shell_output("echo hello");
    if !output.contains("hello") {
        throw `shell_output expected "hello", got "${output}"`;
    }

    // Test shell_status for success
    let status = shell_status("true");
    if status != 0 {
        throw `shell_status for 'true' expected 0, got ${status}`;
    }

    // Test shell_status for failure
    let fail_status = shell_status("false");
    if fail_status == 0 {
        throw `shell_status for 'false' expected non-zero, got ${fail_status}`;
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("shell-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(
        result.is_ok(),
        "shell_output/shell_status test failed: {:?}",
        result.err()
    );
}

#[test]
fn test_exec_helpers() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "exec-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    // Test exec with args
    exec("echo", ["hello", "world"]);

    // Test exec_output
    let output = exec_output("echo", ["test", "args"]);
    if !output.contains("test") || !output.contains("args") {
        throw `exec_output expected "test args", got "${output}"`;
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("exec-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(
        result.is_ok(),
        "exec/exec_output test failed: {:?}",
        result.err()
    );
}

#[test]
fn test_env_helpers() {
    let (_dir, recipes_dir, build_dir) = create_test_env();

    let recipe_content = r#"
let ctx = #{
    name: "env-test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }

fn build(ctx) {
    // Set an environment variable
    set_env("RECIPE_TEST_VAR", "test_value");

    // Read it back
    let val = env("RECIPE_TEST_VAR");
    if val != "test_value" {
        throw `env expected "test_value", got "${val}"`;
    }

    // Test reading PATH (should exist)
    let path = env("PATH");
    if path.len() == 0 {
        throw "PATH environment variable is empty";
    }
    ctx
}

fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#;

    let recipe_path = recipes_dir.join("env-test.rhai");
    write_recipe(&recipe_path, recipe_content);

    let engine = RecipeEngine::new(build_dir);
    let result = engine.execute(&recipe_path);

    assert!(
        result.is_ok(),
        "env/set_env test failed: {:?}",
        result.err()
    );
}

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

#[test]
fn test_parse_version_helper() {
    use levitate_recipe::helpers::acquire::http::parse_version;

    // Test various version formats
    assert_eq!(parse_version("v1.0.0"), "1.0.0");
    assert_eq!(parse_version("version-1.0.0"), "1.0.0");
    assert_eq!(parse_version("release-v2.0.0"), "2.0.0");
    assert_eq!(parse_version("1.2.3"), "1.2.3");
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
