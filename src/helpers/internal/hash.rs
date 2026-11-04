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
