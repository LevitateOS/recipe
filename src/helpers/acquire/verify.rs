//! Hash verification helpers
//!
//! Pure functions for verifying file integrity using cryptographic hashes.
//!
//! ## Supported Hash Algorithms
//!
//! - `verify_sha256(path, expected)` - SHA-256 (recommended)
//! - `verify_sha512(path, expected)` - SHA-512 (stronger)
//! - `verify_blake3(path, expected)` - BLAKE3 (fastest)
//!
//! ## Example
//!
//! ```rhai
//! let archive = download(url, BUILD_DIR + "/foo.tar.gz");
//! verify_sha256(archive, "abc123...");
//! ```

use crate::core::output;
use rhai::EvalAltResult;
use std::path::Path;

use super::super::internal::hash::{self, HashAlgorithm};

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
pub use super::super::internal::hash::FileHashes;
