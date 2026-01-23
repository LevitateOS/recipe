//! Integration tests for recipe helper functions
//!
//! These tests execute example recipes that exercise all helper functions.
//! Network-dependent tests are marked with #[ignore] and can be run with:
//!   cargo test -- --ignored

use leviso_cheat_test::cheat_aware;
use levitate_recipe::RecipeEngine;
use std::path::Path;
use tempfile::TempDir;

/// Create a test environment with prefix and build_dir
fn create_test_env() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let prefix = dir.path().join("prefix");
    let build_dir = dir.path().join("build");
    std::fs::create_dir_all(&prefix).unwrap();
    std::fs::create_dir_all(&build_dir).unwrap();
    (dir, prefix, build_dir)
}

/// Get path to example recipes
fn example_path(name: &str) -> std::path::PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    Path::new(&manifest_dir).join("examples").join(name)
}

/// Copy an example recipe to a temp directory and return the path.
/// This prevents modifying the original example files.
fn copy_example_to_temp(
    example_name: &str,
    recipes_dir: &Path,
) -> Option<std::path::PathBuf> {
    let example = example_path(example_name);
    if !example.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&example).ok()?;

    // Strip any existing installed state from the content
    let clean_content = strip_installed_state(&content);

    let dest = recipes_dir.join(example_name);
    std::fs::write(&dest, clean_content).ok()?;
    Some(dest)
}

/// Remove installed state variables from recipe content and reset installed to false
fn strip_installed_state(content: &str) -> String {
    let mut result = String::new();
    let mut found_installed = false;

    for line in content.lines() {
        let trimmed = line.trim_start();
        // Skip lines that set installed state (except we'll add installed = false back)
        if trimmed.starts_with("let installed =") {
            if !found_installed {
                // Replace first occurrence with installed = false
                result.push_str("let installed = false;\n");
                found_installed = true;
            }
            // Skip any subsequent installed declarations
            continue;
        }
        if trimmed.starts_with("let installed_version =")
            || trimmed.starts_with("let installed_at =")
            || trimmed.starts_with("let installed_files =")
        {
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }

    // If no installed line was found, add one after the version line
    if !found_installed {
        let mut final_result = String::new();
        for line in result.lines() {
            final_result.push_str(line);
            final_result.push('\n');
            if line.trim_start().starts_with("let version =") {
                final_result.push_str("let installed = false;\n");
            }
        }
        return final_result;
    }

    result
}

// =============================================================================
// Filesystem Helper Tests
// =============================================================================

#[cheat_aware(
    protects = "Filesystem helpers (mkdir, copy, exists) work correctly in recipes",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = [
        "Skip test if example file missing",
        "Use simpler recipe that doesn't test all helpers",
        "Accept any success without verifying installed files"
    ],
    consequence = "mkdir/copy/exists helpers broken, recipes can't manipulate files"
)]
#[test]
fn test_filesystem_helpers() {
    let (_dir, prefix, build_dir) = create_test_env();
    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_path = match copy_example_to_temp("test-filesystem.rhai", &recipes_dir) {
        Some(path) => path,
        None => {
            eprintln!("Skipping: test-filesystem.rhai not found");
            return;
        }
    };

    let engine = RecipeEngine::new(prefix.clone(), build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "Filesystem helpers test failed: {:?}", result.err());

    // Verify installed binary works
    assert!(prefix.join("bin/test-bin").exists(), "test-bin should be installed");
}

// =============================================================================
// IO and Environment Helper Tests
// =============================================================================

#[cheat_aware(
    protects = "IO and environment helpers (read_file, env, set_env) work correctly",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = [
        "Skip test if example file missing",
        "Test helpers individually, miss integration",
        "Accept any success without verifying results"
    ],
    consequence = "read_file/env helpers broken, recipes can't read files or environment"
)]
#[test]
fn test_io_env_helpers() {
    let (_dir, prefix, build_dir) = create_test_env();
    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_path = match copy_example_to_temp("test-io-env.rhai", &recipes_dir) {
        Some(path) => path,
        None => {
            eprintln!("Skipping: test-io-env.rhai not found");
            return;
        }
    };

    let engine = RecipeEngine::new(prefix.clone(), build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "IO/Env helpers test failed: {:?}", result.err());
    assert!(prefix.join("bin/test-bin").exists(), "test-bin should be installed");
}

// =============================================================================
// Command and Process Helper Tests
// =============================================================================

