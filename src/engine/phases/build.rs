//! Build phase helpers
//!
//! Compile/transform source: extract, cd, run

use crate::engine::context::{with_context, with_context_mut};
use rhai::EvalAltResult;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Extract an archive
pub fn extract(format: &str) -> Result<(), Box<EvalAltResult>> {
    with_context(|ctx| {
        let file = ctx
            .last_downloaded
            .as_ref()
            .ok_or("No file to extract - call download() or copy() first")?;

        println!("     extracting {}", file.display());

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
            _ => return Err(format!("unknown archive format: {}", format).into()),
        };

        let status = status.map_err(|e| format!("extract failed: {}", e))?;
        if !status.success() {
            return Err("extraction failed".to_string().into());
        }

        Ok(())
    })
}

/// Change the current working directory
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

        println!("     cd {}", dir);
        ctx.current_dir = new_dir;
        Ok(())
    })
}

/// Run a shell command
pub fn run_cmd(cmd: &str) -> Result<(), Box<EvalAltResult>> {
    with_context(|ctx| {
        println!("     run: {}", cmd);

        let status = Command::new("sh")
            .args(["-c", cmd])
            .current_dir(&ctx.current_dir)
            .env("PREFIX", &ctx.prefix)
            .env("BUILD_DIR", &ctx.build_dir)
            .status()
            .map_err(|e| format!("command failed to start: {}", e))?;

        if !status.success() {
            return Err(format!("command failed with exit code: {:?}", status.code()).into());
        }

        Ok(())
    })
}
