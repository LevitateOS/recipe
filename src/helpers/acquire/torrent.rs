//! Torrent/download helpers for recipe scripts
//!
//! Pure functions for downloading files via BitTorrent or HTTP with resume support.
//! Uses a pure-Rust BitTorrent client (librqbit) and pure-Rust HTTP downloads.
//!
//! ## Example
//!
//! ```rhai
//! fn acquire(ctx) {
//!     let path = torrent(ctx.torrent_url, BUILD_DIR);
//!     ctx.iso_path = path;
//!     ctx
//! }
//! ```

use crate::core::output;
use crate::helpers::internal::progress;
use indicatif::ProgressBar;
use librqbit::{AddTorrent, AddTorrentOptions, AddTorrentResponse, Session, SessionOptions};
use rhai::EvalAltResult;
use std::io::{Read, Write};
use std::path::{Component, Path};
use std::time::Duration;

/// Validate that a URL uses an allowed scheme for torrent/download operations.
/// Only http://, https://, and magnet: URLs are supported.
fn validate_download_url(url: &str) -> Result<(), Box<EvalAltResult>> {
    if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("magnet:") {
        Ok(())
    } else {
        Err(format!(
            "Unsupported URL scheme: {}\n\
             Only http://, https://, and magnet: URLs are supported",
            url
        )
        .into())
    }
}

fn validate_safe_relative_path(p: &Path) -> Result<(), Box<EvalAltResult>> {
    if p.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(format!("unsafe torrent path component: {}", p.display()).into());
    }
    Ok(())
}

/// Download a file via BitTorrent.
///
/// Takes a .torrent file URL or magnet link. Downloads the content to dest_dir.
/// Returns the path to the downloaded file.
///
/// # Example
/// ```rhai
/// let iso = torrent("https://example.com/file.torrent", BUILD_DIR);
/// ```
pub fn torrent(url: &str, dest_dir: &str) -> Result<String, Box<EvalAltResult>> {
    // Validate URL scheme for security
    validate_download_url(url)?;

    let dest_dir_path = Path::new(dest_dir);
    std::fs::create_dir_all(dest_dir_path)
        .map_err(|e| format!("cannot create destination directory {}: {}", dest_dir, e))?;

    output::detail(&format!("torrent download: {}", url));

    let pb = progress::create_download_progress("downloading via bittorrent...");
    let pb_for_async = pb.clone();
    let _guard = progress::ProgressGuard::new(&pb);

    let url = url.to_string();
    let output_folder = dest_dir.to_string();
    let dest_dir_path = dest_dir_path.to_path_buf();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to initialize tokio runtime: {}", e))?;

    let downloaded_path = rt
        .block_on(async move {
            let session_opts = SessionOptions {
                // Avoid polluting user config with DHT persistence for build tooling.
                disable_dht_persistence: true,
                // Build tooling should not seed/upload.
                disable_upload: true,
                ..Default::default()
            };
            let session = Session::new_with_opts(dest_dir_path.clone(), session_opts).await?;

            let handle = match session
                .add_torrent(
                    AddTorrent::from_url(url),
                    Some(AddTorrentOptions {
                        // Allow resuming/continuing on existing files in dest_dir.
                        overwrite: true,
                        // Always download into the explicit dest_dir (no implicit subfolder).
                        output_folder: Some(output_folder),
                        ..Default::default()
                    }),
                )
                .await?
            {
                AddTorrentResponse::Added(_, handle) => handle,
                AddTorrentResponse::AlreadyManaged(_, handle) => handle,
                AddTorrentResponse::ListOnly(_) => {
                    anyhow::bail!("internal error: list_only response while downloading")
                }
            };

            // Update progress while waiting for completion.
            let pb2: ProgressBar = pb_for_async;
            let handle2 = handle.clone();
            let progress_task = tokio::spawn(async move {
                let mut upgraded = false;
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let stats = handle2.stats();

                    if stats.total_bytes > 0 {
                        if !upgraded {
                            progress::upgrade_to_bytes(&pb2, stats.total_bytes);
                            upgraded = true;
                        }
                        pb2.set_position(stats.progress_bytes);
                        pb2.set_message(format!(
                            "downloading via bittorrent ({:.1}%)",
                            (stats.progress_bytes as f64 / stats.total_bytes as f64) * 100.0
                        ));
                    } else {
                        pb2.set_message(
                            "downloading via bittorrent (resolving metadata)".to_string(),
                        );
                    }

                    if stats.finished {
                        break;
                    }
                }
            });

            handle.wait_until_completed().await?;
            progress_task.abort();

            // Single-file torrents: return the file path.
            // Multi-file torrents: return dest_dir (the output folder).
            let computed = handle.with_metadata(|m| {
                if m.file_infos.len() == 1 {
                    let rel = &m.file_infos[0].relative_filename;
                    validate_safe_relative_path(rel)?;
                    Ok::<_, Box<EvalAltResult>>(
                        dest_dir_path.join(rel).to_string_lossy().to_string(),
                    )
                } else {
                    Ok::<_, Box<EvalAltResult>>(dest_dir_path.to_string_lossy().to_string())
                }
            })??;

            Ok::<_, anyhow::Error>(computed)
        })
        .map_err(|e| format!("torrent download failed: {:#}", e))?;

    output::detail(&format!("downloaded {}", downloaded_path));
    Ok(downloaded_path)
}

