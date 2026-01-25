//! Acquire phase helpers
//!
//! Get source materials: download, copy, verify.
//!
//! ## Implicit State
//!
//! Both `download()` and `copy()` set `ctx.last_downloaded` to point to the
//! acquired file. This is used by verification functions to know which file to check.
//!
//! ## Example
//!
//! ```rhai
//! fn acquire() {
//!     download("https://example.com/foo-1.0.tar.gz");
//!     verify_sha256("abc123...");  // Verifies the downloaded file
//! }
//! ```
//!
//! ## Supported Hash Algorithms
//!
//! - `verify_sha256(expected)` - SHA-256 (recommended)
//! - `verify_sha512(expected)` - SHA-512 (stronger)
//! - `verify_blake3(expected)` - BLAKE3 (fastest)

use crate::core::{output, with_context, with_context_mut};
use indicatif::{ProgressBar, ProgressStyle};
use rhai::EvalAltResult;
use sha2::{Digest, Sha256, Sha512};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

/// Download a file from a URL with progress bar.
///
/// Downloads to `BUILD_DIR/{filename}` and sets `ctx.last_downloaded` for
/// use with `verify_sha256()`.
///
/// # Example
/// ```rhai
/// download("https://example.com/foo-1.0.tar.gz");
/// verify_sha256("abc123...");
/// ```
pub fn download(url: &str) -> Result<(), Box<EvalAltResult>> {
    with_context_mut(|ctx| {
        let filename = url.rsplit('/').next().unwrap_or("download");
        let dest = ctx.build_dir.join(filename);

        // Create progress bar
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("     {spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
        );
        pb.set_message(format!("downloading {}", filename));
        pb.enable_steady_tick(Duration::from_millis(80));

        // Make the request
        let response = ureq::get(url)
            .call()
            .map_err(|e| format!("download failed: {}", e))?;

        // Get content length if available
        let content_length: Option<u64> = response
            .header("content-length")
            .and_then(|s| s.parse().ok());

        // If we have content length, switch to a progress bar
        if let Some(len) = content_length {
            pb.set_length(len);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template(
                        "     {spinner:.cyan} [{bar:30.cyan/dim}] {bytes}/{total_bytes} ({eta})",
                    )
                    .unwrap()
                    .progress_chars("━╸━"),
            );
        }

        // Create output file
        let mut file =
            std::fs::File::create(&dest).map_err(|e| format!("cannot create file: {}", e))?;

        // Read and write with progress
        let mut reader = response.into_reader();
        let mut buffer = [0u8; 8192];
        let mut total_bytes = 0u64;

        loop {
            let bytes_read = reader
                .read(&mut buffer)
                .map_err(|e| format!("read error: {}", e))?;

            if bytes_read == 0 {
                break;
            }

            file.write_all(&buffer[..bytes_read])
                .map_err(|e| format!("write error: {}", e))?;

            total_bytes += bytes_read as u64;
            pb.set_position(total_bytes);
        }

        pb.finish_and_clear();
        output::detail(&format!("downloaded {} ({} bytes)", filename, total_bytes));

        ctx.last_downloaded = Some(dest);
        Ok(())
    })
}

/// Copy files matching a glob pattern to the build directory
pub fn copy_files(pattern: &str) -> Result<(), Box<EvalAltResult>> {
    with_context_mut(|ctx| {
        output::detail(&format!("copying {}", pattern));

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
            std::fs::copy(src, &dest).map_err(|e| {
                format!(
                    "copy failed: {} -> {}: {}",
                    src.display(),
                    dest.display(),
                    e
                )
            })?;
            ctx.last_downloaded = Some(dest);
        }

        Ok(())
    })
}

/// Verify the SHA256 hash of the last downloaded/copied file
pub fn verify_sha256(expected: &str) -> Result<(), Box<EvalAltResult>> {
    with_context(|ctx| {
        let file = ctx
            .last_downloaded
            .as_ref()
            .ok_or("No file to verify - call download() or copy() first")?;

        output::detail("verifying sha256");
        verify_hash_sha256(file, expected)
    })
}

/// Verify the SHA512 hash of the last downloaded/copied file
pub fn verify_sha512(expected: &str) -> Result<(), Box<EvalAltResult>> {
    with_context(|ctx| {
        let file = ctx
            .last_downloaded
            .as_ref()
            .ok_or("No file to verify - call download() or copy() first")?;

        output::detail("verifying sha512");
        verify_hash_sha512(file, expected)
    })
}

