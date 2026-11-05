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
use anyhow::{Context, bail};
use indicatif::ProgressBar;
use librqbit::{AddTorrent, AddTorrentOptions, AddTorrentResponse, Session, SessionOptions};
use rhai::EvalAltResult;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
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
    if p.as_os_str().is_empty() || p.components().any(|c| !matches!(c, Component::Normal(_))) {
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
    torrent_impl(url, dest_dir, None)
}

/// Download exactly one named file and verify SHA256 before replacing its cache entry.
pub fn torrent_verified(
    url: &str,
    dest_dir: &str,
    filename: &str,
    sha256: &str,
) -> Result<String, Box<EvalAltResult>> {
    super::download::validate_sha256(sha256)?;
    validate_safe_relative_path(Path::new(filename))?;
    if Path::new(filename).components().count() != 1
        || Path::new(filename).file_name() != Some(std::ffi::OsStr::new(filename))
    {
        return Err("verified torrent filename must be a basename".into());
    }
    torrent_impl(url, dest_dir, Some((filename, sha256)))
}

fn torrent_impl(
    url: &str,
    dest_dir: &str,
    expected: Option<(&str, &str)>,
) -> Result<String, Box<EvalAltResult>> {
    validate_download_url(url)?;

    let dest_dir_path = Path::new(dest_dir);
    std::fs::create_dir_all(dest_dir_path)
        .map_err(|e| format!("cannot create destination directory {}: {}", dest_dir, e))?;
    let dest_dir_path = dest_dir_path
        .canonicalize()
        .map_err(|e| format!("cannot resolve torrent destination {dest_dir}: {e}"))?;
    let staging = tempfile::Builder::new()
        .prefix(".torrent-part-")
        .tempdir_in(&dest_dir_path)
        .map_err(|e| format!("cannot stage torrent in {dest_dir}: {e}"))?;

    output::detail(&format!("torrent download: {}", url));

    let pb = progress::create_download_progress("downloading via bittorrent...");
    let pb_for_async = pb.clone();
    let _guard = progress::ProgressGuard::new(&pb);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("failed to initialize tokio runtime: {}", e))?;

    let files = rt
        .block_on(async {
            let session_opts = SessionOptions {
                // Avoid polluting user config with DHT persistence for build tooling.
                disable_dht_persistence: true,
                // Build tooling should not seed/upload.
                disable_upload: true,
                ..Default::default()
            };
            let session =
                Session::new_with_opts(staging.path().to_path_buf(), session_opts).await?;
            let result = async {
                // Resolve metadata without starting filesystem writes. Validate every
                // entry, including multi-file torrents, before starting the transfer.
                let listing = match session
                    .add_torrent(
                        AddTorrent::from_url(url),
                        Some(AddTorrentOptions {
                            list_only: true,
                            output_folder: Some(staging.path().to_string_lossy().into_owned()),
                            ..Default::default()
                        }),
                    )
                    .await?
                {
                    AddTorrentResponse::ListOnly(listing) => listing,
                    _ => bail!("internal error: missing torrent metadata listing"),
                };
                let mut files = Vec::new();
                let mut names = BTreeSet::new();
                for file in listing.info.iter_file_details()? {
                    let relative = file.filename.to_pathbuf()?;
                    validate_safe_relative_path(&relative)?;
                    if !names.insert(relative.clone()) {
                        bail!("duplicate torrent path: {}", relative.display());
                    }
                    if file.symlink_path.is_some() || file.attrs().symlink {
                        bail!("torrent symlink entries are unsupported");
                    }
                    checked_destination(&dest_dir_path, &relative)?;
                    files.push((relative, file.len));
                }
                if files.is_empty() {
                    bail!("torrent contains no files");
                }
                if let Some((filename, _)) = expected {
                    if files.len() != 1 || files[0].0 != Path::new(filename) {
                        bail!("torrent must contain exactly the expected file '{filename}'");
                    }
                }

                // Legacy calls retain piece-verified reuse without modifying the
                // old cache entry. Verified calls acquire a fresh replacement,
                // including when a recipe explicitly requests a refresh.
                for (relative, _) in files.iter().filter(|_| expected.is_none()) {
                    let existing = dest_dir_path.join(relative);
                    if existing.is_file() {
                        let staged = staging.path().join(relative);
                        std::fs::create_dir_all(
                            staged.parent().context("missing torrent parent")?,
                        )?;
                        std::fs::copy(&existing, &staged).with_context(|| {
                            format!("staging existing torrent file {}", existing.display())
                        })?;
                    }
                }
                let handle = session
                    .add_torrent(
                        AddTorrent::from_bytes(listing.torrent_bytes),
                        Some(AddTorrentOptions {
                            overwrite: true,
                            initial_peers: Some(listing.seen_peers),
                            output_folder: Some(staging.path().to_string_lossy().into_owned()),
                            ..Default::default()
                        }),
                    )
                    .await?
                    .into_handle()
                    .context("missing torrent download handle")?;

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

                let completed = handle.wait_until_completed().await;
                progress_task.abort();
                completed?;
                Ok::<_, anyhow::Error>(files)
            }
            .await;
            // Stop writers before verification, rename, or staging cleanup.
            session.stop().await;
            result
        })
        .map_err(|e| format!("torrent download failed: {:#}", e))?;
    drop(rt);

    // Validate the complete set before publishing any file. Multi-file legacy
    // calls publish each completed file atomically, not as a whole-tree transaction.
    for (relative, length) in &files {
        let staged = staging.path().join(relative);
        let metadata = std::fs::symlink_metadata(&staged)
            .map_err(|e| format!("cannot inspect torrent result {}: {e}", staged.display()))?;
        if !metadata.is_file() || metadata.len() != *length {
            return Err(format!("incomplete torrent result: {}", relative.display()).into());
        }
        checked_destination(&dest_dir_path, relative)
            .map_err(|e| format!("cannot publish torrent: {e:#}"))?;
        if let Some((_, sha256)) = expected {
            crate::helpers::internal::hash::verify_file_hash(
                &staged,
                sha256,
                crate::helpers::internal::hash::HashAlgorithm::Sha256,
            )?;
        }
        std::fs::File::open(&staged)
            .and_then(|f| f.sync_all())
            .map_err(|e| format!("cannot flush torrent result: {e}"))?;
    }
    for (relative, _) in &files {
        let destination = checked_destination(&dest_dir_path, relative)
            .map_err(|e| format!("cannot publish torrent: {e:#}"))?;
        std::fs::create_dir_all(
            destination
                .parent()
                .ok_or("missing torrent destination parent")?,
        )
        .map_err(|e| format!("cannot create torrent output parent: {e}"))?;
        checked_destination(&dest_dir_path, relative)
            .map_err(|e| format!("cannot publish torrent: {e:#}"))?;
        std::fs::rename(staging.path().join(relative), &destination)
            .map_err(|e| format!("cannot publish torrent to {}: {e}", destination.display()))?;
    }
    let downloaded_path = if files.len() == 1 {
        dest_dir_path.join(&files[0].0)
    } else {
        dest_dir_path
    }
    .to_string_lossy()
    .into_owned();

    output::detail(&format!("downloaded {}", downloaded_path));
    Ok(downloaded_path)
}

/// Refuse existing symlink or non-directory parents and non-regular destinations.
fn checked_destination(root: &Path, relative: &Path) -> anyhow::Result<PathBuf> {
    let mut path = root.to_path_buf();
    let components = relative.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        path.push(component.as_os_str());
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                let final_component = index + 1 == components.len();
                if metadata.file_type().is_symlink()
                    || (final_component && !metadata.is_file())
                    || (!final_component && !metadata.is_dir())
                {
                    bail!("unsafe torrent destination: {}", path.display());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("checking {}", path.display()));
            }
        }
    }
    Ok(path)
}

// Preserve the public Rust import path while sharing the HTTP download owner.
pub use super::download::download_with_resume;
