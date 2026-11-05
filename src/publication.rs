//! Validate a completed domain archive and publish only its named outputs.
use std::collections::{BTreeMap, HashSet};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;

struct Staging(tempfile::TempDir);
impl Drop for Staging {
    fn drop(&mut self) {
        for entry in walkdir::WalkDir::new(self.0.path())
            .follow_links(false)
            .into_iter()
            .flatten()
        {
            if entry.file_type().is_dir() {
                let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o700));
            }
        }
    }
}

pub(crate) fn publish(invocation: &str, archive: &Path, context: &mut Value) -> Result<()> {
    let Some(outputs) = context.get("outputs") else {
        return Ok(());
    };
    let outputs = outputs
        .as_object()
        .context("named outputs must be an object")?;
    if outputs.is_empty() {
        return Ok(());
    }
    if outputs.len() > 64 {
        bail!("too many named outputs");
    }
    let mut selected = BTreeMap::new();
    for (name, value) in outputs {
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            bail!("invalid output name {name:?}");
        }
        let path = value.as_str().context("output paths must be strings")?;
        let relative = path
            .strip_prefix('/')
            .context("output path must be absolute")?;
        validate_path(relative)?;
        selected.insert(name.clone(), relative.to_owned());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(archive)
        .context("cannot open completed Ginux export")?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES + 8 * 1024 * 1024 {
        bail!("invalid or oversized Ginux export");
    }
    let base = std::env::current_dir()?.join(".artifacts");
    private_directory(&base)?;
    let base = base.join("ginux-export");
    private_directory(&base)?;
    let base = base.canonicalize()?;
    let staging = Staging(
        tempfile::Builder::new()
            .prefix(".pending-")
            .tempdir_in(&base)?,
    );
    let mut seen = HashSet::new();
    let mut found = HashSet::new();
    let mut directories = BTreeMap::new();
    let mut bytes = 0u64;
    let mut published_bytes = 0u64;
    let mut archive = tar::Archive::new(file);
    let mut long_name = None;
    for (raw_count, entry) in archive.entries()?.raw(true).enumerate() {
        if raw_count >= MAX_ENTRIES * 2 {
            bail!("excessive export headers");
        }
        let mut entry = entry?;
        if entry.header().entry_type().is_gnu_longname() {
            if long_name.is_some() || !(2..=256).contains(&entry.size()) {
                bail!("invalid long export name");
            }
            let mut name = vec![0; entry.size() as usize];
            entry.read_exact(&mut name)?;
            if name.pop() != Some(0) {
                bail!("unterminated long export name");
            }
            long_name = Some(name);
            continue;
        }
        let path = String::from_utf8(
            long_name
                .take()
                .unwrap_or_else(|| entry.path_bytes().into_owned()),
        )
        .context("export path is not UTF-8")?;
        let path = path.strip_suffix('/').unwrap_or(&path);
        validate_path(path)?;
        if seen.len() >= MAX_ENTRIES || !seen.insert(path.to_owned()) {
            bail!("duplicate or excessive export entries");
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            bail!("unsupported export entry kind at {path}");
        }
        let size = entry.size();
        bytes = bytes.checked_add(size).context("export size overflow")?;
        if bytes > MAX_BYTES || (kind.is_dir() && size != 0) {
            bail!("invalid export size");
        }
        let mode = entry.header().mode()? & 0o777;
        let mut destinations = Vec::new();
        for (name, root) in &selected {
            let suffix = if path == root {
                found.insert(name.clone());
                Some("")
            } else {
                path.strip_prefix(root)
                    .and_then(|rest| rest.strip_prefix('/'))
            };
            if let Some(suffix) = suffix {
                let destination = staging.0.path().join(name);
                destinations.push(if suffix.is_empty() {
                    destination
                } else {
                    destination.join(suffix)
                });
            }
        }
        if destinations.is_empty() {
            continue;
        }
        if kind.is_dir() {
            for destination in destinations {
                fs::create_dir_all(&destination)?;
                directories.insert(destination, mode);
            }
        } else {
            published_bytes = published_bytes
                .checked_add(
                    size.checked_mul(destinations.len() as u64)
                        .context("output size overflow")?,
                )
                .context("output size overflow")?;
            if published_bytes > MAX_BYTES {
                bail!("selected outputs exceed byte limit");
            }
            let first = &destinations[0];
            fs::create_dir_all(first.parent().context("missing output parent")?)?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(first)?;
            if std::io::copy(&mut entry, &mut output)? != size {
                bail!("truncated export entry");
            }
            for destination in &destinations[1..] {
                fs::create_dir_all(destination.parent().context("missing output parent")?)?;
                let mut copy = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(destination)?;
                std::io::copy(&mut File::open(first)?, &mut copy)?;
                copy.set_permissions(fs::Permissions::from_mode(mode))?;
                copy.sync_all()?;
            }
            output.set_permissions(fs::Permissions::from_mode(mode))?;
            output.sync_all()?;
        }
    }
    if long_name.is_some() {
        bail!("long export name without an entry");
    }
    if found.len() != selected.len() {
        bail!("a declared output is absent from the completed domain export");
    }
    // Set directory modes only after children are materialized. No archive
    // symlink or hardlink is ever created, so parents stay within fresh staging.
    for (directory, mode) in directories.iter().rev() {
        File::open(directory)?.sync_all()?;
        fs::set_permissions(directory, fs::Permissions::from_mode(*mode))?;
    }
    File::open(staging.0.path())?.sync_all()?;
    let destination = base.join(invocation);
    let from = CString::new(staging.0.path().as_os_str().as_bytes())?;
    let to = CString::new(destination.as_os_str().as_bytes())?;
    // Never replace artifacts from another invocation, even under a collision.
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error()).context("cannot publish output directory");
    }
    File::open(&base)?.sync_all()?;
    let outputs = context.get_mut("outputs").unwrap().as_object_mut().unwrap();
    for name in selected.keys() {
        outputs.insert(
            name.clone(),
            Value::String(
                destination
                    .join(name)
                    .to_str()
                    .context("output destination is not UTF-8")?
                    .to_owned(),
            ),
        );
    }
    Ok(())
}

fn validate_path(path: &str) -> Result<()> {
    if path.len() > 255
        || path.contains('\0')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        bail!("invalid export path {path:?}");
    }
    if !matches!(path.split('/').next(), Some("work" | "out")) {
        bail!("output is outside the writable domain filesystem: {path:?}");
    }
    Ok(())
}

fn private_directory(path: &PathBuf) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => bail!(
            "artifact directory is not a real directory: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    private_directory(path)
                }
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}
