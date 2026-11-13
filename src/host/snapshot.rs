//! A host run's snapshot and export archive: `work` and `out` written to and
//! restored from the tar format a Ginux snapshot uses (members `work/…`,
//! `out/…`).
//!
//! Both directions walk by descriptor (`crate::fsx`): every directory is
//! opened with `O_NOFOLLOW` below the one before, every entry is examined
//! through its own `O_PATH | O_NOFOLLOW` descriptor, and every file is opened
//! `O_NOFOLLOW | O_NONBLOCK` and type-checked after the open. A worker that
//! keeps changing its tree while the coordinator reads it (root reading an
//! unprivileged worker's files) can make the archive fail, never make it
//! include anything outside `work` and `out`.

use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::fsx::{check, cstring, list_at, lstat_at, open_dir, open_dir_at, openat, read_link_fd};

/// Check the storage bounds over work and out without keeping an archive.
pub(super) fn check_storage(root: &Path, options: &crate::ExecutionOptions) -> Result<()> {
    write_snapshot_to(root, std::io::sink(), options)
}

/// Archive `work` and `out` into `destination`, in the snapshot and export
/// format: members `work/…` and `out/…`, regular files, directories, symbolic
/// links and hard links, refusing anything else and anything over the
/// storage bounds. Modes keep their permission bits only: a host run makes
/// no setuid, setgid or sticky entries (the target manifest carries such
/// modes as data).
pub(super) fn write_snapshot(
    root: &Path,
    destination: &Path,
    options: &crate::ExecutionOptions,
) -> Result<()> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(destination)?;
    write_snapshot_to(root, BufWriter::new(file), options)
}

struct Archive<W: Write> {
    builder: tar::Builder<W>,
    bytes: u64,
    objects: u64,
    max_bytes: u64,
    max_objects: u64,
    inodes: HashMap<(u64, u64), String>,
}

fn write_snapshot_to(
    root: &Path,
    output: impl Write,
    options: &crate::ExecutionOptions,
) -> Result<()> {
    let mut archive = Archive {
        builder: tar::Builder::new(output),
        bytes: 0,
        objects: 0,
        max_bytes: options.storage_byte_bound(),
        max_objects: options.storage_max_objects,
        inodes: HashMap::new(),
    };
    let root = open_dir(root).with_context(|| format!("cannot open {}", root.display()))?;
    for top in crate::domain::WRITABLE_ROOTS {
        archive_entry(root.as_fd(), &cstring(top.as_bytes())?, top, &mut archive)?;
    }
    archive.builder.into_inner()?.flush()?;
    Ok(())
}

fn changed(name: &str) -> anyhow::Error {
    anyhow::anyhow!("{name} changed while it was archived")
}

/// Why opening the member `name` (a `kind`: "directory" or "file") failed: it
/// was replaced or removed under the walk (a link, a non-directory, gone), or
/// a real error, passed on with what to do.
fn open_failed(name: &str, kind: &str, error: std::io::Error) -> anyhow::Error {
    match error.raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR | libc::ENOENT) => changed(name),
        Some(libc::EACCES | libc::EPERM) => {
            let rule = if kind == "directory" {
                "its directories in work and out readable and searchable by itself (mode 0700 or more)"
            } else {
                "its files in work and out readable by itself (mode 0400 or more)"
            };
            anyhow::anyhow!("cannot read {name} (permission denied): a recipe must leave {rule}")
        }
        _ if error.kind() == std::io::ErrorKind::InvalidInput => changed(name),
        _ => anyhow::Error::new(error).context(format!("cannot read {name}")),
    }
}

/// Exactly `remaining` bytes of a file, failing if it ends sooner (it shrank
/// after its length went into the member's header).
struct Exact<'a> {
    file: std::io::Take<File>,
    remaining: u64,
    member: &'a str,
}

impl std::io::Read for Exact<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.file.read(buffer)?;
        if count == 0 && self.remaining > 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("{} changed while it was archived (it shrank)", self.member),
            ));
        }
        self.remaining -= count as u64;
        Ok(count)
    }
}

