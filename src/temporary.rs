//! Coordinator-owned scratch directories with leases for crash recovery.
//! A later invocation removes only abandoned directories in this private pool.
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

#[derive(Debug)]
pub(crate) struct TemporaryDirectory {
    directory: tempfile::TempDir,
    data: std::path::PathBuf,
    // Inherited by the supervisor, but closed by Ginux before guest execution.
    // Keep the lease until both coordinator and supervisor have released it.
    lease: File,
}

impl TemporaryDirectory {
    pub(crate) fn new() -> Result<Self> {
        let uid = unsafe { libc::geteuid() };
        let pool = std::env::temp_dir().join(format!("recipe-scratch-{uid}"));
        match fs::DirBuilder::new().mode(0o700).create(&pool) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("cannot create recipe scratch pool"),
        }
        let metadata = fs::symlink_metadata(&pool)?;
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
            bail!("recipe scratch pool must be a private directory owned by the coordinator");
        }
        let guard = lease_file(&pool.join("pool.lock"), true)?;
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
            let lease = match lease_file(&path.join("lease"), false) {
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
            make_removable(&path);
            match fs::remove_dir_all(&path) {
                Ok(()) => {}
                // Normal Drop may finish removing an entry after read_dir.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).context("cannot remove abandoned recipe scratch directory");
                }
            }
        }
        let directory = tempfile::Builder::new()
            .prefix("invocation-")
            .tempdir_in(&pool)?;
        let lease = lease_file(&directory.path().join("lease"), true)?;
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
        make_removable(self.directory.path());
    }
}

fn lease_file(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        bail!("recipe scratch lease is not a regular file");
    }
    Ok(file)
}

fn make_removable(path: &Path) {
    // chmod before descending so private/read-only image directories can be
    // removed; never follow image or source links during cleanup.
    for entry in walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .flatten()
    {
        if entry.file_type().is_dir() {
            let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o700));
        }
    }
}
