//! Unified shell command execution
//!
//! Provides a builder pattern for running shell commands with consistent
//! error handling and environment setup.

use rhai::EvalAltResult;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Builder for shell command execution.
///
/// # Example
/// ```ignore
/// ShellCmd::new("make -j4")
///     .dir("/tmp/build")
///     .env("PREFIX", "/usr/local")
///     .run()?;
/// ```
#[derive(Clone)]
pub struct ShellCmd {
    cmd: String,
    cwd: Option<PathBuf>,
    env: HashMap<String, String>,
}

impl ShellCmd {
    /// Create a new shell command.
    pub fn new(cmd: impl Into<String>) -> Self {
        Self {
            cmd: cmd.into(),
            cwd: None,
            env: HashMap::new(),
        }
    }

    /// Set the working directory for the command.
    pub fn dir(mut self, dir: impl AsRef<Path>) -> Self {
        self.cwd = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Set an environment variable for the command.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set multiple environment variables at once.
    pub fn envs<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        for (k, v) in vars {
            self.env.insert(k.into(), v.into());
        }
        self
    }

    /// Build the underlying Command object.
    fn build_command(&self) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &self.cmd]);

        if let Some(ref cwd) = self.cwd {
            cmd.current_dir(cwd);
        }

        for (k, v) in &self.env {
            cmd.env(k, v);
        }

        cmd
    }

    /// Run the command and return success/failure.
    ///
    /// Returns `Ok(())` if exit code is 0, error otherwise.
    pub fn run(&self) -> Result<(), Box<EvalAltResult>> {
        let status = self
            .build_command()
            .status()
            .map_err(|e| format!("command failed to start: {}", e))?;

        if !status.success() {
            return Err(format!(
                "command failed with exit code: {:?}\n  command: {}",
                status.code(),
                self.truncated_cmd()
            )
            .into());
        }

        Ok(())
    }

    /// Run the command and return the exit status code.
    ///
    /// Returns -1 if the command couldn't be started.
    pub fn status(&self) -> i64 {
        self.build_command()
            .status()
            .map(|s| s.code().unwrap_or(-1) as i64)
            .unwrap_or(-1)
    }

    /// Run the command and capture stdout.
    ///
    /// Returns error if command fails (non-zero exit).
    pub fn output(&self) -> Result<String, Box<EvalAltResult>> {
        let output = self
            .build_command()
            .output()
            .map_err(|e| format!("command failed to start: {}", e))?;

        if !output.status.success() {
            return Err(format!(
                "command failed with exit code: {:?}\n  command: {}",
                output.status.code(),
                self.truncated_cmd()
            )
            .into());
        }

        String::from_utf8(output.stdout)
            .map_err(|e| format!("invalid utf8 output: {}", e).into())
    }

    /// Run the command and capture both stdout and stderr.
    pub fn output_all(&self) -> Result<CmdOutput, Box<EvalAltResult>> {
        let output = self
            .build_command()
            .output()
            .map_err(|e| format!("command failed to start: {}", e))?;

        Ok(CmdOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
            success: output.status.success(),
        })
    }

    /// Get a truncated version of the command for display.
    fn truncated_cmd(&self) -> String {
        if self.cmd.len() > 60 {
            format!("{}...", &self.cmd[..57])
        } else {
            self.cmd.clone()
        }
    }

    /// Get the full command string.
    pub fn cmd(&self) -> &str {
        &self.cmd
    }

    /// Get the truncated command string for display.
    pub fn display_cmd(&self) -> String {
        self.truncated_cmd()
    }
}

/// Output from a command execution.
#[derive(Debug, Clone)]
pub struct CmdOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub success: bool,
}

/// Convenience function to run a simple command.
pub fn run(cmd: &str) -> Result<(), Box<EvalAltResult>> {
    ShellCmd::new(cmd).run()
}

/// Convenience function to run a command in a directory.
pub fn run_in(dir: &str, cmd: &str) -> Result<(), Box<EvalAltResult>> {
    ShellCmd::new(cmd).dir(dir).run()
}

/// Convenience function to get command status.
pub fn status(cmd: &str) -> i64 {
    ShellCmd::new(cmd).status()
}

/// Convenience function to get command output.
pub fn output(cmd: &str) -> Result<String, Box<EvalAltResult>> {
    ShellCmd::new(cmd).output()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_command() {
        let result = ShellCmd::new("echo hello").run();
        assert!(result.is_ok());
    }

    #[test]
    fn test_command_with_dir() {
        let result = ShellCmd::new("pwd").dir("/tmp").output().unwrap();
        assert!(result.trim() == "/tmp" || result.contains("tmp"));
    }

    #[test]
    fn test_command_with_env() {
        let result = ShellCmd::new("echo $MY_VAR")
            .env("MY_VAR", "hello_world")
            .output()
            .unwrap();
        assert_eq!(result.trim(), "hello_world");
    }

    #[test]
    fn test_command_status() {
        assert_eq!(ShellCmd::new("true").status(), 0);
        assert_eq!(ShellCmd::new("false").status(), 1);
    }

    #[test]
    fn test_command_failure() {
        let result = ShellCmd::new("exit 42").run();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("42"));
    }

    #[test]
    fn test_output_all() {
        let result = ShellCmd::new("echo stdout; echo stderr >&2")
            .output_all()
            .unwrap();
        assert!(result.stdout.contains("stdout"));
        assert!(result.stderr.contains("stderr"));
        assert!(result.success);
    }

    #[test]
    fn test_truncated_cmd() {
        let short = ShellCmd::new("echo hi");
        assert_eq!(short.display_cmd(), "echo hi");

        let long = ShellCmd::new("a".repeat(100));
        assert!(long.display_cmd().len() <= 60);
        assert!(long.display_cmd().ends_with("..."));
    }

    #[test]
    fn test_convenience_functions() {
        assert!(run("true").is_ok());
        assert_eq!(status("true"), 0);
        assert!(output("echo test").unwrap().contains("test"));
    }
}
