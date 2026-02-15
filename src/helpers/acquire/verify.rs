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
use super::http;

fn is_valid_sha256(s: &str) -> bool {
    s.len() == 64 && s.as_bytes().iter().all(|b| b.is_ascii_hexdigit())
}

fn parse_sha256_from_checksum_file(content: &str, filename: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Format: "SHA256 (Foo.iso) = abc123..."
        if let (Some(open), Some(close)) = (line.find('('), line.find(')'))
            && close > open
        {
            let inside = line[open + 1..close].trim();
            if inside == filename
                && let Some(eq) = line[close + 1..].find('=')
            {
                let after_eq = line[close + 1 + eq + 1..].trim();
                let hash = after_eq.split_whitespace().next().unwrap_or("");
                if is_valid_sha256(hash) {
                    return Some(hash.to_string());
                }
            }
        }

        // Format: "abc123...  Foo.iso" (sha256sum-compatible)
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && is_valid_sha256(parts[0]) {
            let candidate = parts[1].trim_start_matches('*');
            if candidate == filename {
                return Some(parts[0].to_string());
            }
        }
    }
    None
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

/// Fetch a SHA256 checksum from a remote checksum file.
///
/// Supported formats:
/// - `SHA256 (filename) = <hash>`
/// - `<hash>  filename` (sha256sum)
///
/// Throws if no matching checksum is found.
pub fn fetch_sha256(url: &str, filename: &str) -> Result<String, Box<EvalAltResult>> {
    let filename = filename.trim();
    if filename.is_empty() {
        return Err("filename must not be empty".into());
    }
    if filename.contains('\n') || filename.contains('\r') {
        return Err("filename must not contain newlines".into());
    }

    output::detail(&format!("fetching sha256 for {} from {}", filename, url));
    let content = http::http_get(url)?;
    parse_sha256_from_checksum_file(&content, filename).ok_or_else(|| {
        format!(
            "sha256 not found for '{}' in checksum file {}",
            filename, url
        )
        .into()
    })
}

/// Compute all hashes for a file (used by `recipe hash` command)
pub fn compute_hashes(file: &Path) -> Result<hash::FileHashes, std::io::Error> {
    hash::compute_all_hashes(file)
}

/// Re-export FileHashes for backwards compatibility
pub use super::super::internal::hash::FileHashes;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sha256_rocky_style() {
        let content = r#"
# comment
SHA256 (Rocky-10.1-x86_64-dvd1.iso) = 55f96d45a052c0ed4f06309480155cb66281a008691eb7f3f359957205b1849a
SHA256 (Other.iso) = deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef
"#;
        let hash = parse_sha256_from_checksum_file(content, "Rocky-10.1-x86_64-dvd1.iso").unwrap();
        assert_eq!(
            hash,
            "55f96d45a052c0ed4f06309480155cb66281a008691eb7f3f359957205b1849a"
        );
    }

    #[test]
    fn test_parse_sha256_sha256sum_style() {
        let content = r#"
55f96d45a052c0ed4f06309480155cb66281a008691eb7f3f359957205b1849a  Rocky.iso
"#;
        let hash = parse_sha256_from_checksum_file(content, "Rocky.iso").unwrap();
        assert_eq!(
            hash,
            "55f96d45a052c0ed4f06309480155cb66281a008691eb7f3f359957205b1849a"
        );
    }

    #[test]
    fn test_parse_sha256_sha256sum_style_binary_marker() {
        let content = r#"
55f96d45a052c0ed4f06309480155cb66281a008691eb7f3f359957205b1849a *Rocky.iso
"#;
        let hash = parse_sha256_from_checksum_file(content, "Rocky.iso").unwrap();
        assert_eq!(
            hash,
            "55f96d45a052c0ed4f06309480155cb66281a008691eb7f3f359957205b1849a"
        );
    }

    #[test]
    fn test_parse_sha256_missing() {
        let content =
            "SHA256 (Other.iso) = 55f96d45a052c0ed4f06309480155cb66281a008691eb7f3f359957205b1849a";
        assert!(parse_sha256_from_checksum_file(content, "Nope.iso").is_none());
    }
}
