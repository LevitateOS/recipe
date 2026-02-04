//! Generic hash verification helpers
//!
//! Provides unified hash verification for SHA256, SHA512, and BLAKE3.

use rhai::EvalAltResult;
use std::io::Read;
use std::path::Path;

/// Chunk size for reading files during hashing (1MB)
const CHUNK_SIZE: usize = 1024 * 1024;

/// Threshold for showing progress (100MB)
const PROGRESS_THRESHOLD: u64 = 100 * 1024 * 1024;

/// Supported hash algorithms
#[derive(Debug, Clone, Copy)]
pub enum HashAlgorithm {
    Sha256,
    Sha512,
    Blake3,
}

impl HashAlgorithm {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Sha256 => "SHA256",
            Self::Sha512 => "SHA512",
            Self::Blake3 => "BLAKE3",
        }
    }
}

/// Verify a file's hash against an expected value.
///
/// Shows progress for files larger than 100MB.
///
/// # Example
/// ```ignore
/// verify_file_hash(Path::new("/tmp/foo.tar.gz"), "abc123...", HashAlgorithm::Sha256)?;
/// ```
pub fn verify_file_hash(
    file: &Path,
    expected: &str,
    algorithm: HashAlgorithm,
) -> Result<(), Box<EvalAltResult>> {
    let mut f = std::fs::File::open(file).map_err(|e| format!("cannot open file: {}", e))?;

    let file_size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let show_progress = file_size > PROGRESS_THRESHOLD;

    let hash = match algorithm {
        HashAlgorithm::Sha256 => {
            hash_with_progress::<sha2::Sha256>(&mut f, file_size, show_progress)?
        }
        HashAlgorithm::Sha512 => {
            hash_with_progress::<sha2::Sha512>(&mut f, file_size, show_progress)?
        }
        HashAlgorithm::Blake3 => hash_blake3_with_progress(&mut f, file_size, show_progress)?,
    };

    if hash != expected.to_lowercase() {
        return Err(format!(
            "{} integrity check failed for '{}'\n  expected: {}\n  got:      {}",
            algorithm.name(),
            file.display(),
            expected.to_lowercase(),
            hash
        )
        .into());
    }

    Ok(())
}

/// Compute hash using sha2 crate (SHA256/SHA512)
fn hash_with_progress<D: sha2::Digest>(
    reader: &mut impl Read,
    file_size: u64,
    show_progress: bool,
) -> Result<String, Box<EvalAltResult>> {
    let mut hasher = D::new();
    let mut buffer = [0u8; CHUNK_SIZE];
    let mut total_read = 0u64;
    let mut last_percent = 0u8;

    loop {
        let n = reader
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

    Ok(hex::encode(hasher.finalize()))
}

/// Compute BLAKE3 hash (separate implementation due to different API)
fn hash_blake3_with_progress(
    reader: &mut impl Read,
    file_size: u64,
    show_progress: bool,
) -> Result<String, Box<EvalAltResult>> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; CHUNK_SIZE];
    let mut total_read = 0u64;
    let mut last_percent = 0u8;

    loop {
        let n = reader
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

    Ok(hasher.finalize().to_hex().to_string())
}

/// Compute all hashes for a file at once (for `recipe hash` command).
pub fn compute_all_hashes(file: &Path) -> Result<FileHashes, std::io::Error> {
    let mut f = std::fs::File::open(file)?;
    let mut sha256_hasher = sha2::Sha256::new();
    let mut sha512_hasher = sha2::Sha512::new();
    let mut blake3_hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 8192];

    use sha2::Digest;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_sha256() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, b"hello world").unwrap();

        // SHA256 of "hello world"
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        verify_file_hash(&file_path, expected, HashAlgorithm::Sha256).unwrap();
    }

    #[test]
    fn test_verify_sha256_mismatch() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, b"hello world").unwrap();

        let result = verify_file_hash(&file_path, "wrong_hash", HashAlgorithm::Sha256);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("integrity check failed")
        );
    }

    #[test]
    fn test_verify_blake3() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, b"hello world").unwrap();

        // BLAKE3 of "hello world"
        let expected = "d74981efa70a0c880b8d8c1985d075dbcbf679b99a5f9914e5aaf96b831a9e24";
        verify_file_hash(&file_path, expected, HashAlgorithm::Blake3).unwrap();
    }

    #[test]
    fn test_compute_all_hashes() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, b"hello world").unwrap();

        let hashes = compute_all_hashes(&file_path).unwrap();
        assert_eq!(
            hashes.sha256,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
        assert_eq!(
            hashes.blake3,
            "d74981efa70a0c880b8d8c1985d075dbcbf679b99a5f9914e5aaf96b831a9e24"
        );
    }

    #[test]
    fn test_case_insensitive_comparison() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        std::fs::write(&file_path, b"hello world").unwrap();

        // Uppercase should work
        let expected = "B94D27B9934D3E08A52E52D7DA7DABFAC484EFE37A5380EE9088F7ACE2EFCDE9";
        verify_file_hash(&file_path, expected, HashAlgorithm::Sha256).unwrap();
    }
}
