//! Download helpers for acquiring files
//!
//! File downloads with explicit destinations and atomic publication.
//! All functions take explicit inputs and return explicit outputs.
//!
//! ## Example
//!
//! ```rhai
//! fn acquire(ctx) {
//!     let archive = download(ctx.url, BUILD_DIR + "/foo.tar.gz", ctx.sha256);
//!     ctx.archive_path = archive;
//!     ctx
//! }
//! ```

use crate::core::output;
use rhai::EvalAltResult;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use super::super::internal::fs_utils;
use super::super::internal::hash::{self, HashAlgorithm};
use super::super::internal::progress::{self, upgrade_to_bytes};

/// Download a file from a URL to a specific destination.
///
/// Returns the path to the downloaded file on success.
///
/// Throws on failure.
/// The destination is replaced only after the complete response is written.
/// Recipes still verify package integrity with the appropriate hash helper.
///
/// # Example
/// ```rhai
/// let path = download("https://example.com/foo.tar.gz", "/tmp/foo.tar.gz");
/// verify_sha256(path, "abc123...");
/// ```
pub fn download(url: &str, dest: &str) -> Result<String, Box<EvalAltResult>> {
    download_impl(url, dest, None)
}

/// Verify the recipe's SHA256 before publishing the completed destination.
pub fn download_verified(
    url: &str,
    dest: &str,
    sha256: &str,
) -> Result<String, Box<EvalAltResult>> {
    validate_sha256(sha256)?;
    download_impl(url, dest, Some(sha256))
}

pub(super) fn validate_sha256(sha256: &str) -> Result<(), Box<EvalAltResult>> {
    if !super::verify::is_valid_sha256(sha256) {
        return Err("download SHA256 must contain exactly 64 hexadecimal characters".into());
    }
    Ok(())
}

fn download_impl(
    url: &str,
    dest: &str,
    sha256: Option<&str>,
) -> Result<String, Box<EvalAltResult>> {
    let dest_path = Path::new(dest);

    let filename = dest_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".to_string());

    let response = request(url)
        .call()
        .map_err(|e| format!("download failed: {}", e))?;
    let total_bytes = save_response(response, dest_path, &filename, sha256)?;
    output::detail(&format!("downloaded {} ({} bytes)", filename, total_bytes));

    Ok(dest.to_string())
}

// ============================================================================
// Internal helpers
// ============================================================================

fn request(url: &str) -> ureq::Request {
    ureq::get(url)
        .set("Accept-Encoding", "identity")
        .timeout(Duration::from_secs(300))
}

fn identity_encoding(response: &ureq::Response) -> Result<(), Box<EvalAltResult>> {
    if response
        .header("content-encoding")
        .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
    {
        return Err("download requires identity Content-Encoding".into());
    }
    Ok(())
}

fn content_length(response: &ureq::Response) -> Result<Option<u64>, Box<EvalAltResult>> {
    response
        .header("content-length")
        .map(|value| {
            value
                .parse()
                .map_err(|_| "invalid download Content-Length".into())
        })
        .transpose()
}

/// Resume an existing prefix, publishing a replacement only after completion.
/// A server must honor Range; a size-only 416 response is not integrity evidence.
/// Recipes must verify the completed file against their declared digest.
pub fn download_with_resume(url: &str, dest: &str) -> Result<String, Box<EvalAltResult>> {
    resume_impl(url, dest, None)
}

/// Resume and verify the complete SHA256 before publication, including 416 reuse.
pub fn download_with_resume_verified(
    url: &str,
    dest: &str,
    sha256: &str,
) -> Result<String, Box<EvalAltResult>> {
    validate_sha256(sha256)?;
    resume_impl(url, dest, Some(sha256))
}