/// Download a file via HTTP with resume support.
///
/// If `dest` already exists, this function requires the server to honor HTTP `Range`
/// requests (HTTP 206) or report that the file is already complete (HTTP 416 with a
/// valid `Content-Range`). It will not fall back to restarting the download.
///
/// # Example
/// ```rhai
/// let file = download_with_resume("https://example.com/large-file.iso", BUILD_DIR + "/file.iso");
/// ```
pub fn download_with_resume(url: &str, dest: &str) -> Result<String, Box<EvalAltResult>> {
    // Validate URL scheme for security
    validate_download_url(url)?;

    let dest_path = Path::new(dest);
    let dest_dir = dest_path.parent().unwrap_or(Path::new("."));
    let filename = dest_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| extract_filename_from_url(url));

    std::fs::create_dir_all(dest_dir)
        .map_err(|e| format!("cannot create destination directory: {}", e))?;

    // Determine current size for resume.
    let existing_len = dest_path.metadata().map(|m| m.len()).unwrap_or(0);

    output::detail(&format!("downloading with resume: {}", url));
    let pb = progress::create_download_progress(&format!("downloading {}", filename));
    let _guard = progress::ProgressGuard::new(&pb);

    // Build request with Range header if resuming.
    let mut req = ureq::get(url);
    if existing_len > 0 {
        req = req.set("Range", &format!("bytes={}-", existing_len));
        pb.set_message(format!("resuming {} ({} bytes)", filename, existing_len));
    }

    let resp = req.call().map_err(|e| format!("download failed: {}", e))?;
    let status = resp.status();

    // No fallback: if resuming, server must honor Range (206) or indicate already complete (416).
    let (append, total_len_opt) = if existing_len > 0 {
        match status {
            206 => {
                let total = resp
                    .header("content-range")
                    .and_then(|cr| cr.split('/').nth(1))
                    .and_then(|t| t.parse::<u64>().ok())
                    .or_else(|| {
                        resp.header("content-length")
                            .and_then(|s| s.parse::<u64>().ok())
                            .map(|remaining| existing_len + remaining)
                    });
                (true, total)
            }
            416 => {
                let total = resp
                    .header("content-range")
                    .and_then(|cr| cr.split('/').nth(1))
                    .and_then(|t| t.parse::<u64>().ok())
                    .ok_or("resume requested but server returned 416 without Content-Range")?;

                if existing_len == total {
                    output::detail(&format!("{} already fully downloaded", dest_path.display()));
                    return Ok(dest.to_string());
                }
                return Err(format!(
                    "resume requested but local file size {} does not match remote size {} for {}",
                    existing_len, total, url
                )
                .into());
            }
            200 => {
                return Err(format!(
                    "resume requested for {} but server did not honor Range (returned 200). Delete '{}' to restart from scratch.",
                    url,
                    dest_path.display()
                )
                .into());
            }
            other => {
                return Err(
                    format!("unexpected HTTP status {} while downloading {}", other, url).into(),
                );
            }
        }
    } else {
        match status {
            200 => {
                let total = resp
                    .header("content-length")
                    .and_then(|s| s.parse::<u64>().ok());
                (false, total)
            }
            other => {
                return Err(
                    format!("unexpected HTTP status {} while downloading {}", other, url).into(),
                );
            }
        }
    };

    if let Some(total) = total_len_opt {
        progress::upgrade_to_bytes(&pb, total);
        pb.set_position(existing_len);
    }

    let mut file = if append {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dest_path)
            .map_err(|e| format!("cannot open for append {}: {}", dest_path.display(), e))?
    } else {
        std::fs::File::create(dest_path)
            .map_err(|e| format!("cannot create file {}: {}", dest_path.display(), e))?
    };

    let mut reader = resp.into_reader();
    let mut buf = [0u8; 64 * 1024];
    let mut written = existing_len;

    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("read error: {}", e))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("write error: {}", e))?;
        written += n as u64;
        pb.set_position(written);
    }

    output::detail(&format!("downloaded {}", dest_path.display()));
    Ok(dest.to_string())
}

