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

pub fn verify_file_hash(
    file: &Path,
    expected: &str,
    algorithm: HashAlgorithm,
) -> Result<(), Box<EvalAltResult>> {
    let mut f = std::fs::File::open(file).map_err(|e| format!("cannot open file: {}", e))?;

    // Size only decides whether to show progress.
    let file_size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let show_progress = file_size > PROGRESS_THRESHOLD;

    let hash = match algorithm {
        HashAlgorithm::Sha256 => {
            use sha2::Digest;
            let mut hasher = sha2::Sha256::new();
            read_chunks(&mut f, file_size, show_progress, |chunk| hasher.update(chunk))?;
            hex::encode(hasher.finalize())
        }
        HashAlgorithm::Sha512 => {
            use sha2::Digest;
            let mut hasher = sha2::Sha512::new();
            read_chunks(&mut f, file_size, show_progress, |chunk| hasher.update(chunk))?;
            hex::encode(hasher.finalize())
        }
        HashAlgorithm::Blake3 => {
            let mut hasher = blake3::Hasher::new();
            read_chunks(&mut f, file_size, show_progress, |chunk| {
                hasher.update(chunk);
            })?;
            hasher.finalize().to_hex().to_string()
        }
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

/// Feed `reader` to `update` in chunks, with out-of-band percent progress.
fn read_chunks(
    reader: &mut impl Read,
    file_size: u64,
    show_progress: bool,
    mut update: impl FnMut(&[u8]),
) -> Result<(), Box<EvalAltResult>> {
    let mut buffer = vec![0u8; CHUNK_SIZE];
    let mut total_read = 0u64;
    let mut last_percent = 0u8;
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|e| format!("read error: {}", e))?;
        if n == 0 {
            break;
        }
        update(&buffer[..n]);
        total_read += n as u64;
        if show_progress && file_size > 0 {
            let percent = ((total_read * 100) / file_size) as u8;
            if percent >= last_percent + 10 {
                crate::diagnostics::out_of_band(
                    format!("\r     checksum: {percent}%...").as_bytes(),
                );
                last_percent = percent;
            }
        }
    }
    if show_progress {
        crate::diagnostics::out_of_band(b"\n");
    }
    Ok(())
}

/// Compute all hashes for a file at once (for `recipe hash` command).
pub fn compute_all_hashes(file: &Path) -> Result<FileHashes, Box<EvalAltResult>> {
    use sha2::Digest;
    let mut f = std::fs::File::open(file).map_err(|e| format!("cannot open file: {}", e))?;
    let mut sha256 = sha2::Sha256::new();
    let mut sha512 = sha2::Sha512::new();
    let mut blake3 = blake3::Hasher::new();
    read_chunks(&mut f, 0, false, |chunk| {
        sha256.update(chunk);
        sha512.update(chunk);
        blake3.update(chunk);
    })?;
    Ok(FileHashes {
        sha256: hex::encode(sha256.finalize()),
        sha512: hex::encode(sha512.finalize()),
        blake3: blake3.finalize().to_hex().to_string(),
    })
}

/// Container for computed file hashes
#[derive(Debug, Clone)]
pub struct FileHashes {
    pub sha256: String,
    pub sha512: String,
    pub blake3: String,
}