fn resume_impl(url: &str, dest: &str, sha256: Option<&str>) -> Result<String, Box<EvalAltResult>> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err("HTTP resume requires an http:// or https:// URL".into());
    }
    let dest_path = Path::new(dest);
    let prefix = match File::open(dest_path) {
        Ok(file) => {
            let metadata = file
                .metadata()
                .map_err(|e| format!("cannot inspect resume prefix: {e}"))?;
            if !metadata.is_file() {
                return Err("resume destination must be a regular file".into());
            }
            Some((file, metadata.len()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("cannot open resume prefix: {error}").into()),
    };
    let existing_len = prefix.as_ref().map_or(0, |(_, len)| *len);
    let mut req = request(url);
    if existing_len > 0 {
        req = req.set("Range", &format!("bytes={existing_len}-"));
    }
    // ureq returns HTTP error statuses as Error::Status, including 416.
    let response = match req.call() {
        Ok(response) => response,
        Err(ureq::Error::Status(416, response)) if existing_len > 0 => response,
        Err(error) => return Err(format!("download failed: {error}").into()),
    };
    let filename = dest_path.file_name().unwrap_or_default().to_string_lossy();
    if existing_len == 0 {
        save_response(response, dest_path, &filename, sha256)?;
        return Ok(dest.to_owned());
    }
    match response.status() {
        416 => {
            let total = response.header("content-range")
                .and_then(|range| range.strip_prefix("bytes */"))
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or("resume returned 416 without a valid Content-Range")?;
            if total != existing_len {
                return Err(format!("resume prefix size {existing_len} does not match remote size {total}").into());
            }
            if let Some(expected) = sha256 {
                hash::verify_file_hash(dest_path, expected, HashAlgorithm::Sha256)?;
            } else {
                output::detail("download size matches remote; recipe integrity verification is still required");
            }
        }
        206 => {
            identity_encoding(&response)?;
            let (bounds, total) = response.header("content-range")
                .and_then(|range| range.strip_prefix("bytes "))
                .and_then(|range| range.split_once('/'))
                .ok_or("resume requires a valid Content-Range")?;
            let (start, end) = bounds.split_once('-').ok_or("invalid resume Content-Range bounds")?;
            let start = start.parse::<u64>().map_err(|_| "invalid resume range start")?;
            let end = end.parse::<u64>().map_err(|_| "invalid resume range end")?;
            let total = total.parse::<u64>().map_err(|_| "invalid resume range total")?;
            if start != existing_len || end < start || end.checked_add(1) != Some(total) {
                return Err("resume Content-Range does not cover the requested remainder".into());
            }
            if content_length(&response)?.is_some_and(|len| len != total - start) {
                return Err("resume Content-Length does not match Content-Range".into());
            }
            save_response_with_prefix(response, dest_path, &filename, prefix, Some(total), sha256)?;
        }
        200 => return Err("resume requested but server did not honor Range (returned 200); use download to restart explicitly".into()),
        status => return Err(format!("unexpected HTTP status {status} while resuming").into()),
    }
    Ok(dest.to_owned())
}

/// Write a response beside its destination, then publish the completed file.
pub(crate) fn save_response(
    response: ureq::Response,
    dest: &Path,
    filename: &str,
    sha256: Option<&str>,
) -> Result<u64, Box<EvalAltResult>> {
    if response.status() != 200 {
        return Err(format!(
            "expected complete HTTP response (200), received {}",
            response.status()
        )
        .into());
    }
    identity_encoding(&response)?;
    let total = content_length(&response)?;
    save_response_with_prefix(response, dest, filename, None, total, sha256)
}

fn save_response_with_prefix(
    response: ureq::Response,
    dest: &Path,
    filename: &str,
    prefix: Option<(File, u64)>,
    expected_total: Option<u64>,
    sha256: Option<&str>,
) -> Result<u64, Box<EvalAltResult>> {
    let pb = progress::create_spinner(&format!("downloading {}", filename));

    // Get content length if available and upgrade progress bar
    if let Some(len) = expected_total {
        upgrade_to_bytes(&pb, len);
    }

    let result = (|| {
        fs_utils::ensure_parent_dir(dest)?;
        let parent = dest
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut file = tempfile::Builder::new()
            .prefix(".recipe-download.")
            .tempfile_in(parent)
            .map_err(|e| {
                format!(
                    "cannot create partial download in {}: {}",
                    parent.display(),
                    e
                )
            })?;

        let mut total_bytes = 0u64;
        if let Some((prefix, length)) = prefix {
            total_bytes = std::io::copy(&mut prefix.take(length), &mut file)
                .map_err(|e| format!("cannot stage resume prefix: {e}"))?;
            if total_bytes != length {
                return Err("resume prefix changed while staging".into());
            }
        }
        pb.set_position(total_bytes);
        let mut reader = response.into_reader();
        let mut buffer = [0u8; 8192];
        loop {
            let bytes_read = reader
                .read(&mut buffer)
                .map_err(|e| format!("read error: {}", e))?;
            if bytes_read == 0 {
                break;
            }
            total_bytes = total_bytes
                .checked_add(bytes_read as u64)
                .ok_or("download length overflow")?;
            if expected_total.is_some_and(|total| total_bytes > total) {
                return Err("download exceeds declared length".into());
            }
            file.write_all(&buffer[..bytes_read])
                .map_err(|e| format!("write error: {}", e))?;
            pb.set_position(total_bytes);
        }
        if expected_total.is_some_and(|total| total_bytes != total) {
            return Err("download ended before the declared length".into());
        }
        if let Some(expected) = sha256 {
            hash::verify_file_hash(file.path(), expected, HashAlgorithm::Sha256)?;
        }
        file.as_file()
            .sync_all()
            .map_err(|e| format!("cannot flush download: {}", e))?;
        file.persist(dest)
            .map_err(|e| format!("cannot publish download to {}: {}", dest.display(), e.error))?;
        Ok(total_bytes)
    })();

    pb.finish_and_clear();
    result
}