/// Extract filename from a URL, handling query strings and sanitizing
fn extract_filename_from_url(url: &str) -> String {
    // Strip query parameters and fragments
    let url_without_query = url.split('?').next().unwrap_or(url);
    let url_without_fragment = url_without_query
        .split('#')
        .next()
        .unwrap_or(url_without_query);

    let filename = url_without_fragment
        .rsplit('/')
        .next()
        .unwrap_or("download");

    // Strip .torrent extension if present
    let name = filename.strip_suffix(".torrent").unwrap_or(filename);

    // Sanitize: ensure non-empty and not a path traversal
    if name.is_empty() || name == "." || name == ".." {
        return "download".to_string();
    }

    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviso_cheat_test::cheat_reviewed;

    // ==================== URL Validation Tests ====================

    #[cheat_reviewed("Security - HTTPS URLs accepted")]
    #[test]
    fn test_validate_download_url_https() {
        assert!(validate_download_url("https://example.com/file.iso").is_ok());
    }

    #[cheat_reviewed("Security - HTTP URLs accepted")]
    #[test]
    fn test_validate_download_url_http() {
        assert!(validate_download_url("http://example.com/file.iso").is_ok());
    }

    #[cheat_reviewed("Security - magnet URLs accepted")]
    #[test]
    fn test_validate_download_url_magnet() {
        assert!(validate_download_url("magnet:?xt=urn:btih:abc123").is_ok());
    }

    #[cheat_reviewed("Security - file:// URLs rejected")]
    #[test]
    fn test_validate_download_url_file_rejected() {
        let result = validate_download_url("file:///etc/passwd");
        assert!(result.is_err());
    }

    #[cheat_reviewed("Security - ftp:// URLs rejected")]
    #[test]
    fn test_validate_download_url_ftp_rejected() {
        let result = validate_download_url("ftp://example.com/file.iso");
        assert!(result.is_err());
    }

    #[cheat_reviewed("Security - bare paths rejected")]
    #[test]
    fn test_validate_download_url_bare_path_rejected() {
        let result = validate_download_url("/local/path/to/file");
        assert!(result.is_err());
    }

    // ==================== extract_filename_from_url Tests ====================

    #[cheat_reviewed("URL filename extraction - basic case")]
    #[test]
    fn test_extract_filename_from_url_basic() {
        let name = extract_filename_from_url("https://example.com/file.iso");
        assert_eq!(name, "file.iso");
    }

    #[cheat_reviewed("URL filename extraction - query string stripped")]
    #[test]
    fn test_extract_filename_from_url_query() {
        let name = extract_filename_from_url("https://example.com/file.iso?token=abc");
        assert_eq!(name, "file.iso");
    }

    #[cheat_reviewed("URL filename extraction - fragment stripped")]
    #[test]
    fn test_extract_filename_from_url_fragment() {
        let name = extract_filename_from_url("https://example.com/file.iso#section");
        assert_eq!(name, "file.iso");
    }
}
