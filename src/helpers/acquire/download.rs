//! Download helpers for acquiring files
//!
//! File downloads with explicit destinations and atomic publication.
//! All functions take explicit inputs and return explicit outputs.
//!
//! ## Example
//!
//! ```rhai
//! fn acquire(ctx) {
//!     let archive = download(ctx.url, ctx.runtime.paths.build + "/foo.tar.gz", ctx.sha256);
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
use super::super::internal::progress::Spinner;

/// Acquire a complete payload at an explicit path, checking SHA256 before
/// atomically replacing any existing file. Resume preserves the old prefix on
/// every failure and verifies the complete file, including size-only 416 replies.
pub fn download(
    url: &str,
    dest: &str,
    sha256: &str,
    resume: bool,
) -> Result<String, Box<EvalAltResult>> {
    super::verify::validate_sha256(sha256)?;
    super::validate_http_url(url)?;
    let path = Path::new(dest);
    if path.file_name().is_none() {
        return Err("download destination must name a regular file".into());
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err("download destination must be a regular file".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot inspect download destination: {error}").into()),
    }
    if resume {
        resume_impl(url, dest, sha256)
    } else {
        download_impl(url, dest, sha256)
    }
}

fn download_impl(url: &str, dest: &str, sha256: &str) -> Result<String, Box<EvalAltResult>> {
    let dest_path = Path::new(dest);

    let filename = dest_path
        .file_name()
        .expect("download() rejects destinations without a file name")
        .to_string_lossy()
        .to_string();

    let response = request(url)?
        .call()
        .map_err(|e| format!("download failed: {}", e))?;
    let total_bytes = save_response(response, dest_path, &filename, sha256)?;
    output::detail(&format!("downloaded {} ({} bytes)", filename, total_bytes));

    Ok(dest.to_string())
}

// ============================================================================
// Internal helpers
// ============================================================================

fn request(url: &str) -> Result<ureq::Request, Box<EvalAltResult>> {
    Ok(super::http_request(url, Duration::from_secs(300))?.set("Accept-Encoding", "identity"))
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

fn resume_impl(url: &str, dest: &str, sha256: &str) -> Result<String, Box<EvalAltResult>> {
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
    let mut req = request(url)?;
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
            hash::verify_file_hash(dest_path, sha256, HashAlgorithm::Sha256)?;
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
fn save_response(
    response: ureq::Response,
    dest: &Path,
    filename: &str,
    sha256: &str,
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
    sha256: &str,
) -> Result<u64, Box<EvalAltResult>> {
    let pb = Spinner::new(&format!("downloading {}", filename));
    if let Some(len) = expected_total {
        pb.bytes(len);
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
        // A front end is told at the start, a few times a second, and at the end.
        crate::core::output::fetch_event(filename, total_bytes, expected_total);
        let mut told = std::time::Instant::now();
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
            if told.elapsed() >= std::time::Duration::from_millis(250) {
                crate::core::output::fetch_event(filename, total_bytes, expected_total);
                told = std::time::Instant::now();
            }
        }
        crate::core::output::fetch_event(filename, total_bytes, expected_total);
        if expected_total.is_some_and(|total| total_bytes != total) {
            return Err("download ended before the declared length".into());
        }
        hash::verify_file_hash(file.path(), sha256, HashAlgorithm::Sha256)?;
        file.as_file()
            .sync_all()
            .map_err(|e| format!("cannot flush download: {}", e))?;
        file.persist(dest)
            .map_err(|e| format!("cannot publish download to {}: {}", dest.display(), e.error))?;
        Ok(total_bytes)
    })();
    drop(pb);
    result
}

/// Bounded, exact-byte metadata acquisition. Authenticity is established by the
/// caller's signed metadata protocol; an unbounded text/UTF-8 conversion cannot
/// represent detached binary signatures.
pub fn fetch_bounded(url: &str, dest: &str, limit: i64) -> Result<String, Box<EvalAltResult>> {
    if !(1..=1024 * 1024).contains(&limit) {
        return Err("metadata acquisition bound must be 1..1048576 bytes".into());
    }
    let response = request(url)?
        .call()
        .map_err(|e| format!("metadata acquisition failed: {e}"))?;
    if response.status() != 200 {
        return Err("metadata acquisition requires HTTP 200".into());
    }
    identity_encoding(&response)?;
    let declared = content_length(&response)?;
    if declared.is_some_and(|n| n > limit as u64) {
        return Err("metadata exceeds acquisition bound".into());
    }
    let path = Path::new(dest);
    fs_utils::ensure_parent_dir(path)?;
    let parent = path.parent().ok_or("metadata destination needs a parent")?;
    let mut output =
        tempfile::NamedTempFile::new_in(parent).map_err(|e| format!("metadata staging: {e}"))?;
    let mut input = response.into_reader().take(limit as u64 + 1);
    let size = std::io::copy(&mut input, &mut output).map_err(|e| format!("metadata read: {e}"))?;
    if size > limit as u64 || declared.is_some_and(|n| n != size) {
        return Err("metadata length differs from acquisition bound/Content-Length".into());
    }
    output
        .as_file()
        .sync_all()
        .map_err(|e| format!("metadata flush: {e}"))?;
    output
        .persist(path)
        .map_err(|e| format!("metadata publication: {e}"))?;
    Ok(dest.into())
}
/// A signed release's length is authority, not a response header chosen by the
/// server. Publication remains atomic and SHA256-verified by the existing writer.
pub fn download_exact(
    url: &str,
    dest: &str,
    sha256: &str,
    size: i64,
) -> Result<String, Box<EvalAltResult>> {
    if size <= 0 {
        return Err("download length must be positive".into());
    }
    super::verify::validate_sha256(sha256)?;
    let response = request(url)?
        .call()
        .map_err(|e| format!("download failed: {e}"))?;
    if response.status() != 200 {
        return Err("complete download requires HTTP 200".into());
    }
    identity_encoding(&response)?;
    if content_length(&response)?.is_some_and(|n| n != size as u64) {
        return Err("download Content-Length differs from signed size".into());
    }
    let path = Path::new(dest);
    save_response_with_prefix(
        response,
        path,
        &path.file_name().unwrap_or_default().to_string_lossy(),
        None,
        Some(size as u64),
        sha256,
    )?;
    Ok(dest.into())
}
