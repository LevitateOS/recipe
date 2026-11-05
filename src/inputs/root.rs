//! Host-owned, per-invocation filesystem for the configured Ginux launcher.
//! An assembled image tar is selected by the coordinator and verified against
//! the content digest in domain.root. No recipe code runs during preparation.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::PreparedInvocation;

const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_IMAGE_ENTRIES: usize = 500_000;
const MAX_WORKER_BYTES: u64 = 512 * 1024 * 1024;

/// Owns the isolated root until the launcher and worker have been reaped.
pub(crate) struct PreparedRoot {
    directory: crate::temporary::TemporaryDirectory,
    pub(crate) worker_digest: String,
}

impl PreparedRoot {
    pub(crate) fn lease_fd(&self) -> std::os::fd::RawFd {
        self.directory.lease_fd()
    }
    pub(crate) fn path(&self) -> &Path {
        self.directory.path()
    }
}

pub(crate) fn prepare_root(invocation: &PreparedInvocation) -> Result<PreparedRoot> {
    if std::env::consts::OS != "linux" || std::env::consts::ARCH != "x86_64" {
        bail!("Ginux execution currently requires Linux x86_64");
    }
    let digest = root_digest(&invocation.root_reference)?;
    let archive_path = match std::env::var_os("GINUX_ROOTFS") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        Some(_) => bail!("GINUX_ROOTFS is empty; select a verified assembled rootfs tar"),
        None => dirs::cache_dir()
            .context("cannot locate the image cache; set GINUX_ROOTFS to an assembled rootfs tar")?
            .join("recipe/images")
            .join(format!("{digest}.tar")),
    };
    // Copy and hash the same bytes into an unlinked snapshot. Extraction must
    // not reread a mutable source pathname after verifying a different read.
    let mut archive = regular_file(&archive_path).with_context(|| {
        format!(
            "cannot open Ginux rootfs image {}; set GINUX_ROOTFS to the assembled tar matching {}",
            archive_path.display(),
            invocation.root_reference
        )
    })?;
    let mut snapshot = tempfile::tempfile().context("cannot snapshot the rootfs image")?;
    let (_, actual) = copy_hashed(&mut archive, &mut snapshot, MAX_ARCHIVE_BYTES)?;
    if actual != digest {
        bail!("Ginux rootfs digest mismatch: expected {digest}, got {actual}");
    }
    snapshot.seek(SeekFrom::Start(0))?;
    let mut root = PreparedRoot {
        worker_digest: String::new(),
        directory: crate::temporary::TemporaryDirectory::new()?,
    };
    unpack_image(snapshot, root.path())?;

    let inputs = root.path().join("inputs");
    std::fs::create_dir(&inputs)?;
    for (index, input) in invocation.inputs.iter().enumerate() {
        super::discover::validate_input_name(&input.name)?;
        ensure_parents(&inputs, Path::new(&input.name))?;
        match input.kind {
            super::InputKind::Directory => {
                let path = inputs.join(&input.name);
                if !path.exists() {
                    std::fs::create_dir(&path)?;
                }
                if !std::fs::symlink_metadata(&path)?.is_dir() {
                    bail!("input directory collision: {}", input.name);
                }
                continue;
            }
            super::InputKind::Symlink(_) => continue, // Install links after all ordinary paths.
            super::InputKind::File => {}
        }
        let mut source = regular_file(&invocation.staging_dir().join(format!("input-{index}")))?;
        let mut destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(inputs.join(&input.name))?;
        let (size, digest) = copy_hashed(&mut source, &mut destination, input.size)?;
        if size != input.size || digest != hex::encode(input.sha256) {
            bail!(
                "staged input changed after discovery: /inputs/{}",
                input.name
            );
        }
        // Explicitly imported bytes must be readable after Ginux drops to its
        // domain uid, even when the coordinator read a host-private 0600 file.
        destination.set_permissions(std::fs::Permissions::from_mode(
            0o444 | (input.mode & 0o111),
        ))?;
    }
    for input in &invocation.inputs {
        if let super::InputKind::Symlink(target) = &input.kind {
            ensure_parents(&inputs, Path::new(&input.name))?;
            std::os::unix::fs::symlink(target, inputs.join(&input.name))?;
        }
    }
    let worker_path = worker_binary()?;
    let mut worker = regular_file(&worker_path)
        .with_context(|| format!("cannot open worker {}", worker_path.display()))?;
    let mut header = [0u8; 20];
    worker
        .read_exact(&mut header)
        .context("worker is not an x86_64 ELF executable")?;
    if &header[..7] != b"\x7fELF\x02\x01\x01" || header[18..20] != [62, 0] {
        bail!("worker must be a Linux x86_64 ELF executable");
    }
    worker.seek(SeekFrom::Start(0))?;
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(inputs.join("recipe-worker"))?;
    root.worker_digest = copy_hashed(&mut worker, &mut destination, MAX_WORKER_BYTES)?.1;
    destination.set_permissions(std::fs::Permissions::from_mode(0o555))?;

    // No shared cached tree or mutable hardlinks are handed to the domain.
    // Write syscalls still require Ginux storage mediation; these directories
    // do not grant native host filesystem access.
    for path in [
        "work",
        "work/build",
        super::TOOLS_DOMAIN_PATH.trim_start_matches('/'),
        "work/tmp",
        "out",
    ] {
        let directory = root.path().join(path);
        std::fs::create_dir(&directory)?;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o777))?;
    }
    for entry in walkdir::WalkDir::new(&inputs).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_dir() {
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o555))?;
        }
    }
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o555))?;
    Ok(root)
}