/// Archive the entry `name` below `dir` as the member `member`, and a
/// directory's entries after it, in name order.
fn archive_entry<W: Write>(
    dir: BorrowedFd<'_>,
    name: &CStr,
    member: &str,
    archive: &mut Archive<W>,
) -> Result<()> {
    crate::cancellation::check()?;
    let (entry, metadata) =
        lstat_at(dir, name).with_context(|| format!("cannot inspect {member}"))?;
    archive.objects += 1;
    if archive.objects > archive.max_objects {
        bail!(
            "storage limit: work and out hold more than NEEDS.storage_max_objects = {} objects",
            archive.max_objects
        );
    }
    let mut header = tar::Header::new_gnu();
    header.set_mode(metadata.mode() & 0o777);
    header.set_uid(crate::domain::UID);
    header.set_gid(crate::domain::UID);
    header.set_mtime(metadata.mtime().max(0) as u64);
    let kind = metadata.file_type();
    if kind.is_dir() {
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        archive
            .builder
            .append_data(&mut header, member, std::io::empty())?;
        let directory =
            open_dir_at(dir, name).map_err(|error| open_failed(member, "directory", error))?;
        let opened = File::from(directory.try_clone()?).metadata()?;
        if (opened.dev(), opened.ino()) != (metadata.dev(), metadata.ino()) {
            return Err(changed(member));
        }
        for child in list_at(directory.as_fd())? {
            let text = child
                .to_str()
                .with_context(|| format!("a path in {member} is not UTF-8"))?;
            archive_entry(
                directory.as_fd(),
                &child,
                &format!("{member}/{text}"),
                archive,
            )?;
        }
    } else if kind.is_symlink() {
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        let target = read_link_fd(entry.as_fd())?;
        archive.builder.append_link(&mut header, member, target)?;
    } else if kind.is_file() {
        let inode = (metadata.dev(), metadata.ino());
        if metadata.nlink() > 1
            && let Some(first) = archive.inodes.get(&inode)
        {
            header.set_entry_type(tar::EntryType::Link);
            header.set_size(0);
            let first = first.clone();
            archive.builder.append_link(&mut header, member, first)?;
            return Ok(());
        }
        archive.bytes = archive.bytes.saturating_add(metadata.len());
        if archive.bytes > archive.max_bytes {
            bail!(
                "storage limit: work and out hold more than {} bytes (NEEDS.storage_max_bytes, or memory_mib)",
                archive.max_bytes
            );
        }
        let file = crate::fsx::open_regular_at(dir, name)
            .map_err(|error| open_failed(member, "file", error))?;
        let opened = file.metadata()?;
        if (opened.dev(), opened.ino()) != inode || opened.len() != metadata.len() {
            return Err(changed(member));
        }
        if metadata.nlink() > 1 {
            archive.inodes.insert(inode, member.to_owned());
        }
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(metadata.len());
        let length = metadata.len();
        let exact = Exact {
            file: std::io::Read::take(file, length),
            remaining: length,
            member,
        };
        archive.builder.append_data(&mut header, member, exact)?;
    } else {
        bail!(
            "work and out may hold only files, directories and links; {member} is something else"
        );
    }
    Ok(())
}

/// The directory holding the member `path` (relative, plain names) below
/// `root`, opened by descriptor without following a link, and the member's
/// final name. With `create`, missing directories on the way are made (0755).
fn parent_at(root: BorrowedFd<'_>, path: &str, create: bool) -> Result<(OwnedFd, CString)> {
    let mut parts = path.split('/').collect::<Vec<_>>();
    let name = cstring(parts.pop().context("empty snapshot path")?.as_bytes())?;
    let mut current = openat(root, c".", libc::O_PATH | libc::O_DIRECTORY, 0)?;
    for part in parts {
        let part = cstring(part.as_bytes())?;
        let flags = libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW;
        current = match openat(current.as_fd(), &part, flags, 0) {
            Ok(next) => next,
            Err(error) if create && error.kind() == std::io::ErrorKind::NotFound => {
                check(unsafe { libc::mkdirat(current.as_raw_fd(), part.as_ptr(), 0o755) })?;
                let next = openat(current.as_fd(), &part, flags, 0)?;
                crate::fsx::chmod_fd(next.as_fd(), 0o755)?;
                next
            }
            Err(error) if matches!(error.raw_os_error(), Some(libc::ENOTDIR | libc::ELOOP)) => {
                bail!("snapshot entry {path} passes through a non-directory")
            }
            Err(error) => return Err(error.into()),
        };
    }
    Ok((current, name))
}