/// Verify the BLAKE3 hash of the last downloaded/copied file
pub fn verify_blake3(expected: &str) -> Result<(), Box<EvalAltResult>> {
    with_context(|ctx| {
        let file = ctx
            .last_downloaded
            .as_ref()
            .ok_or("No file to verify - call download() or copy() first")?;

        output::detail("verifying blake3");
        verify_hash_blake3(file, expected)
    })
}

/// Internal: Verify SHA256 hash of a specific file
///
/// Shows progress for files larger than 100 MB.
pub fn verify_hash_sha256(file: &Path, expected: &str) -> Result<(), Box<EvalAltResult>> {
    let mut f = std::fs::File::open(file).map_err(|e| format!("cannot open file: {}", e))?;
    let file_size = f
        .metadata()
        .map(|m| m.len())
        .unwrap_or(0);

    let mut hasher = Sha256::new();
    let mut buffer = [0; 1024 * 1024]; // 1MB chunks for better progress
    let mut total_read = 0u64;
    let mut last_percent = 0u8;

    // Show progress for large files (> 100 MB)
    let show_progress = file_size > 100 * 1024 * 1024;

    loop {
        let n = f
            .read(&mut buffer)
            .map_err(|e| format!("read error: {}", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        total_read += n as u64;

        if show_progress && file_size > 0 {
            let percent = ((total_read * 100) / file_size) as u8;
            if percent >= last_percent + 10 {
                print!("\r     checksum: {}%...", percent);
                std::io::Write::flush(&mut std::io::stdout()).ok();
                last_percent = percent;
            }
        }
    }

    if show_progress {
        println!();
    }

    let hash = hex::encode(hasher.finalize());

    if hash != expected.to_lowercase() {
        return Err(format!(
            "SHA256 integrity check failed for '{}'\n  expected: {}\n  got:      {}",
            file.display(),
            expected.to_lowercase(),
            hash
        )
        .into());
    }

    Ok(())
}

/// Internal: Verify SHA512 hash of a specific file
///
/// Shows progress for files larger than 100 MB.
pub fn verify_hash_sha512(file: &Path, expected: &str) -> Result<(), Box<EvalAltResult>> {
    let mut f = std::fs::File::open(file).map_err(|e| format!("cannot open file: {}", e))?;
    let file_size = f
        .metadata()
        .map(|m| m.len())
        .unwrap_or(0);

    let mut hasher = Sha512::new();
    let mut buffer = [0; 1024 * 1024]; // 1MB chunks for better progress
    let mut total_read = 0u64;
    let mut last_percent = 0u8;

    // Show progress for large files (> 100 MB)
    let show_progress = file_size > 100 * 1024 * 1024;

    loop {
        let n = f
            .read(&mut buffer)
            .map_err(|e| format!("read error: {}", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        total_read += n as u64;

        if show_progress && file_size > 0 {
            let percent = ((total_read * 100) / file_size) as u8;
            if percent >= last_percent + 10 {
                print!("\r     checksum: {}%...", percent);
                std::io::Write::flush(&mut std::io::stdout()).ok();
                last_percent = percent;
            }
        }
    }

    if show_progress {
        println!();
    }

    let hash = hex::encode(hasher.finalize());

    if hash != expected.to_lowercase() {
        return Err(format!(
            "SHA512 integrity check failed for '{}'\n  expected: {}\n  got:      {}",
            file.display(),
            expected.to_lowercase(),
            hash
        )
        .into());
    }

    Ok(())
}

/// Internal: Verify BLAKE3 hash of a specific file
///
/// Shows progress for files larger than 100 MB.
pub fn verify_hash_blake3(file: &Path, expected: &str) -> Result<(), Box<EvalAltResult>> {
    let mut f = std::fs::File::open(file).map_err(|e| format!("cannot open file: {}", e))?;
    let file_size = f
        .metadata()
        .map(|m| m.len())
        .unwrap_or(0);

    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0; 1024 * 1024]; // 1MB chunks for better progress
    let mut total_read = 0u64;
    let mut last_percent = 0u8;

    // Show progress for large files (> 100 MB)
    let show_progress = file_size > 100 * 1024 * 1024;

    loop {
        let n = f
            .read(&mut buffer)
            .map_err(|e| format!("read error: {}", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        total_read += n as u64;

        if show_progress && file_size > 0 {
            let percent = ((total_read * 100) / file_size) as u8;
            if percent >= last_percent + 10 {
                print!("\r     checksum: {}%...", percent);
                std::io::Write::flush(&mut std::io::stdout()).ok();
                last_percent = percent;
            }
        }
    }

    if show_progress {
        println!();
    }

    let hash = hasher.finalize().to_hex().to_string();

    if hash != expected.to_lowercase() {
        return Err(format!(
            "BLAKE3 integrity check failed for '{}'\n  expected: {}\n  got:      {}",
            file.display(),
            expected.to_lowercase(),
            hash
        )
        .into());
    }

    Ok(())
}

/// Compute all hashes for a file (used by `recipe hash` command)
pub fn compute_hashes(file: &Path) -> Result<FileHashes, std::io::Error> {
    let mut f = std::fs::File::open(file)?;
    let mut sha256_hasher = Sha256::new();
    let mut sha512_hasher = Sha512::new();
    let mut blake3_hasher = blake3::Hasher::new();
    let mut buffer = [0; 8192];

    loop {
        let n = f.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        sha256_hasher.update(&buffer[..n]);
        sha512_hasher.update(&buffer[..n]);
        blake3_hasher.update(&buffer[..n]);
    }

    Ok(FileHashes {
        sha256: hex::encode(sha256_hasher.finalize()),
        sha512: hex::encode(sha512_hasher.finalize()),
        blake3: blake3_hasher.finalize().to_hex().to_string(),
    })
}

/// Container for computed file hashes
#[derive(Debug, Clone)]
pub struct FileHashes {
    pub sha256: String,
    pub sha512: String,
    pub blake3: String,
}

// ============================================================================
// Pure helpers (no context dependency)
// ============================================================================

/// Download a file from a URL to a specific destination (pure version).
///
/// Returns the path to the downloaded file on success, empty string on failure.
///
/// # Example
/// ```rhai
/// let path = download("https://example.com/foo.tar.gz", "/tmp/foo.tar.gz");
/// if path != "" {
///     verify_sha256(path, "abc123...");
/// }
/// ```
pub fn download_to(url: &str, dest: &str) -> Result<String, Box<EvalAltResult>> {
    let dest_path = Path::new(dest);

    // Create parent directory if needed
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create directory: {}", e))?;
    }

    let filename = dest_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".to_string());

    // Create progress bar
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("     {spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.set_message(format!("downloading {}", filename));
    pb.enable_steady_tick(Duration::from_millis(80));

    // Make the request
    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("download failed: {}", e))?;

    // Get content length if available
    let content_length: Option<u64> = response
        .header("content-length")
        .and_then(|s| s.parse().ok());

    // If we have content length, switch to a progress bar
    if let Some(len) = content_length {
        pb.set_length(len);
        pb.set_style(
            ProgressStyle::default_bar()
                .template(
                    "     {spinner:.cyan} [{bar:30.cyan/dim}] {bytes}/{total_bytes} ({eta})",
                )
                .unwrap()
                .progress_chars("━╸━"),
        );
    }

    // Create output file
    let mut file =
        std::fs::File::create(dest_path).map_err(|e| format!("cannot create file: {}", e))?;

    // Read and write with progress
    let mut reader = response.into_reader();
    let mut buffer = [0u8; 8192];
    let mut total_bytes = 0u64;

    loop {
        let bytes_read = reader
            .read(&mut buffer)
            .map_err(|e| format!("read error: {}", e))?;

        if bytes_read == 0 {
            break;
        }

        file.write_all(&buffer[..bytes_read])
            .map_err(|e| format!("write error: {}", e))?;

        total_bytes += bytes_read as u64;
        pb.set_position(total_bytes);
    }

    pb.finish_and_clear();
    output::detail(&format!("downloaded {} ({} bytes)", filename, total_bytes));

    Ok(dest.to_string())
}

/// Verify the SHA256 hash of a specific file (pure version).
///
/// # Example
/// ```rhai
/// verify_sha256("/tmp/foo.tar.gz", "abc123...");
/// ```
pub fn verify_sha256_file(path: &str, expected: &str) -> Result<(), Box<EvalAltResult>> {
    output::detail(&format!("verifying sha256 of {}", path));
    verify_hash_sha256(Path::new(path), expected)
}

/// Verify the SHA512 hash of a specific file (pure version).
pub fn verify_sha512_file(path: &str, expected: &str) -> Result<(), Box<EvalAltResult>> {
    output::detail(&format!("verifying sha512 of {}", path));
    verify_hash_sha512(Path::new(path), expected)
}

/// Verify the BLAKE3 hash of a specific file (pure version).
pub fn verify_blake3_file(path: &str, expected: &str) -> Result<(), Box<EvalAltResult>> {
    output::detail(&format!("verifying blake3 of {}", path));
    verify_hash_blake3(Path::new(path), expected)
}
