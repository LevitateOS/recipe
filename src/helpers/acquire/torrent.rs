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

// Preserve the public Rust import path while sharing the HTTP download owner.
pub use super::download::download_with_resume;
