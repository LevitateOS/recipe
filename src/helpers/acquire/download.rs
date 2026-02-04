//! Download helpers for acquiring files
//!
//! Pure functions for downloading files from URLs.
//! All functions take explicit inputs and return explicit outputs.
//!
//! ## Example
//!
//! ```rhai
//! fn acquire(ctx) {
//!     let archive = download(ctx.url, BUILD_DIR + "/foo.tar.gz");
//!     if archive == "" {
//!         throw "download failed";
//!     }
//!     verify_sha256(archive, ctx.sha256);
//!     ctx.archive_path = archive;
//!     ctx
//! }
//! ```

use crate::core::output;
use rhai::EvalAltResult;
use std::io::{Read, Write};
use std::path::Path;

use super::super::internal::fs_utils;
use super::super::internal::progress::{self, upgrade_to_bytes};

/// Download a file from a URL to a specific destination.
///
/// Returns the path to the downloaded file on success, or empty string on failure.
///
/// # Example
/// ```rhai
/// let path = download("https://example.com/foo.tar.gz", "/tmp/foo.tar.gz");
/// if path == "" {
///     throw "download failed";
/// }
/// verify_sha256(path, "abc123...");
/// ```
pub fn download(url: &str, dest: &str) -> Result<String, Box<EvalAltResult>> {
    let dest_path = Path::new(dest);
    fs_utils::ensure_parent_dir(dest_path)?;

    let filename = dest_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".to_string());

    let total_bytes = download_with_progress(url, dest_path, &filename)?;
    output::detail(&format!("downloaded {} ({} bytes)", filename, total_bytes));

    Ok(dest.to_string())
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Download a file with progress bar (shared implementation)
fn download_with_progress(
    url: &str,
    dest: &Path,
    filename: &str,
) -> Result<u64, Box<EvalAltResult>> {
    let pb = progress::create_spinner(&format!("downloading {}", filename));

    // Make the request
    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("download failed: {}", e))?;

    // Get content length if available and upgrade progress bar
    if let Some(len) = response
        .header("content-length")
        .and_then(|s| s.parse().ok())
    {
        upgrade_to_bytes(&pb, len);
    }

    // Create output file
    let mut file = std::fs::File::create(dest).map_err(|e| format!("cannot create file: {}", e))?;

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
    Ok(total_bytes)
}
