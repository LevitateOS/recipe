//! Acquire phase helpers
//!
//! Get source materials: download, copy, verify

use crate::engine::context::with_context_mut;
use rhai::EvalAltResult;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::process::Command;

/// Download a file from a URL
pub fn download(url: &str) -> Result<(), Box<EvalAltResult>> {
    with_context_mut(|ctx| {
        let filename = url.rsplit('/').next().unwrap_or("download");
        let dest = ctx.build_dir.join(filename);

        println!("     downloading {}", url);

        let status = Command::new("curl")
            .args(["-fsSL", "-o", &dest.to_string_lossy(), url])
            .status()
            .map_err(|e| format!("curl failed: {}", e))?;

        if !status.success() {
            return Err(format!("download failed: {}", url).into());
        }

        ctx.last_downloaded = Some(dest);
        Ok(())
    })
}

/// Copy files matching a glob pattern to the build directory
pub fn copy_files(pattern: &str) -> Result<(), Box<EvalAltResult>> {
    with_context_mut(|ctx| {
        println!("     copying {}", pattern);

        // Expand glob pattern
        let matches: Vec<_> = glob::glob(pattern)
            .map_err(|e| format!("invalid pattern: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        if matches.is_empty() {
            return Err(format!("no files match pattern: {}", pattern).into());
        }

        for src in &matches {
            let filename = src.file_name().ok_or("invalid filename")?;
            let dest = ctx.build_dir.join(filename);
            std::fs::copy(src, &dest)
                .map_err(|e| format!("copy failed: {} -> {}: {}", src.display(), dest.display(), e))?;
            ctx.last_downloaded = Some(dest);
        }

        Ok(())
    })
}

/// Verify the SHA256 hash of the last downloaded/copied file
pub fn verify_sha256(expected: &str) -> Result<(), Box<EvalAltResult>> {
    crate::engine::context::with_context(|ctx| {
        let file = ctx
            .last_downloaded
            .as_ref()
            .ok_or("No file to verify - call download() or copy() first")?;

        println!("     verifying sha256");

        let mut f = std::fs::File::open(file).map_err(|e| format!("cannot open file: {}", e))?;
        let mut hasher = Sha256::new();
        let mut buffer = [0; 8192];
        loop {
            let n = f.read(&mut buffer).map_err(|e| format!("read error: {}", e))?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        let hash = hex::encode(hasher.finalize());

        if hash != expected.to_lowercase() {
            return Err(format!("sha256 mismatch: expected {}, got {}", expected, hash).into());
        }

        Ok(())
    })
}
