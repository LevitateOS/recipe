// Test recipe: HTTP helpers
// Tests: http_get, github_latest_release, github_latest_tag, parse_version
// Note: These tests require network access

let name = "test-http";
let version = "1.0.0";
let description = "Test recipe for HTTP helper functions";
let installed = false;

fn acquire() {
    // Nothing to acquire - we test HTTP functions in build phase
}

fn build() {
    // Test parse_version - this doesn't need network
    let v1 = parse_version("v1.2.3");
    if v1 != "1.2.3" {
        throw `parse_version failed: "v1.2.3" -> "${v1}", expected "1.2.3"`;
    }

    let v2 = parse_version("release-2.0.0");
    if v2 != "2.0.0" {
        throw `parse_version failed: "release-2.0.0" -> "${v2}", expected "2.0.0"`;
    }

    let v3 = parse_version("version-3.1.4");
    if v3 != "3.1.4" {
        throw `parse_version failed: "version-3.1.4" -> "${v3}", expected "3.1.4"`;
    }

    let v4 = parse_version("4.0.0");
    if v4 != "4.0.0" {
        throw `parse_version failed: "4.0.0" -> "${v4}", expected "4.0.0"`;
    }

    // Test http_get - fetch a simple URL
    // Using httpbin.org which is a reliable test endpoint
    let response = http_get("https://httpbin.org/get");
    if !response.contains("httpbin") {
        throw `http_get failed: response doesn't contain "httpbin"`;
    }

    // Test github_latest_release - get latest release from a known repo
    let rg_version = github_latest_release("BurntSushi/ripgrep");
    if rg_version.len() == 0 {
        throw "github_latest_release failed: empty version for ripgrep";
    }
    // Version should start with a digit (after v prefix is stripped)
    let first_char = rg_version.sub_string(0, 1);
    if first_char < "0" || first_char > "9" {
        throw `github_latest_release failed: unexpected version format "${rg_version}"`;
    }

    // Test github_latest_tag - get latest tag from a known repo
    let linux_tag = github_latest_tag("torvalds/linux");
    if linux_tag.len() == 0 {
        throw "github_latest_tag failed: empty tag for linux";
    }

    // Create a binary for install
    run(`echo '#!/bin/sh\necho "test-http"' > ${BUILD_DIR}/test-bin`);
    chmod(`${BUILD_DIR}/test-bin`, 0o755);
}

fn install() {
    cd(BUILD_DIR);
    install_bin("test-bin");
}

fn check_update() {
    // Use github_latest_release to check for updates
    let latest = github_latest_release("BurntSushi/ripgrep");
    if latest != version {
        return latest;
    }
}
