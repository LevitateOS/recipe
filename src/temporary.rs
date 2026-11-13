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

/// Where a host run's directories live (`crate::host`). For an ordinary
/// user, the [`pool`]. For root, whose worker runs as an unprivileged user
/// that could not traverse a pool under `/root`, the root-owned
/// `/var/lib/recipe/host-runs`, mode 0711: anyone may pass through it, no one
/// may list it, and each run's own directory in it stays root's (0700, or
/// 0710 for the run's own group while its worker runs).
pub(crate) fn host_pool() -> Result<PathBuf> {
    if unsafe { libc::geteuid() } != 0 {
        return pool();
    }
    let base = Path::new(HOST_POOL_PARENT);
    for directory in [base.parent().context("host pool has no parent")?, base] {
        match fs::DirBuilder::new().mode(0o755).create(directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot create {}", directory.display()));
            }
        }
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            bail!(
                "{} must be a directory owned by root that only root can write",
                directory.display()
            );
        }
    }
    let pool = base.join("host-runs");
    match fs::DirBuilder::new().mode(0o711).create(&pool) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("cannot create the host run pool"),
    }
    let metadata = fs::symlink_metadata(&pool)?;
    if !metadata.is_dir() || metadata.uid() != 0 {
        bail!(
            "host run pool {} must be a directory owned by root",
            pool.display()
        );
    }
    // The mode is set rather than checked: a umask may have narrowed it.
    fs::set_permissions(&pool, std::os::unix::fs::PermissionsExt::from_mode(0o711))?;
    Ok(pool)
}

/// The directory a root coordinator keeps its host runs in (see [`host_pool`]).
const HOST_POOL_PARENT: &str = "/var/lib/recipe";

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
        Self::create(None)
    }

    /// A scratch directory with a fixed name in the pool, `invocation-NAME`:
    /// the same path on every run that asks for it, as a host run's saved
    /// state needs (`crate::host`). The caller excludes concurrent users of
    /// the name (the destination lock); an abandoned one is removed first,
    /// like every abandoned scratch directory.
    /// It is made in the [`host_pool`], which is the scratch pool except
    /// for root.
    pub(crate) fn named_for_host(name: &str) -> Result<Self> {
        anyhow::ensure!(
            crate::domain::valid_name(name),
            "invalid scratch directory name {name:?}"
        );
        Self::create_in(host_pool()?, Some(name))
    }

    fn create(name: Option<&str>) -> Result<Self> {
        Self::create_in(pool()?, name)
    }

    fn create_in(pool: PathBuf, name: Option<&str>) -> Result<Self> {
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
            // Closed first: a host run's directory may have been left open
            // to its worker's group (`crate::host`). The pool is the
            // coordinator's alone, so this name cannot be replaced.
            let _ = fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o700));
            if unsafe { libc::geteuid() } == 0 {
                let _ = std::os::unix::fs::lchown(&path, Some(0), Some(0));
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
        let directory = match name {
            None => tempfile::Builder::new()
                .prefix("invocation-")
                .tempdir_in(&pool)?,
            Some(name) => tempfile::Builder::new()
                .prefix(&format!("invocation-{name}"))
                .rand_bytes(0)
                .tempdir_in(&pool)
                .with_context(|| {
                    format!("scratch directory invocation-{name} is in use by another run")
                })?,
        };
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

    /// The directory holding [`path`](Self::path) and the lease.
    pub(crate) fn container(&self) -> &Path {
        self.directory.path()
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
