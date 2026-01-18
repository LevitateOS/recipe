// Test recipe: IO and Environment helpers
// Tests: read_file, glob_list, env, set_env

let name = "test-io-env";
let version = "1.0.0";
let description = "Test recipe for IO and environment helper functions";
let installed = false;

fn acquire() {
    // Nothing to acquire
}

fn build() {
    // Create test files
    mkdir(`${BUILD_DIR}/docs`);
    run(`echo "content of file A" > ${BUILD_DIR}/docs/a.txt`);
    run(`echo "content of file B" > ${BUILD_DIR}/docs/b.txt`);
    run(`echo "content of file C" > ${BUILD_DIR}/docs/c.md`);

    // Test read_file
    let content = read_file(`${BUILD_DIR}/docs/a.txt`);
    if !content.contains("content of file A") {
        throw `read_file failed: got "${content}"`;
    }

    // Test glob_list
    let txt_files = glob_list(`${BUILD_DIR}/docs/*.txt`);
    if txt_files.len() != 2 {
        throw `glob_list failed: expected 2 .txt files, got ${txt_files.len()}`;
    }

    let all_files = glob_list(`${BUILD_DIR}/docs/*`);
    if all_files.len() != 3 {
        throw `glob_list failed: expected 3 files, got ${all_files.len()}`;
    }

    // Test env - read existing env var
    let path = env("PATH");
    if path.len() == 0 {
        throw "env failed: PATH is empty";
    }

    // Test env - read non-existent var (returns empty string)
    let nonexistent = env("RECIPE_TEST_NONEXISTENT_VAR_12345");
    if nonexistent.len() != 0 {
        throw `env failed: expected empty for nonexistent var, got "${nonexistent}"`;
    }

    // Test set_env
    set_env("RECIPE_TEST_VAR", "hello_from_recipe");
    let test_var = env("RECIPE_TEST_VAR");
    if test_var != "hello_from_recipe" {
        throw `set_env failed: expected "hello_from_recipe", got "${test_var}"`;
    }

    // Create a binary for install
    run(`echo '#!/bin/sh\necho "test-io-env"' > ${BUILD_DIR}/test-bin`);
    chmod(`${BUILD_DIR}/test-bin`, 0o755);
}

fn install() {
    cd(BUILD_DIR);
    install_bin("test-bin");
}