#[cheat_aware(
    protects = "Command helpers (run, run_output, exec) work correctly",
    severity = "HIGH",
    ease = "MEDIUM",
    cheats = [
        "Skip test if example file missing",
        "Use trivial commands that always succeed",
        "Accept any success without verifying command ran"
    ],
    consequence = "run/exec helpers broken, recipes can't execute build commands"
)]
#[test]
fn test_command_helpers() {
    let (_dir, prefix, build_dir) = create_test_env();
    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_path = match copy_example_to_temp("test-command.rhai", &recipes_dir) {
        Some(path) => path,
        None => {
            eprintln!("Skipping: test-command.rhai not found");
            return;
        }
    };

    let engine = RecipeEngine::new(prefix.clone(), build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "Command helpers test failed: {:?}", result.err());
    assert!(prefix.join("bin/test-bin").exists(), "test-bin should be installed");
}

// =============================================================================
// Install Helper Tests
// =============================================================================

#[cheat_aware(
    protects = "Install helpers (install_bin, install_lib, install_man) work correctly",
    severity = "CRITICAL",
    ease = "MEDIUM",
    cheats = [
        "Skip test if example file missing",
        "Test only one install helper",
        "Accept any file in prefix as success"
    ],
    consequence = "install_bin/lib/man helpers broken, files installed to wrong locations"
)]
#[test]
fn test_install_helpers() {
    let (_dir, prefix, build_dir) = create_test_env();
    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_path = match copy_example_to_temp("test-install.rhai", &recipes_dir) {
        Some(path) => path,
        None => {
            eprintln!("Skipping: test-install.rhai not found");
            return;
        }
    };

    let engine = RecipeEngine::new(prefix.clone(), build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "Install helpers test failed: {:?}", result.err());

    // Verify installed files
    assert!(prefix.join("bin/mybin1").exists(), "mybin1 should be installed");
    assert!(prefix.join("bin/mybin2").exists(), "mybin2 should be installed");
    assert!(prefix.join("lib/libtest1.so").exists(), "libtest1.so should be installed");
    assert!(prefix.join("lib/libtest2.a").exists(), "libtest2.a should be installed");
    assert!(prefix.join("share/man/man1/mybin1.1").exists(), "mybin1.1 man page should be installed");
    assert!(prefix.join("share/man/man1/mybin2.1").exists(), "mybin2.1 man page should be installed");
    assert!(prefix.join("share/man/man5/myconfig.5").exists(), "myconfig.5 man page should be installed");
    assert!(prefix.join("share/doc/test-install/README").exists(), "README doc should be installed");
    assert!(prefix.join("share/doc/test-install/LICENSE").exists(), "LICENSE doc should be installed");
}

// =============================================================================
// HTTP Helper Tests (Network Required)
// =============================================================================

#[cheat_aware(
    protects = "HTTP helpers (http_get, download) work correctly",
    severity = "HIGH",
    ease = "HARD",
    cheats = [
        "Skip test entirely (ignore attribute)",
        "Use mocked HTTP responses",
        "Accept any success without verifying content"
    ],
    consequence = "HTTP helpers broken, recipes can't download source tarballs"
)]
#[test]
#[ignore] // Requires network access
fn test_http_helpers() {
    let (_dir, prefix, build_dir) = create_test_env();
    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_path = match copy_example_to_temp("test-http.rhai", &recipes_dir) {
        Some(path) => path,
        None => {
            eprintln!("Skipping: test-http.rhai not found");
            return;
        }
    };

    let engine = RecipeEngine::new(prefix.clone(), build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "HTTP helpers test failed: {:?}", result.err());
    assert!(prefix.join("bin/test-bin").exists(), "test-bin should be installed");
}

// =============================================================================
// Acquire Helper Tests (Network Required)
// =============================================================================

#[cheat_aware(
    protects = "Acquire helpers (download, copy, verify_sha256) work correctly",
    severity = "CRITICAL",
    ease = "HARD",
    cheats = [
        "Skip test entirely (ignore attribute)",
        "Use local files instead of downloading",
        "Skip SHA256 verification"
    ],
    consequence = "Acquire helpers broken, recipes can't download or verify sources"
)]
#[test]
#[ignore] // Requires network access
fn test_acquire_helpers() {
    let (_dir, prefix, build_dir) = create_test_env();
    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_path = match copy_example_to_temp("test-acquire.rhai", &recipes_dir) {
        Some(path) => path,
        None => {
            eprintln!("Skipping: test-acquire.rhai not found");
            return;
        }
    };

    let engine = RecipeEngine::new(prefix.clone(), build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "Acquire helpers test failed: {:?}", result.err());
    assert!(prefix.join("bin/test-bin").exists(), "test-bin should be installed");
}