fn mtime(seconds: u64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds)
}

/// Recreate `work` and `out` under `root` from a snapshot archive (a saved
/// generation or the seeded target), by descriptor, never following a link.
pub(super) fn unpack_snapshot(archive: &Path, root: &Path) -> Result<()> {
    let mut archive =
        tar::Archive::new(std::io::BufReader::new(crate::fsx::open_regular(archive)?));
    let root = open_dir(root).with_context(|| format!("cannot open {}", root.display()))?;
    let mut directories = Vec::<(String, u32, u64)>::new();
    for entry in archive.entries()? {
        crate::cancellation::check()?;
        let mut entry = entry?;
        let path = entry
            .path()?
            .to_str()
            .context("snapshot path is not UTF-8")?
            .to_owned();
        let path = path.strip_suffix('/').unwrap_or(&path).to_owned();
        crate::domain::validate_export_path(&path)?;
        let (parent, name) = parent_at(root.as_fd(), &path, true)?;
        let kind = entry.header().entry_type();
        // Permission bits only, as `write_snapshot_to` archives them.
        let mode = entry.header().mode()? & 0o777;
        let modified = entry.header().mtime()?;
        let existing = lstat_at(parent.as_fd(), &name);
        if kind.is_dir() {
            let directory = match existing {
                Ok((directory, metadata)) if metadata.is_dir() => directory,
                Ok(_) => bail!("snapshot directory {path} conflicts with another entry"),
                Err(_) => {
                    check(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) })?;
                    lstat_at(parent.as_fd(), &name)?.0
                }
            };
            crate::fsx::chmod_fd(directory.as_fd(), 0o700)?;
            directories.push((path, mode, modified));
            continue;
        }
        anyhow::ensure!(existing.is_err(), "snapshot entry {path} appears twice");
        if kind.is_file() {
            let mut file = File::from(openat(
                parent.as_fd(),
                &name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
                0o600,
            )?);
            std::io::copy(&mut entry, &mut file)?;
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
            file.set_modified(mtime(modified))?;
        } else if kind.is_symlink() {
            let target = entry
                .link_name()?
                .context("snapshot link without a target")?;
            let target = cstring(target.as_os_str().as_encoded_bytes())?;
            check(unsafe { libc::symlinkat(target.as_ptr(), parent.as_raw_fd(), name.as_ptr()) })?;
        } else if kind.is_hard_link() {
            let target = entry
                .link_name()?
                .context("snapshot hard link without a target")?
                .to_str()
                .context("snapshot hard link target is not UTF-8")?
                .to_owned();
            crate::domain::validate_export_path(&target)?;
            let (source_parent, source_name) = parent_at(root.as_fd(), &target, true)?;
            anyhow::ensure!(
                lstat_at(source_parent.as_fd(), &source_name)
                    .is_ok_and(|(_, metadata)| metadata.is_file()),
                "snapshot hard link {path} names no earlier file"
            );
            check(unsafe {
                libc::linkat(
                    source_parent.as_raw_fd(),
                    source_name.as_ptr(),
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    0,
                )
            })?;
        } else {
            bail!("unsupported snapshot entry kind at {path}");
        }
    }
    for (path, mode, modified) in directories.into_iter().rev() {
        let (parent, name) = parent_at(root.as_fd(), &path, false)?;
        let directory = File::from(open_dir_at(parent.as_fd(), &name)?);
        directory.set_modified(mtime(modified))?;
        directory.set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}
