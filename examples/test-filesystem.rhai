// Test recipe: Filesystem helpers
// Tests: mkdir, exists, file_exists, dir_exists, rm, mv, ln, chmod

let name = "test-filesystem";
let version = "1.0.0";
let description = "Test recipe for filesystem helper functions";
let installed = false;

fn acquire() {
    // Nothing to acquire - we create test files locally
}

fn build() {
    // Test mkdir - create nested directories
    mkdir(`${BUILD_DIR}/test-dir/nested/deep`);

    // Test dir_exists
    if !dir_exists(`${BUILD_DIR}/test-dir`) {
        throw "mkdir failed: test-dir doesn't exist";
    }
    if !dir_exists(`${BUILD_DIR}/test-dir/nested/deep`) {
        throw "mkdir failed: nested dirs don't exist";
    }

    // Create test files using run
    run(`echo "hello world" > ${BUILD_DIR}/test-dir/file1.txt`);
    run(`echo "goodbye world" > ${BUILD_DIR}/test-dir/file2.txt`);

    // Test file_exists
    if !file_exists(`${BUILD_DIR}/test-dir/file1.txt`) {
        throw "file_exists failed: file1.txt not found";
    }

    // Test exists (works for both files and dirs)
    if !exists(`${BUILD_DIR}/test-dir`) {
        throw "exists failed for directory";
    }
    if !exists(`${BUILD_DIR}/test-dir/file1.txt`) {
        throw "exists failed for file";
    }

    // Test mv - rename file
    mv(`${BUILD_DIR}/test-dir/file1.txt`, `${BUILD_DIR}/test-dir/renamed.txt`);
    if !file_exists(`${BUILD_DIR}/test-dir/renamed.txt`) {
        throw "mv failed: renamed.txt not found";
    }
    if file_exists(`${BUILD_DIR}/test-dir/file1.txt`) {
        throw "mv failed: file1.txt still exists";
    }

    // Test ln - create symlink
    ln(`${BUILD_DIR}/test-dir/renamed.txt`, `${BUILD_DIR}/test-dir/link.txt`);
    if !exists(`${BUILD_DIR}/test-dir/link.txt`) {
        throw "ln failed: symlink not created";
    }

    // Test chmod
    chmod(`${BUILD_DIR}/test-dir/renamed.txt`, 0o755);

    // Create a binary for install
    run(`echo '#!/bin/sh\necho "test-filesystem"' > ${BUILD_DIR}/test-bin`);
    chmod(`${BUILD_DIR}/test-bin`, 0o755);

    // Test rm - remove file
    rm(`${BUILD_DIR}/test-dir/file2.txt`);
    if file_exists(`${BUILD_DIR}/test-dir/file2.txt`) {
        throw "rm failed: file2.txt still exists";
    }
}

fn install() {
    cd(BUILD_DIR);
    install_bin("test-bin");
}