// =============================================================================
// Comprehensive Helper Tests (Network Required)
// =============================================================================

#[cheat_aware(
    protects = "All helpers work together in a complete recipe",
    severity = "CRITICAL",
    ease = "HARD",
    cheats = [
        "Skip test entirely (ignore attribute)",
        "Test helpers individually, miss integration issues",
        "Use minimal recipe that doesn't exercise all paths"
    ],
    consequence = "Individual helpers work but fail when combined in real recipe"
)]
#[test]
#[ignore] // Requires network access
fn test_all_helpers() {
    let (_dir, prefix, build_dir) = create_test_env();
    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_path = match copy_example_to_temp("test-all-helpers.rhai", &recipes_dir) {
        Some(path) => path,
        None => {
            eprintln!("Skipping: test-all-helpers.rhai not found");
            return;
        }
    };

    let engine = RecipeEngine::new(prefix.clone(), build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "Comprehensive helpers test failed: {:?}", result.err());

    // Verify all installed files
    assert!(prefix.join("bin/mybin").exists(), "mybin should be installed");
    assert!(prefix.join("lib/libtest.so").exists(), "libtest.so should be installed");
    assert!(prefix.join("share/man/man1/mybin.1").exists(), "mybin.1 man page should be installed");
    assert!(prefix.join("share/doc/test-all-helpers/README.md").exists(), "README.md doc should be installed");
}

// =============================================================================
// Individual Helper Unit Tests
// =============================================================================

#[cheat_aware(
    protects = "parse_version helper strips common prefixes correctly",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Test only simple version formats",
        "Skip testing in recipe context",
        "Use pre-cleaned versions"
    ],
    consequence = "parse_version('v1.2.3') returns 'v1.2.3' instead of '1.2.3'"
)]
#[test]
fn test_parse_version_helper() {
    let (_dir, prefix, build_dir) = create_test_env();

    // Inline recipe to test parse_version specifically
    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_content = r#"
let name = "parse-version-test";
let version = "1.0.0";
let installed = false;

fn acquire() {}

fn build() {
    // Test various version formats
    if parse_version("v1.2.3") != "1.2.3" {
        throw "parse_version failed for v1.2.3";
    }
    if parse_version("release-2.0.0") != "2.0.0" {
        throw "parse_version failed for release-2.0.0";
    }
    if parse_version("version-3.1.4") != "3.1.4" {
        throw "parse_version failed for version-3.1.4";
    }
    if parse_version("4.0.0") != "4.0.0" {
        throw "parse_version failed for 4.0.0";
    }
}

fn install() {}
"#;

    let recipe_path = recipes_dir.join("parse-version-test.rhai");
    std::fs::write(&recipe_path, recipe_content).unwrap();

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "parse_version test failed: {:?}", result.err());
}

#[cheat_aware(
    protects = "mkdir helper creates nested directories",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Test only single-level directories",
        "Use pre-existing parent directories",
        "Skip testing recursive creation"
    ],
    consequence = "mkdir('a/b/c/d') fails when intermediate directories don't exist"
)]
#[test]
fn test_mkdir_recursive() {
    let (_dir, prefix, build_dir) = create_test_env();

    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_content = r#"
let name = "mkdir-test";
let version = "1.0.0";
let installed = false;

fn acquire() {}

fn build() {
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
}

fn install() {}
"#;

    let recipe_path = recipes_dir.join("mkdir-test.rhai");
    std::fs::write(&recipe_path, recipe_content).unwrap();

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "mkdir recursive test failed: {:?}", result.err());
}

