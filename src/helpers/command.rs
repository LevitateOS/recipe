//! Command execution helpers

use crate::core::{CONTEXT, with_context};
use rhai::EvalAltResult;
use std::process::Command;

/// Run a command and return its stdout
pub fn run_output(cmd: &str) -> Result<String, Box<EvalAltResult>> {
    with_context(|ctx| {
        let output = Command::new("sh")
            .args(["-c", cmd])
            .current_dir(&ctx.current_dir)
            .env("PREFIX", &ctx.prefix)
            .env("BUILD_DIR", &ctx.build_dir)
            .output()
            .map_err(|e| format!("command failed: {}", e))?;

        if !output.status.success() {
            return Err(
                format!("command failed with exit code: {:?}", output.status.code()).into(),
            );
        }

        String::from_utf8(output.stdout).map_err(|e| format!("invalid utf8: {}", e).into())
    })
}

/// Run a command and return its exit status code
pub fn run_status(cmd: &str) -> i64 {
    CONTEXT.with(|c| {
        let ctx = c.borrow();
        if let Some(ctx) = ctx.as_ref() {
            Command::new("sh")
                .args(["-c", cmd])
                .current_dir(&ctx.current_dir)
                .env("PREFIX", &ctx.prefix)
                .env("BUILD_DIR", &ctx.build_dir)
                .status()
                .map(|s| s.code().unwrap_or(-1) as i64)
                .unwrap_or(-1)
        } else {
            -1
        }
    })
}
