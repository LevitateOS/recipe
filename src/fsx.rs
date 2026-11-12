//! Filesystem primitives the coordinator's stores and the image builder share.
//!
//! Each rule has one implementation here: open without following links,
//! private directories, removable trees, leases and bounded hashed copies.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
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

/// Make `output` a copy-on-write clone of all of `input` (`FICLONE`): a new
/// inode whose data extents are shared until either side writes. Returns
/// false, leaving `output` unchanged, when this pair of files cannot be cloned
/// (another filesystem, or one without reflinks).
pub fn reflink(input: &File, output: &File) -> Result<bool> {
    // SAFETY: both descriptors are open for the duration of the call.
    if unsafe { libc::ioctl(output.as_raw_fd(), libc::FICLONE, input.as_raw_fd()) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EOPNOTSUPP | libc::EXDEV | libc::EINVAL | libc::ENOTTY | libc::ENOSYS) => {
            Ok(false)
        }
        _ => Err(error).context("cannot clone file data"),
    }
}

/// Like [`copy_hashed`] from the start of `input` into an empty `output`
/// (open for reading and writing), but clone the data when the filesystem can
/// and hash the clone, read back through `output`, instead of writing a second
/// copy. Either way the returned digest describes exactly the
/// bytes `output` now holds, which a later change of `input` cannot alter.
pub fn snapshot_hashed(input: &mut File, output: &mut File, maximum: u64) -> Result<(u64, String)> {
    if input.metadata()?.len() <= maximum && reflink(input, output)? {
        output.seek(SeekFrom::Start(0))?;
        let hashed = copy_hashed(output, &mut std::io::sink(), maximum)?;
        output.seek(SeekFrom::End(0))?;
        return Ok(hashed);
    }
    copy_hashed(input, output, maximum)
}

/// Start writing `file`'s dirty pages back without waiting, so that a later
/// fsync of a whole batch overlaps their I/O instead of serializing it. Only a
/// hint: the fsync still provides the guarantee.
pub fn start_writeback(file: &File) {
    // SAFETY: plain syscall on an open descriptor; failure only loses the hint.
    unsafe {
        libc::sync_file_range(file.as_raw_fd(), 0, 0, libc::SYNC_FILE_RANGE_WRITE);
    }
}

/// Paths opened at once by [`sync_files`] and [`sync_new_directories`].
const SYNC_CHUNK: usize = 512;

/// Make the data and metadata of these regular files durable as a batch, and
/// only these: never the whole filesystem, so a busy neighbour's dirty data
/// does not lengthen the wait. Per chunk, every file's final mode (if given)
/// is applied and its writeback started before any wait, then all data is
/// awaited, and only then is each file fsynced: the first fsync commits the
/// journal for the whole chunk and the rest find nothing left to commit. A
/// file left unreadable to its owner by its mode is still synced.
pub fn sync_files(files: &[(&Path, Option<u32>)]) -> Result<()> {
    for chunk in files.chunks(SYNC_CHUNK) {
        let opened = chunk
            .iter()
            .map(|(path, mode)| {
                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(path)
                    .with_context(|| format!("cannot open {} to sync it", path.display()))?;
                if let Some(mode) = mode {
                    file.set_permissions(fs::Permissions::from_mode(*mode))?;
                }
                start_writeback(&file);
                Ok(file)
            })
            .collect::<Result<Vec<_>>>()?;
        for file in &opened {
            // SAFETY: plain syscall on an open descriptor; fsync below is
            // what guarantees durability, this only orders the waits.
            unsafe {
                libc::sync_file_range(
                    file.as_raw_fd(),
                    0,
                    0,
                    libc::SYNC_FILE_RANGE_WAIT_BEFORE
                        | libc::SYNC_FILE_RANGE_WRITE
                        | libc::SYNC_FILE_RANGE_WAIT_AFTER,
                );
            }
        }
        for file in &opened {
            file.sync_all()?;
        }
    }
    Ok(())
}

/// Make a new tree's directories durable, each given children before
/// parents with the mtime and mode to apply first (None keeps both). As in
/// [`sync_files`], a chunk's changes all precede its first fsync, so the
/// chunk costs one journal commit; the descriptors are opened before the
/// modes change, so a directory left unreadable to its owner is still synced.
pub fn sync_new_directories(
    directories: &[(&Path, Option<(u32, std::time::SystemTime)>)],
) -> Result<()> {
    for chunk in directories.chunks(SYNC_CHUNK) {
        let opened = chunk
            .iter()
            .map(|(path, change)| {
                let directory = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                    .open(path)
                    .with_context(|| format!("cannot open directory {}", path.display()))?;
                if let Some((mode, modified)) = change {
                    directory.set_modified(*modified)?;
                    directory.set_permissions(fs::Permissions::from_mode(*mode))?;
                }
                Ok(directory)
            })
            .collect::<Result<Vec<_>>>()?;
        for directory in &opened {
            directory.sync_all()?;
        }
    }
    Ok(())
}

/// Make these directories' entries durable, in the order given (children
/// before parents when a new tree is being committed).
pub fn sync_directories(directories: &[&Path]) -> Result<()> {
    for directory in directories {
        File::open(directory)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("cannot sync directory {}", directory.display()))?;
    }
    Ok(())
}
