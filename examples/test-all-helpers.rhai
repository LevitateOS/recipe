// Test recipe: All helpers comprehensive test
// This recipe tests all helper functions in a single integration test
// Requires network access for HTTP tests

let name = "test-all-helpers";
let version = "1.0.0";
let description = "Comprehensive test of all recipe helper functions";
let installed = false;

fn acquire() {
    // === ACQUIRE HELPERS ===

    // Test download
    download("https://httpbin.org/robots.txt");

    // Test copy (create source first)
    mkdir(`${BUILD_DIR}/copy-source`);
    run(`echo "copy test" > ${BUILD_DIR}/copy-source/file.txt`);
    copy(`${BUILD_DIR}/copy-source/*.txt`);

    // Note: verify_sha256 would be tested with a known hash
    // Skipping as we don't have a stable hash to test against
}

fn build() {
    // === FILESYSTEM HELPERS ===

    // Test mkdir
    mkdir(`${BUILD_DIR}/test/nested/dir`);

    // Test exists, file_exists, dir_exists
    if !dir_exists(`${BUILD_DIR}/test/nested/dir`) {
        throw "mkdir or dir_exists failed";
    }
    run(`echo "test" > ${BUILD_DIR}/test/file.txt`);
    if !file_exists(`${BUILD_DIR}/test/file.txt`) {
        throw "file_exists failed";
    }
    if !exists(`${BUILD_DIR}/test`) {
        throw "exists failed";
    }

    // Test mv
    mv(`${BUILD_DIR}/test/file.txt`, `${BUILD_DIR}/test/renamed.txt`);
    if !file_exists(`${BUILD_DIR}/test/renamed.txt`) {
        throw "mv failed";
    }

    // Test ln
    ln(`${BUILD_DIR}/test/renamed.txt`, `${BUILD_DIR}/test/link.txt`);
    if !exists(`${BUILD_DIR}/test/link.txt`) {
        throw "ln failed";
    }

    // Test chmod
    chmod(`${BUILD_DIR}/test/renamed.txt`, 0o644);

    // Test rm
    run(`echo "delete me" > ${BUILD_DIR}/test/deleteme.txt`);
    rm(`${BUILD_DIR}/test/deleteme.txt`);
    if file_exists(`${BUILD_DIR}/test/deleteme.txt`) {
        throw "rm failed";
    }

    // === IO HELPERS ===

    // Test read_file
    let content = read_file(`${BUILD_DIR}/test/renamed.txt`);
    if !content.contains("test") {
        throw "read_file failed";
    }

    // Test glob_list
    run(`echo "a" > ${BUILD_DIR}/test/a.log`);
    run(`echo "b" > ${BUILD_DIR}/test/b.log`);
    let logs = glob_list(`${BUILD_DIR}/test/*.log`);
    if logs.len() != 2 {
        throw `glob_list failed: expected 2, got ${logs.len()}`;
    }

    // === ENV HELPERS ===

    // Test env
    let path = env("PATH");
    if path.len() == 0 {
        throw "env failed: PATH is empty";
    }

    // Test set_env
    set_env("TEST_RECIPE_VAR", "test_value");
    if env("TEST_RECIPE_VAR") != "test_value" {
        throw "set_env failed";
    }

    // === COMMAND HELPERS ===

    // Test run
    run("echo 'run test'");

    // Test shell (alias)
    shell("echo 'shell test'");

    // Test run_output
    let output = run_output("echo captured");
    if !output.contains("captured") {
        throw "run_output failed";
    }

    // Test run_status
    if run_status("true") != 0 {
        throw "run_status failed for 'true'";
    }
    if run_status("false") == 0 {
        throw "run_status failed for 'false'";
    }

    // === PROCESS HELPERS ===

    // Test exec
    if exec("true", []) != 0 {
        throw "exec failed";
    }

    // Test exec_output
    let exec_out = exec_output("echo", ["hello", "world"]);
    if !exec_out.contains("hello world") {
        throw "exec_output failed";
    }

    // === HTTP HELPERS ===

    // Test parse_version (no network needed)
    if parse_version("v1.2.3") != "1.2.3" {
        throw "parse_version failed";
    }

    // Test http_get
    let http_resp = http_get("https://httpbin.org/get");
    if !http_resp.contains("httpbin") {
        throw "http_get failed";
    }

    // Test github_latest_release
    let rg_ver = github_latest_release("BurntSushi/ripgrep");
    if rg_ver.len() == 0 {
        throw "github_latest_release failed";
    }

    // Test github_latest_tag
    let linux_tag = github_latest_tag("torvalds/linux");
    if linux_tag.len() == 0 {
        throw "github_latest_tag failed";
    }

    // === BUILD HELPERS ===

    // Test extract (create tarball first)
    mkdir(`${BUILD_DIR}/extract-test`);
    run(`echo "extract" > ${BUILD_DIR}/extract-test/data.txt`);
    cd(BUILD_DIR);
    run(`tar czf archive.tar.gz extract-test`);
    rm(`${BUILD_DIR}/extract-test`);
    extract("tar.gz");
    if !dir_exists(`${BUILD_DIR}/extract-test`) {
        throw "extract failed";
    }

    // Test cd
    cd(`${BUILD_DIR}/extract-test`);
    if !file_exists("data.txt") {
        throw "cd failed: data.txt not visible";
    }

    // === PREPARE FOR INSTALL ===
    cd(BUILD_DIR);
    mkdir(`${BUILD_DIR}/output`);

    // Create files for install tests
    run(`echo '#!/bin/sh\necho "bin"' > ${BUILD_DIR}/output/mybin`);
    chmod(`${BUILD_DIR}/output/mybin`, 0o755);
    run(`echo "lib content" > ${BUILD_DIR}/output/libtest.so`);
    run(`echo ".TH MYBIN 1" > ${BUILD_DIR}/output/mybin.1`);
    run(`echo "doc content" > ${BUILD_DIR}/output/README.md`);
}

fn install() {
    // === INSTALL HELPERS ===
    cd(`${BUILD_DIR}/output`);

    // Test install_bin
    install_bin("mybin");

    // Test install_lib
    install_lib("libtest.so");

    // Test install_man
    install_man("mybin.1");

    // Test install_to_dir
    install_to_dir("README.md", "share/doc/test-all-helpers");
}

fn check_update() {
    // Test check_update with github_latest_release
    let latest = github_latest_release("BurntSushi/ripgrep");
    if latest != version {
        return latest;
    }
}