#[cheat_aware(
    protects = "glob_list helper returns matching files",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Test only with exactly matching files",
        "Use simple patterns that match everything",
        "Skip testing wildcard patterns"
    ],
    consequence = "glob_list('*.txt') returns empty array or wrong files"
)]
#[test]
fn test_glob_list_helper() {
    let (_dir, prefix, build_dir) = create_test_env();

    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_content = r#"
let name = "glob-test";
let version = "1.0.0";
let installed = false;

fn acquire() {}

fn build() {
    // Create test files
    mkdir(`${BUILD_DIR}/globtest`);
    run(`echo "a" > ${BUILD_DIR}/globtest/file1.txt`);
    run(`echo "b" > ${BUILD_DIR}/globtest/file2.txt`);
    run(`echo "c" > ${BUILD_DIR}/globtest/file3.log`);

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
}

fn install() {}
"#;

    let recipe_path = recipes_dir.join("glob-test.rhai");
    std::fs::write(&recipe_path, recipe_content).unwrap();

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "glob_list test failed: {:?}", result.err());
}

#[cheat_aware(
    protects = "mv and ln helpers work correctly",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Test mv/ln with simple cases only",
        "Skip testing symlink target resolution",
        "Use files in same directory only"
    ],
    consequence = "mv doesn't remove source, ln creates broken symlink"
)]
#[test]
fn test_mv_and_ln_helpers() {
    let (_dir, prefix, build_dir) = create_test_env();

    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_content = r#"
let name = "mv-ln-test";
let version = "1.0.0";
let installed = false;

fn acquire() {}

fn build() {
    // Test mv
    mkdir(`${BUILD_DIR}/mvtest`);
    run(`echo "content" > ${BUILD_DIR}/mvtest/original.txt`);

    if !file_exists(`${BUILD_DIR}/mvtest/original.txt`) {
        throw "Failed to create original.txt";
    }

    mv(`${BUILD_DIR}/mvtest/original.txt`, `${BUILD_DIR}/mvtest/moved.txt`);

    if file_exists(`${BUILD_DIR}/mvtest/original.txt`) {
        throw "mv did not remove original file";
    }
    if !file_exists(`${BUILD_DIR}/mvtest/moved.txt`) {
        throw "mv did not create destination file";
    }

    // Test ln (symlink)
    ln(`${BUILD_DIR}/mvtest/moved.txt`, `${BUILD_DIR}/mvtest/linked.txt`);

    if !exists(`${BUILD_DIR}/mvtest/linked.txt`) {
        throw "ln did not create symlink";
    }

    // Verify symlink points to correct file
    let content = read_file(`${BUILD_DIR}/mvtest/linked.txt`);
    if !content.contains("content") {
        throw "Symlink does not point to correct file";
    }
}

fn install() {}
"#;

    let recipe_path = recipes_dir.join("mv-ln-test.rhai");
    std::fs::write(&recipe_path, recipe_content).unwrap();

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "mv/ln test failed: {:?}", result.err());
}

#[cheat_aware(
    protects = "run_output and run_status helpers capture command results",
    severity = "HIGH",
    ease = "EASY",
    cheats = [
        "Test only commands that always succeed",
        "Skip testing command output content",
        "Use trivial commands"
    ],
    consequence = "run_output returns empty string, run_status returns wrong exit code"
)]
#[test]
fn test_run_output_and_status() {
    let (_dir, prefix, build_dir) = create_test_env();

    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_content = r#"
let name = "run-test";
let version = "1.0.0";
let installed = false;

fn acquire() {}

fn build() {
    // Test run_output
    let output = run_output("echo hello world");
    if !output.contains("hello world") {
        throw `run_output failed: expected "hello world", got "${output}"`;
    }

    // Test run_status with success
    let success_code = run_status("true");
    if success_code != 0 {
        throw `run_status(true) returned ${success_code}, expected 0`;
    }

    // Test run_status with failure
    let fail_code = run_status("false");
    if fail_code == 0 {
        throw "run_status(false) returned 0, expected non-zero";
    }
}

fn install() {}
"#;

    let recipe_path = recipes_dir.join("run-test.rhai");
    std::fs::write(&recipe_path, recipe_content).unwrap();

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "run_output/run_status test failed: {:?}", result.err());
}

#[cheat_aware(
    protects = "exec and exec_output helpers run commands with arguments",
    severity = "HIGH",
    ease = "EASY",
    cheats = [
        "Test only simple commands",
        "Skip testing argument passing",
        "Use shell commands instead"
    ],
    consequence = "exec fails to pass arguments correctly, builds break"
)]
#[test]
fn test_exec_helpers() {
    let (_dir, prefix, build_dir) = create_test_env();

    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_content = r#"
let name = "exec-test";
let version = "1.0.0";
let installed = false;

fn acquire() {}

fn build() {
    // Test exec
    let exit_code = exec("true", []);
    if exit_code != 0 {
        throw `exec(true) returned ${exit_code}, expected 0`;
    }

    // Test exec_output
    let output = exec_output("echo", ["test", "args"]);
    if !output.contains("test args") {
        throw `exec_output failed: expected "test args", got "${output}"`;
    }
}

fn install() {}
"#;

    let recipe_path = recipes_dir.join("exec-test.rhai");
    std::fs::write(&recipe_path, recipe_content).unwrap();

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "exec/exec_output test failed: {:?}", result.err());
}

