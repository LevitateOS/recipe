//! Shell commands with shared checked, status, and captured-output execution.
//! Streaming output goes to stderr; output helpers return stdout to the recipe.

use crate::helpers::internal::cmd;
use rhai::EvalAltResult;
use std::process::Command;

fn command(dir: Option<&str>, script: &str) -> Command {
    let mut command = Command::new("sh");
    command.args(["-c", script]);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    command
}

/// Run a shell command, streaming diagnostics to stderr and throwing on failure.
pub fn shell(script: &str) -> Result<(), Box<EvalAltResult>> {
    cmd::run_checked(command(None, script))
}

/// Run a checked shell command in the given working directory.
pub fn shell_in(dir: &str, script: &str) -> Result<(), Box<EvalAltResult>> {
    cmd::run_checked(command(Some(dir), script))
}

/// Return a shell exit code, or -1 when it could not run (legacy convention).
pub fn shell_status(script: &str) -> i64 {
    cmd::run_status(command(None, script)).unwrap_or(-1)
}

/// Return a shell exit code in the given working directory.
pub fn shell_status_in(dir: &str, script: &str) -> i64 {
    cmd::run_status(command(Some(dir), script)).unwrap_or(-1)
}

fn output(dir: Option<&str>, script: &str) -> Result<String, Box<EvalAltResult>> {
    let output = cmd::checked_capture(command(dir, script))?;
    String::from_utf8(output.stdout).map_err(|error| format!("invalid utf8 output: {error}").into())
}

/// Return stdout from a successful shell command; throw with diagnostics on failure.
pub fn shell_output(script: &str) -> Result<String, Box<EvalAltResult>> {
    output(None, script)
}

/// Capture a checked shell command in the given working directory.
pub fn shell_output_in(dir: &str, script: &str) -> Result<String, Box<EvalAltResult>> {
    output(Some(dir), script)
}