fn root_digest(reference: &str) -> Result<&str> {
    let (image, digest) = reference.split_once("@sha256:").context(
        "domain.root must identify the assembled rootfs tar as oci:NAME@sha256:CONTENT_DIGEST",
    )?;
    if !image.starts_with("oci:")
        || image.len() <= 4
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!(
            "domain.root must identify the assembled rootfs tar as oci:NAME@sha256:CONTENT_DIGEST"
        );
    }
    Ok(digest)
}

fn worker_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("RECIPE_WORKER_BIN") {
        if path.is_empty() {
            bail!("RECIPE_WORKER_BIN is empty");
        }
        return PathBuf::from(path)
            .canonicalize()
            .context("cannot resolve RECIPE_WORKER_BIN");
    }
    let current = std::env::current_exe().context("cannot locate the Recipe worker")?;
    if current.file_name().is_some_and(|name| name == "recipe") {
        return Ok(current);
    }
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = directory.join("recipe");
        if candidate.is_file() && candidate.metadata()?.permissions().mode() & 0o111 != 0 {
            return candidate
                .canonicalize()
                .context("cannot resolve installed Recipe worker");
        }
    }
    bail!("cannot locate the Recipe worker; install recipe on PATH or set RECIPE_WORKER_BIN")
}

fn regular_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        bail!("not a regular file: {}", path.display());
    }
    Ok(file)
}

fn copy_hashed(source: &mut File, destination: &mut File, maximum: u64) -> Result<(u64, String)> {
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .context("image size overflow")?;
        if size > maximum {
            bail!("input exceeds preparation size limit of {maximum} bytes");
        }
        hash.update(&buffer[..count]);
        destination.write_all(&buffer[..count])?;
    }
    Ok((size, hex::encode(hash.finalize())))
}

fn relative_path(path: &Path) -> Result<PathBuf> {
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => relative.push(name),
            _ => bail!("unsafe rootfs image path: {}", path.display()),
        }
    }
    if relative.as_os_str().len() > 4096 {
        bail!("rootfs image path exceeds 4096 bytes");
    }
    Ok(relative)
}

fn ensure_parents(root: &Path, relative: &Path) -> Result<()> {
    let mut directory = root.to_path_buf();
    for component in relative.parent().unwrap_or(Path::new("")).components() {
        directory.push(component);
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => bail!(
                "rootfs image entry traverses a non-directory: {}",
                relative.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&directory)?
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn unpack_image(snapshot: File, root: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(snapshot);
    let mut paths = HashSet::new();
    let mut directories = HashMap::new();
    let mut total = 0u64;
    for (index, entry) in archive.entries()?.enumerate() {
        if index >= MAX_IMAGE_ENTRIES {
            bail!("rootfs image entry limit exceeded");
        }
        let mut entry = entry?;
        let relative = relative_path(&entry.path()?)?;
        let kind = entry.header().entry_type();
        if relative.as_os_str().is_empty() && kind.is_dir() {
            continue;
        }
        if relative.as_os_str().is_empty() {
            bail!("empty rootfs image entry");
        }
        if ["inputs", "work", "out"]
            .iter()
            .any(|name| relative.starts_with(name))
        {
            bail!(
                "rootfs image uses reserved invocation path: {}",
                relative.display()
            );
        }
        if !paths.insert(relative.clone()) {
            bail!("duplicate rootfs image path: {}", relative.display());
        }
        ensure_parents(root, &relative)?;
        let destination = root.join(&relative);
        if kind.is_dir() {
            match std::fs::symlink_metadata(&destination) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => bail!(
                    "rootfs directory conflicts with a file: {}",
                    relative.display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(&destination)?
                }
                Err(error) => return Err(error.into()),
            }
            directories.insert(destination, entry.header().mode()? & 0o555);
        } else if kind.is_file() {
            total = total
                .checked_add(entry.size())
                .context("rootfs image size overflow")?;
            if total > MAX_IMAGE_BYTES {
                bail!("rootfs unpacked size limit exceeded");
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            let count = std::io::copy(&mut entry, &mut file)?;
            if count != entry.size() {
                bail!("truncated rootfs image entry: {}", relative.display());
            }
            file.set_permissions(std::fs::Permissions::from_mode(
                entry.header().mode()? & 0o555,
            ))?;
        } else if kind.is_symlink() {
            let target = entry.link_name()?.context("rootfs symlink has no target")?;
            // Absolute symlinks are meaningful inside chroot (e.g. /proc/mounts).
            // They are never traversed while constructing or cleaning the root.
            if target.as_os_str().is_empty() || target.as_os_str().len() > 4096 {
                bail!("invalid rootfs symlink target");
            }
            std::os::unix::fs::symlink(target, destination)?;
        } else if kind.is_hard_link() {
            // Copy regular content, never alias the immutable image to a
            // mutable inode. Reject symlink ancestors and nonregular targets.
            let target = relative_path(
                &entry
                    .link_name()?
                    .context("rootfs hardlink has no target")?,
            )?;
            ensure_parents(root, &target)?;
            let mut source = regular_file(&root.join(target))?;
            total = total
                .checked_add(source.metadata()?.len())
                .context("rootfs image size overflow")?;
            if total > MAX_IMAGE_BYTES {
                bail!("rootfs unpacked size limit exceeded");
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)?;
            copy_hashed(&mut source, &mut file, MAX_IMAGE_BYTES)?;
            file.set_permissions(source.metadata()?.permissions())?;
        } else {
            bail!(
                "unsupported rootfs image entry type at {}",
                relative.display()
            );
        }
    }
    // Include implicitly-created parent directories too.
    let mut image_directories = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    image_directories.sort_by_key(|entry| std::cmp::Reverse(entry.depth()));
    for entry in image_directories {
        if entry.depth() > 0 && entry.file_type().is_dir() {
            let mode = directories.get(entry.path()).copied().unwrap_or(0o555);
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(())
}
