//! The offline cache: pinned downloads by content, read before the network.
//!
//! A directory holding `sha256/<hex>` files, one per payload, named by the
//! SHA-256 of its bytes. `download` (and `download_exact`) look there first and
//! use a file whose bytes match the pin, with no network at all. It is
//! `$RECIPE_CACHE_DIR` when set (an absolute path; empty turns the cache off),
//! else `/usr/share/recipe/cache` when that directory exists (an install medium
//! carries it). It is read-only to Recipe: nothing is ever added to it.
//!
//! The coordinator chooses the directory and hands it to a host-run worker in
//! its request. A Ginux run does not see it.
use anyhow::{Result, ensure};
use std::fs::File;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const DEFAULT: &str = "/usr/share/recipe/cache";
pub const ENV: &str = "RECIPE_CACHE_DIR";

/// The cache directory this coordinator uses, if any.
pub fn dir() -> Result<Option<PathBuf>> {
    match std::env::var_os(ENV) {
        Some(value) if value.is_empty() => Ok(None),
        Some(value) => {
            let path = PathBuf::from(value);
            ensure!(
                path.is_absolute()
                    && !path
                        .components()
                        .any(|c| c == std::path::Component::ParentDir),
                "{ENV} must be an absolute path without `..`: {}",
                path.display()
            );
            ensure!(
                path.is_dir(),
                "{ENV} names {}, which is not a directory",
                path.display()
            );
            Ok(Some(path))
        }
        None => Ok(Path::new(DEFAULT).is_dir().then(|| PathBuf::from(DEFAULT))),
    }
}

static WORKER: OnceLock<PathBuf> = OnceLock::new();

/// Select this worker's cache (a host run's request names it).
pub(crate) fn set_worker(path: PathBuf) -> Result<()> {
    WORKER
        .set(path)
        .map_err(|_| anyhow::anyhow!("offline cache is already set"))
}

/// This worker's cache, if its request named one.
pub(crate) fn worker() -> Option<&'static Path> {
    WORKER.get().map(PathBuf::as_path)
}

/// The cached file for `sha256` (64 hex digits), opened without following a
/// link, or None when the cache does not hold it as a regular file.
pub(crate) fn open(dir: &Path, sha256: &str) -> Option<File> {
    let hex = sha256.to_ascii_lowercase();
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join("sha256").join(hex))
        .ok()?;
    file.metadata().ok()?.is_file().then_some(file)
}
