//! Host-owned, per-invocation filesystem for the configured Ginux launcher.
//! An assembled image tar is selected by the coordinator and verified against
//! the content digest in domain.root. No recipe code runs during preparation.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::PreparedInvocation;

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
    let mut archive = crate::fsx::open_regular(&archive_path).with_context(|| {
        format!(
            "cannot open Ginux rootfs image {}; set GINUX_ROOTFS to the assembled tar matching {}",
            archive_path.display(),
            invocation.root_reference
        )
    })?;
    let mut snapshot = tempfile::tempfile().context("cannot snapshot the rootfs image")?;
    let (_, actual) =
        crate::fsx::copy_hashed(&mut archive, &mut snapshot, crate::image::MAX_ARCHIVE_BYTES)?;
    if actual != digest {
        bail!("Ginux rootfs digest mismatch: expected {digest}, got {actual}");
    }
    snapshot.seek(SeekFrom::Start(0))?;
    let mut root = PreparedRoot {
        worker_digest: String::new(),
        directory: crate::temporary::TemporaryDirectory::new()?,
    };
    unpack_image(snapshot, root.path())?;

    super::network::install_hosts(&invocation.network, root.path())?;
    let inputs = root
        .path()
        .join(crate::domain::INPUTS.trim_start_matches('/'));
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
            super::InputKind::Hardlink(_) => continue,
            super::InputKind::Symlink(_) => continue, // Install links after all ordinary paths.
            super::InputKind::File => {}
        }
        let mut source =
            crate::fsx::open_regular(&invocation.staging_dir().join(format!("input-{index}")))?;
        let mut destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(inputs.join(&input.name))?;
        let (size, digest) = crate::fsx::copy_hashed(&mut source, &mut destination, input.size)?;
        if size != input.size || digest != hex::encode(input.sha256) {
            bail!(
                "staged input changed after discovery: /inputs/{}",
                input.name
            );
        }
        // Explicitly imported bytes must be readable after Ginux drops to its
        // domain uid, even when the coordinator read a host-private 0600 file.
        destination.set_permissions(std::fs::Permissions::from_mode(input.mode | 0o444))?;
    }
    for input in &invocation.inputs {
        if let super::InputKind::Hardlink(target) = &input.kind {
            super::discover::validate_input_name(target)?;
            ensure_parents(&inputs, Path::new(&input.name))?;
            let target = inputs.join(target);
            if !std::fs::symlink_metadata(&target)?.is_file() {
                bail!("input hardlink target is not regular");
            }
            std::fs::hard_link(target, inputs.join(&input.name))?;
        }
    }
    for input in &invocation.inputs {
        if let super::InputKind::Symlink(target) = &input.kind {
            ensure_parents(&inputs, Path::new(&input.name))?;
            std::os::unix::fs::symlink(target, inputs.join(&input.name))?;
        }
    }
    let worker_path = worker_binary()?;
    let mut worker = crate::fsx::open_regular(&worker_path)
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
        .open(inputs.join(crate::domain::WORKER_NAME))?;
    root.worker_digest =
        crate::fsx::copy_hashed(&mut worker, &mut destination, MAX_WORKER_BYTES)?.1;
    destination.set_permissions(std::fs::Permissions::from_mode(0o555))?;

    // No shared cached tree or mutable hardlinks are handed to the domain.
    // Write syscalls still require Ginux storage mediation; these directories
    // do not grant native host filesystem access.
    for path in crate::domain::WRITABLE_DIRS {
        let directory = root.path().join(path.trim_start_matches('/'));
        std::fs::create_dir(&directory)?;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o777))?;
    }
    // Preserve logical input permissions independently from the readability
    // required to consume immutable inputs. Tree copying uses this coordinator-
    // authored metadata instead of publishing the snapshot's read grants.
    let modes = invocation
        .inputs
        .iter()
        .map(|input| (input.name.clone(), input.mode))
        .collect::<crate::domain::InputModes>();
    let manifest = inputs.join(crate::domain::INPUT_MODES_NAME);
    let modes = serde_json::to_vec(&modes)?;
    if modes.len() as u64 > crate::domain::MAX_INPUT_MODES_BYTES {
        bail!("input permission manifest exceeds its bound");
    }
    std::fs::write(&manifest, modes)?;
    std::fs::set_permissions(manifest, std::fs::Permissions::from_mode(0o444))?;
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
    let mut validator = crate::image::Validator::default();
    let mut directories = HashMap::new();
    for entry in archive.entries()? {
        crate::cancellation::check()?;
        let mut entry = entry?;
        let relative = crate::image::member_path(&entry.path()?)?;
        let kind = entry.header().entry_type();
        if relative.as_os_str().is_empty() && kind.is_dir() {
            continue;
        }
        if relative.as_os_str().is_empty() {
            bail!("empty rootfs image entry");
        }
        let link = entry.link_name()?.map(|link| link.into_owned());
        validator.check(
            &relative,
            kind,
            entry.size(),
            entry.header().mode()?,
            link.as_deref(),
        )?;
        let mode = crate::image::installed_mode(entry.header().mode()?);
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
            directories.insert(destination, mode);
        } else if kind.is_file() {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            let count = std::io::copy(&mut entry, &mut file)?;
            if count != entry.size() {
                bail!("truncated rootfs image entry: {}", relative.display());
            }
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        } else if kind.is_symlink() {
            // Absolute symlinks are meaningful inside chroot (e.g. /proc/mounts).
            // They are never traversed while constructing or cleaning the root.
            std::os::unix::fs::symlink(link.expect("validated symlink target"), destination)?;
        } else {
            // A hardlink aliases an earlier verified regular file in this
            // private snapshot; host sources and shared caches stay separate.
            let target = crate::image::member_path(&link.expect("validated hardlink target"))?;
            ensure_parents(root, &target)?;
            std::fs::hard_link(root.join(target), destination)?;
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

/// Seed the owned target from explicitly selected, already verified input bytes.
/// Read only the invocation snapshot, never the original host input pathname.
pub(crate) fn seed_target(
    invocation: &PreparedInvocation,
    root: &Path,
    destination: &Path,
) -> Result<bool> {
    use super::InputKind;
    let selected: Vec<_> = invocation
        .inputs
        .iter()
        .filter(|input| input.name == "target" || input.name.starts_with("target/"))
        .collect();
    if selected.is_empty() {
        return Ok(false);
    }
    if !selected
        .iter()
        .any(|input| input.name == "target" && input.kind == InputKind::Directory)
    {
        bail!("target input must be a directory tree");
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut archive = tar::Builder::new(file);
    for input in selected {
        let suffix = input.name.strip_prefix("target").unwrap();
        let name = format!("out{suffix}");
        let mut header = tar::Header::new_ustar();
        header.set_mode(input.mode);
        header.set_uid(crate::domain::UID);
        header.set_gid(crate::domain::UID);
        header.set_mtime(0);
        match &input.kind {
            InputKind::Directory => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_cksum();
                archive.append_data(&mut header, name, std::io::empty())?;
            }
            InputKind::Symlink(target) => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_cksum();
                archive.append_link(&mut header, name, target)?;
            }
            InputKind::Hardlink(target) if target == "target" || target.starts_with("target/") => {
                header.set_entry_type(tar::EntryType::Link);
                header.set_size(0);
                let link = format!("out{}", target.strip_prefix("target").unwrap());
                archive.append_link(&mut header, name, link)?;
            }
            InputKind::File | InputKind::Hardlink(_) => {
                let mut file = crate::fsx::open_regular(
                    &root
                        .join(crate::domain::INPUTS.trim_start_matches('/'))
                        .join(&input.name),
                )?;
                if file.metadata()?.len() != input.size {
                    bail!("target snapshot size changed: {}", input.name);
                }
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(input.size);
                header.set_cksum();
                archive.append_data(&mut header, name, &mut file)?;
            }
        }
    }
    archive.finish()?;
    let file = archive.into_inner()?;
    file.sync_all()?;
    Ok(true)
}
