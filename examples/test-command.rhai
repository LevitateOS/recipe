// Test recipe: Command and Process helpers
// Tests: run, shell, run_output, run_status, exec, exec_output

let name = "test-command";
let version = "1.0.0";
let description = "Test recipe for command and process helper functions";
let installed = false;

fn acquire() {
    // Nothing to acquire
}

fn build() {
    // Test run - execute shell command
    run("echo 'Testing run command'");

    // Test shell (alias for run)
    shell("echo 'Testing shell command'");

    // Test run_output - capture command output
    let output = run_output("echo hello");
    if !output.contains("hello") {
        throw `run_output failed: expected "hello", got "${output}"`;
    }

    // Test run_output with more complex command
    let pwd_output = run_output("pwd");
    if pwd_output.len() == 0 {
        throw "run_output failed: pwd returned empty";
    }

    // Test run_status - get exit code
    let success_code = run_status("true");
    if success_code != 0 {
        throw `run_status failed: 'true' returned ${success_code}, expected 0`;
    }

    let fail_code = run_status("false");
    if fail_code == 0 {
        throw "run_status failed: 'false' returned 0, expected non-zero";
    }

    // Test exec - run command with args array
    let exit_code = exec("echo", ["hello", "world"]);
    if exit_code != 0 {
        throw `exec failed: exit code ${exit_code}`;
    }

    // Test exec_output - run command and capture output
    let exec_out = exec_output("echo", ["test", "output"]);
    if !exec_out.contains("test output") {
        throw `exec_output failed: expected "test output", got "${exec_out}"`;
    }

    // Create a binary for install
    run(`echo '#!/bin/sh\necho "test-command"' > ${BUILD_DIR}/test-bin`);
    chmod(`${BUILD_DIR}/test-bin`, 0o755);
}

fn install() {
    cd(BUILD_DIR);
    install_bin("test-bin");
}
