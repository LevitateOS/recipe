// Test recipe: Acquire helpers
// Tests: download, copy, verify_sha256, extract
// Note: download test requires network access

let name = "test-acquire";
let version = "1.0.0";
let description = "Test recipe for acquire helper functions";
let installed = false;

fn acquire() {
    // Test download - fetch a small known file
    // Using a small text file from httpbin
    download("https://httpbin.org/robots.txt");

    // Test copy - copy files from local path
    // First create some source files
    mkdir(`${BUILD_DIR}/source`);
    run(`echo "file to copy" > ${BUILD_DIR}/source/copyme.txt`);

    // Copy using glob pattern
    copy(`${BUILD_DIR}/source/*.txt`);
}

fn build() {
    // Verify downloaded file exists
    if !file_exists(`${BUILD_DIR}/robots.txt`) {
        throw "download failed: robots.txt not found";
    }

    // Verify copied file exists
    if !file_exists(`${BUILD_DIR}/copyme.txt`) {
        throw "copy failed: copyme.txt not found";
    }

    // Test extract with a real tarball
    // Create a test tarball
    mkdir(`${BUILD_DIR}/tartest`);
    run(`echo "tar content" > ${BUILD_DIR}/tartest/file.txt`);
    cd(BUILD_DIR);
    run(`tar czf test.tar.gz tartest`);
    rm(`${BUILD_DIR}/tartest`);  // Remove original

    // Test extract
    extract("tar.gz");

    // Verify extraction worked
    if !dir_exists(`${BUILD_DIR}/tartest`) {
        throw "extract failed: tartest directory not found";
    }
    if !file_exists(`${BUILD_DIR}/tartest/file.txt`) {
        throw "extract failed: file.txt not found in extracted dir";
    }

    // Create binary for install
    run(`echo '#!/bin/sh\necho "test-acquire"' > ${BUILD_DIR}/test-bin`);
    chmod(`${BUILD_DIR}/test-bin`, 0o755);
}

fn install() {
    cd(BUILD_DIR);
    install_bin("test-bin");
}
