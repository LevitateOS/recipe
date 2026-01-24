//! Build phase helpers
//!
//! Compile/transform source: extract, cd, run.
//!
//! ## Implicit State
//!
//! - `extract()` uses `ctx.last_downloaded` (set by download/copy)
//! - `cd()` updates `ctx.current_dir` for subsequent commands
//! - `run()` executes in `ctx.current_dir` with `PREFIX` and `BUILD_DIR` env vars
//!
//! ## Example
//!
//! ```rhai
//! fn build() {
//!     extract("tar.gz");           // Extracts last_downloaded to BUILD_DIR
//!     cd("foo-1.0");               // Changes to extracted directory
//!     run("./configure --prefix=$PREFIX");  // PREFIX env var is set
//!     run("make -j4");
//! }
//! ```

use crate::core::{output, with_context, with_context_mut};
use indicatif::{ProgressBar, ProgressStyle};
use rhai::EvalAltResult;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Extract an archive with spinner.
///
/// Extracts `ctx.last_downloaded` (set by download/copy) to `BUILD_DIR`.
/// Supports: tar.gz, tar.xz, tar.bz2, zip.
pub fn extract(format: &str) -> Result<(), Box<EvalAltResult>> {
    with_context(|ctx| {
        let file = ctx
            .last_downloaded
            .as_ref()
            .ok_or("No file to extract - call download() or copy() first")?;

        let filename = file
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "archive".to_string());

        // Create spinner for extraction
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("     {spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
        );
        pb.set_message(format!("extracting {}", filename));
        pb.enable_steady_tick(Duration::from_millis(80));

        let status = match format.to_lowercase().as_str() {
            "tar.gz" | "tgz" => Command::new("tar")
                .args(["xzf", &file.to_string_lossy()])
                .current_dir(&ctx.build_dir)
                .status(),
            "tar.xz" | "txz" => Command::new("tar")
                .args(["xJf", &file.to_string_lossy()])
                .current_dir(&ctx.build_dir)
                .status(),
            "tar.bz2" | "tbz2" => Command::new("tar")
                .args(["xjf", &file.to_string_lossy()])
                .current_dir(&ctx.build_dir)
                .status(),
            "zip" => Command::new("unzip")
                .args(["-q", &file.to_string_lossy()])
                .current_dir(&ctx.build_dir)
                .status(),
            _ => {
                pb.finish_and_clear();
                return Err(format!("unknown archive format: {}", format).into());
            }
        };

        pb.finish_and_clear();

        let status = status.map_err(|e| format!("extract failed: {}", e))?;
        if !status.success() {
            return Err("extraction failed".to_string().into());
        }

        output::detail(&format!("extracted {}", filename));
        Ok(())
    })
}

/// Change the current working directory.
///
/// Updates `ctx.current_dir` which affects subsequent `run()` calls.
/// Relative paths are resolved from `BUILD_DIR`.
///
/// # Example
/// ```rhai
/// cd("foo-1.0");  // Now in BUILD_DIR/foo-1.0
/// run("make");    // Runs in that directory
/// ```
pub fn change_dir(dir: &str) -> Result<(), Box<EvalAltResult>> {
    with_context_mut(|ctx| {
        let new_dir = if Path::new(dir).is_absolute() {
            PathBuf::from(dir)
        } else {
            ctx.build_dir.join(dir)
        };

        if !new_dir.exists() {
            return Err(format!("directory does not exist: {}", new_dir.display()).into());
        }

        output::detail(&format!("cd {}", dir));
        ctx.current_dir = new_dir;
        Ok(())
    })
}

/// Run a shell command with spinner for long-running commands.
///
/// Executes in `ctx.current_dir` (set by `cd()`).
///
/// ## Environment Variables
/// These are automatically set:
/// - `PREFIX` - Installation prefix (e.g., `/usr/local`)
/// - `BUILD_DIR` - Temporary build directory
///
/// # Example
/// ```rhai
/// run("./configure --prefix=$PREFIX");  // PREFIX is available
/// run("make -j4");
/// run("make install");
/// ```
pub fn run_cmd(cmd: &str) -> Result<(), Box<EvalAltResult>> {
    with_context(|ctx| {
        // Truncate long commands for display
        let display_cmd = if cmd.len() > 60 {
            format!("{}...", &cmd[..57])
        } else {
            cmd.to_string()
        };

        // Create spinner for commands that might take a while
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("     {spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
        );
        pb.set_message(format!("run: {}", display_cmd));
        pb.enable_steady_tick(Duration::from_millis(80));

        let status = Command::new("sh")
            .args(["-c", cmd])
            .current_dir(&ctx.current_dir)
            .env("PREFIX", &ctx.prefix)
            .env("BUILD_DIR", &ctx.build_dir)
            .status()
            .map_err(|e| {
                pb.finish_and_clear();
                format!("command failed to start: {}", e)
            })?;

        pb.finish_and_clear();

        if !status.success() {
            output::detail(&format!("run: {} [FAILED]", display_cmd));
            return Err(format!("command failed with exit code: {:?}", status.code()).into());
        }

        output::detail(&format!("run: {}", display_cmd));
        Ok(())
    })
}