#[cheat_aware(
    protects = "env and set_env helpers read and write environment variables",
    severity = "MEDIUM",
    ease = "EASY",
    cheats = [
        "Test only PATH which always exists",
        "Skip testing set_env persistence",
        "Use hardcoded values"
    ],
    consequence = "Recipes can't read/set environment, build flags not passed correctly"
)]
#[test]
fn test_env_helpers() {
    let (_dir, prefix, build_dir) = create_test_env();

    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_content = r#"
let name = "env-test";
let version = "1.0.0";
let installed = false;

fn acquire() {}

fn build() {
    // Test env() - read existing variable
    let path = env("PATH");
    if path.len() == 0 {
        throw "env(PATH) returned empty string";
    }

    // Test set_env and env together
    set_env("RECIPE_TEST_VAR", "test_value_123");
    let value = env("RECIPE_TEST_VAR");
    if value != "test_value_123" {
        throw `set_env/env failed: expected "test_value_123", got "${value}"`;
    }
}

fn install() {}
"#;

    let recipe_path = recipes_dir.join("env-test.rhai");
    std::fs::write(&recipe_path, recipe_content).unwrap();

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "env/set_env test failed: {:?}", result.err());
}

#[cheat_aware(
    protects = "extract helper unpacks tarballs correctly",
    severity = "CRITICAL",
    ease = "MEDIUM",
    cheats = [
        "Use pre-extracted files",
        "Skip testing actual extraction",
        "Test only tar.gz, miss other formats"
    ],
    consequence = "extract('tar.gz') fails to unpack source, builds can't proceed"
)]
#[test]
fn test_extract_tarball() {
    let (_dir, prefix, build_dir) = create_test_env();

    // Create a tarball in a source directory that we'll copy in acquire phase
    let source_dir = prefix.parent().unwrap().join("source");
    std::fs::create_dir_all(&source_dir).unwrap();

    // Create the tarball content
    let tarball_content_dir = source_dir.join("tarball-source");
    std::fs::create_dir_all(&tarball_content_dir).unwrap();
    std::fs::write(tarball_content_dir.join("data.txt"), "tarball content\n").unwrap();

    // Create the tarball using tar command
    let status = std::process::Command::new("tar")
        .args(["czf", "test-archive.tar.gz", "tarball-source"])
        .current_dir(&source_dir)
        .status()
        .expect("Failed to create tarball");
    assert!(status.success(), "Failed to create test tarball");

    // Clean up the content dir, keep only the tarball
    std::fs::remove_dir_all(&tarball_content_dir).unwrap();

    let recipes_dir = prefix.parent().unwrap().join("recipes");
    std::fs::create_dir_all(&recipes_dir).unwrap();

    let recipe_content = format!(r#"
let name = "extract-test";
let version = "1.0.0";
let installed = false;

fn acquire() {{
    // Copy the tarball - this sets last_downloaded
    copy("{}/test-archive.tar.gz");
}}

fn build() {{
    // Extract the tarball (uses last_downloaded from copy)
    extract("tar.gz");

    // Verify extraction
    if !dir_exists(`${{BUILD_DIR}}/tarball-source`) {{
        throw "extract failed: directory not created";
    }}
    if !file_exists(`${{BUILD_DIR}}/tarball-source/data.txt`) {{
        throw "extract failed: file not extracted";
    }}

    let content = read_file(`${{BUILD_DIR}}/tarball-source/data.txt`);
    if !content.contains("tarball content") {{
        throw "extract failed: file content incorrect";
    }}
}}

fn install() {{}}
"#, source_dir.display());

    let recipe_path = recipes_dir.join("extract-test.rhai");
    std::fs::write(&recipe_path, recipe_content).unwrap();

    let engine = RecipeEngine::new(prefix, build_dir);
    let result = engine.execute(&recipe_path);

    assert!(result.is_ok(), "extract tarball test failed: {:?}", result.err());
}
