//! Coordinator-owned scratch directories with leases for crash recovery.
//! A later invocation removes only abandoned `invocation-*` directories in
//! this private pool; the verified image cache (`crate::inputs::image_cache`)
//! lives beside them.
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::fs::{self, File};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

/// The coordinator's private scratch pool, created on first use: the absolute
/// `RECIPE_SCRATCH_DIR` when set; else `$TMPDIR/recipe-scratch-<uid>` when
/// `TMPDIR` is set (a caller that chose a temporary directory, as recab does);
/// else `$XDG_CACHE_HOME/recipe/scratch` (`~/.cache/recipe/scratch`). The default
/// is on disk, not in `/tmp`, which is RAM on many systems. Only this uid can
/// enter it.
pub(crate) fn pool() -> Result<PathBuf> {
    let uid = unsafe { libc::geteuid() };
    let pool = match std::env::var_os("RECIPE_SCRATCH_DIR") {
        Some(path) if !path.is_empty() => {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                bail!("RECIPE_SCRATCH_DIR must be an absolute path");
            }
            path
        }
        _ if std::env::var_os("TMPDIR").is_some_and(|dir| !dir.is_empty()) => {
            std::env::temp_dir().join(format!("recipe-scratch-{uid}"))
        }
        _ => dirs::cache_dir()
            .context("cannot locate the cache directory; set RECIPE_SCRATCH_DIR")?
            .join("recipe")
            .join("scratch"),
    };
    if let Some(parent) = pool.parent() {
        fs::create_dir_all(parent).context("cannot create recipe scratch parent directory")?;
    }
    match fs::DirBuilder::new().mode(0o700).create(&pool) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("cannot create recipe scratch pool"),
    }
    let metadata = fs::symlink_metadata(&pool)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
        bail!(
            "recipe scratch pool {} must be a private directory owned by the coordinator; \
             set RECIPE_SCRATCH_DIR to use another",
            pool.display()
        );
    }
    Ok(pool)
}

#[derive(Debug)]
pub(crate) struct TemporaryDirectory {
    directory: tempfile::TempDir,
    data: std::path::PathBuf,
    // Coordinator owns this lease through launcher teardown. Ginux closes
    // inherited duplicates before workload; they do not extend its lifetime.
    lease: File,
}

impl TemporaryDirectory {
    pub(crate) fn new() -> Result<Self> {
        let pool = pool()?;
        let guard = crate::fsx::open_lease(&pool.join("pool.lock"), true)?;
        guard
            .lock_exclusive()
            .context("cannot lock recipe scratch pool")?;
        for entry in fs::read_dir(&pool)? {
            let entry = entry?;
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with("invocation-")
                || !entry.file_type()?.is_dir()
            {
                continue;
            }
            let path = entry.path();
            let lease = match crate::fsx::open_lease(&path.join("lease"), false) {
                Ok(file) => Some(file),
                // Only creation holds the pool lock. A lease-less directory
                // encountered here was interrupted before its lease was made.
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
                {
                    None
                }
                Err(error) => return Err(error),
            };
            if let Some(lease) = &lease {
                match lease.try_lock_exclusive() {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                    Err(error) => return Err(error).context("cannot inspect recipe scratch lease"),
                }
            }
            crate::fsx::make_removable(&path);
            match fs::remove_dir_all(&path) {
                Ok(()) => {}
                // Normal Drop may finish removing an entry after read_dir.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                // An earlier invocation's leftovers never fail this one.
                Err(error) => crate::diagnostics::diag!(
                    "recipe: warning: cannot remove abandoned scratch {}: {error}",
                    path.display()
                ),
            }
        }
        let directory = tempfile::Builder::new()
            .prefix("invocation-")
            .tempdir_in(&pool)?;
        let lease = crate::fsx::open_lease(&directory.path().join("lease"), true)?;
        lease.lock_exclusive()?;
        let data = directory.path().join("data");
        fs::create_dir(&data)?;
        drop(guard);
        Ok(Self {
            directory,
            data,
            lease,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.data
    }

    pub(crate) fn lease_fd(&self) -> RawFd {
        self.lease.as_raw_fd()
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        crate::fsx::make_removable(self.directory.path());
    }
}
