//! Pure shell execution helpers
//!
//! These functions run commands in the current working directory
//! without depending on execution context.
//!
//! IMPORTANT: `shell()`, `shell_in()`, `shell_status()`, and `shell_status_in()`
//! redirect child stdout → stderr so that shell output does not corrupt the
//! JSON context emitted on stdout by the recipe binary.

use rhai::EvalAltResult;
use std::process::{Command, Stdio};

/// Run a shell command in the current directory.
///
/// Throws an error if the command fails.
/// Child stdout and stderr are inherited so build output streams to the terminal.
///
/// # Example
/// ```rhai
/// shell("make -j4");
/// ```
pub fn shell(cmd: &str) -> Result<(), Box<EvalAltResult>> {
    let status = Command::new("sh")
        .args(["-c", cmd])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("command failed to start: {}", e))?;

    if !status.success() {
        return Err(format!(
            "command failed with exit code: {:?}\n  command: {}",
            status.code(),
            cmd
        )
        .into());
    }

    Ok(())
}

/// Run a shell command in a specific directory.
///
/// Throws an error if the command fails.
/// Child stdout is redirected to stderr to protect the JSON output pipe.
///
/// # Example
/// ```rhai
/// shell_in("/tmp/build", "make -j4");
/// ```
pub fn shell_in(dir: &str, cmd: &str) -> Result<(), Box<EvalAltResult>> {
    let status = Command::new("sh")
        .args(["-c", cmd])
        .current_dir(dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("command failed to start: {}", e))?;

    if !status.success() {
        return Err(format!(
            "command failed with exit code: {:?}\n  command: {}\n  in: {}",
            status.code(),
            cmd,
            dir
        )
        .into());
    }

    Ok(())
}

/// Run a shell command and return its exit status code.
///
/// Returns the exit code (0 for success), or -1 if the command couldn't run.
/// Child stdout is redirected to stderr to protect the JSON output pipe.
///
/// # Example
/// ```rhai
/// let code = shell_status("test -f /etc/passwd");
/// if code == 0 {
///     log("file exists");
/// }
/// ```
pub fn shell_status(cmd: &str) -> i64 {
    Command::new("sh")
        .args(["-c", cmd])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::null())
        .status()
        .map(|s| s.code().unwrap_or(-1) as i64)
        .unwrap_or(-1)
}

/// Run a shell command in a specific directory and return its exit status code.
/// Child stdout is redirected to stderr to protect the JSON output pipe.
pub fn shell_status_in(dir: &str, cmd: &str) -> i64 {
    Command::new("sh")
        .args(["-c", cmd])
        .current_dir(dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .stdin(Stdio::null())
        .status()
        .map(|s| s.code().unwrap_or(-1) as i64)
        .unwrap_or(-1)
}

/// Run a shell command and return its stdout output.
///
/// Throws an error if the command fails.
/// NOTE: This captures stdout for the caller — does NOT redirect to stderr.
///
/// # Example
/// ```rhai
/// let output = shell_output("uname -r");
/// log("kernel: " + trim(output));
/// ```
pub fn shell_output(cmd: &str) -> Result<String, Box<EvalAltResult>> {
    let output = Command::new("sh")
        .args(["-c", cmd])
        .output()
        .map_err(|e| format!("command failed to start: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "command failed with exit code: {:?}\n  command: {}",
            output.status.code(),
            cmd
        )
        .into());
    }

    String::from_utf8(output.stdout).map_err(|e| format!("invalid utf8 output: {}", e).into())
}

/// Run a shell command in a specific directory and return its stdout output.
/// NOTE: This captures stdout for the caller — does NOT redirect to stderr.
pub fn shell_output_in(dir: &str, cmd: &str) -> Result<String, Box<EvalAltResult>> {
    let output = Command::new("sh")
        .args(["-c", cmd])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("command failed to start: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "command failed with exit code: {:?}\n  command: {}\n  in: {}",
            output.status.code(),
            cmd,
            dir
        )
        .into());
    }

    String::from_utf8(output.stdout).map_err(|e| format!("invalid utf8 output: {}", e).into())
}
