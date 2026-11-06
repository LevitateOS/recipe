//! Filesystem primitives the coordinator's stores and the image builder share.
//!
//! Each rule has one implementation here: open without following links,
//! private directories, removable trees, leases and bounded hashed copies.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Open `path` for reading without following a final symlink or blocking on a
/// FIFO, and require a regular file.
pub fn open_regular(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    if !file.metadata()?.is_file() {
        bail!("not a regular file: {}", path.display());
    }
    Ok(file)
}

/// Open (optionally creating) a private 0600 lease file, without following links.
pub fn open_lease(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        bail!("lease is not a regular file: {}", path.display());
    }
    Ok(file)
}

/// Create `path` as a 0700 directory, or accept an existing real directory
/// (never a symlink to one). With `durable`, the parent is fsynced after creation.
pub fn private_dir(path: &Path, durable: bool) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {
            if durable {
                File::open(path.parent().context("private directory has no parent")?)?
                    .sync_all()?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(path)?.file_type().is_dir() {
                Ok(())
            } else {
                bail!("not a real directory: {}", path.display())
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// Make every directory under `path` owner-writable so the tree can be removed.
/// Links are never followed. Best effort: removal reports what still fails.
pub fn make_removable(path: &Path) {
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

/// Copy at most `maximum` bytes from `input` to `output`, returning the byte
/// count and sha256 hex. Exceeding the bound or a cancellation request fails.
pub fn copy_hashed(
    input: &mut impl Read,
    output: &mut impl Write,
    maximum: u64,
) -> Result<(u64, String)> {
    let mut buffer = vec![0u8; 64 * 1024];
    let mut count = 0u64;
    let mut hash = Sha256::new();
    loop {
        crate::cancellation::check()?;
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .filter(|count| *count <= maximum)
            .with_context(|| format!("copy exceeds its bound of {maximum} bytes"))?;
        hash.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
    }
    Ok((count, hex::encode(hash.finalize())))
}
