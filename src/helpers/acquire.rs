//! Acquire phase helpers
//!
//! Pure functions for downloading and verifying files.
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
//!
//! ## Supported Hash Algorithms
//!
//! - `verify_sha256(path, expected)` - SHA-256 (recommended)
//! - `verify_sha512(path, expected)` - SHA-512 (stronger)
//! - `verify_blake3(path, expected)` - BLAKE3 (fastest)

use crate::core::output;
use rhai::EvalAltResult;
use std::io::{Read, Write};
use std::path::Path;

use super::fs_utils;
use super::hash::{self, HashAlgorithm};
use super::progress::{self, upgrade_to_bytes};

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

/// Verify the SHA256 hash of a file.
///
/// Throws an error if the hash doesn't match.
///
/// # Example
/// ```rhai
/// verify_sha256("/tmp/foo.tar.gz", "abc123...");
/// ```
pub fn verify_sha256(path: &str, expected: &str) -> Result<(), Box<EvalAltResult>> {
    output::detail(&format!("verifying sha256 of {}", path));
    hash::verify_file_hash(Path::new(path), expected, HashAlgorithm::Sha256)
}

/// Verify the SHA512 hash of a file.
///
/// Throws an error if the hash doesn't match.
pub fn verify_sha512(path: &str, expected: &str) -> Result<(), Box<EvalAltResult>> {
    output::detail(&format!("verifying sha512 of {}", path));
    hash::verify_file_hash(Path::new(path), expected, HashAlgorithm::Sha512)
}

/// Verify the BLAKE3 hash of a file.
///
/// Throws an error if the hash doesn't match.
pub fn verify_blake3(path: &str, expected: &str) -> Result<(), Box<EvalAltResult>> {
    output::detail(&format!("verifying blake3 of {}", path));
    hash::verify_file_hash(Path::new(path), expected, HashAlgorithm::Blake3)
}

/// Compute all hashes for a file (used by `recipe hash` command)
pub fn compute_hashes(file: &Path) -> Result<hash::FileHashes, std::io::Error> {
    hash::compute_all_hashes(file)
}

/// Re-export FileHashes for backwards compatibility
pub use hash::FileHashes;

// ============================================================================
// Internal helpers
// ============================================================================

/// Download a file with progress bar (shared implementation)
fn download_with_progress(url: &str, dest: &Path, filename: &str) -> Result<u64, Box<EvalAltResult>> {
    let pb = progress::create_spinner(&format!("downloading {}", filename));

    // Make the request
    let response = ureq::get(url)
        .call()
        .map_err(|e| format!("download failed: {}", e))?;

    // Get content length if available and upgrade progress bar
    if let Some(len) = response.header("content-length").and_then(|s| s.parse().ok()) {
        upgrade_to_bytes(&pb, len);
    }

    // Create output file
    let mut file =
        std::fs::File::create(dest).map_err(|e| format!("cannot create file: {}", e))?;

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
