//! Validate a completed domain archive and publish only its named outputs.
use std::collections::{BTreeMap, HashSet};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value;

struct Staging(tempfile::TempDir);
impl Drop for Staging {
    fn drop(&mut self) {
        crate::fsx::make_removable(self.0.path());
    }
}

pub(crate) fn publish(
    invocation: &str,
    archive: &Path,
    context: &mut Value,
    options: &crate::ExecutionOptions,
) -> Result<()> {
    let max_bytes = options.storage_byte_bound();
    let max_entries = options.storage_max_objects as usize;
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
        if !crate::domain::valid_name(name) {
            bail!("invalid output name {name:?}");
        }
        let path = value.as_str().context("output paths must be strings")?;
        let relative = path
            .strip_prefix('/')
            .context("output path must be absolute")?;
        validate_path(relative)?;
        selected.insert(name.clone(), relative.to_owned());
    }
    let file = crate::fsx::open_regular(archive).context("cannot open completed Ginux export")?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > options.archive_bound() {
        bail!("invalid or oversized Ginux export");
    }
    let base = std::env::current_dir()?.join(".artifacts");
    crate::fsx::private_dir(&base, false)?;
    let base = base.join("ginux-export");
    crate::fsx::private_dir(&base, false)?;
    let base = base.canonicalize()?;
    let staging = Staging(
        tempfile::Builder::new()
            .prefix(".pending-")
            .tempdir_in(&base)?,
    );
    let mut seen = BTreeMap::<String, tar::EntryType>::new();
    let mut found = HashSet::new();
    let mut directories = BTreeMap::new();
    let mut bytes = 0u64;
    let mut archive = tar::Archive::new(file);
    let mut long_name = None;
    let mut long_link = None;
    let mut links = BTreeMap::new();
    // Payloads are private snapshot inodes. Keeping all prior regular payloads
    // permits a selected alias to reference an unselected earlier name without
    // copying data or following arbitrary archive paths.
    let payloads = staging.0.path().join(".payloads");
    fs::create_dir(&payloads)?;
    let mut regulars = BTreeMap::<String, (PathBuf, u32, u64)>::new();
    for (raw_count, entry) in archive.entries()?.raw(true).enumerate() {
        if raw_count >= max_entries * 3 {
            bail!("excessive export headers");
        }
        let mut entry = entry?;
        if entry.header().entry_type().is_gnu_longname()
            || entry.header().entry_type().is_gnu_longlink()
        {
            let pending = if entry.header().entry_type().is_gnu_longname() {
                &mut long_name
            } else {
                &mut long_link
            };
            if pending.is_some() || !(2..=256).contains(&entry.size()) {
                bail!("invalid long export name");
            }
            let mut name = vec![0; entry.size() as usize];
            entry.read_exact(&mut name)?;
            if name.pop() != Some(0) || name.contains(&0) {
                bail!("unterminated long export name");
            }
            *pending = Some(name);
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
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() && !kind.is_symlink() && !kind.is_hard_link() {
            bail!("unsupported export entry kind at {path}");
        }
        if seen.len() >= max_entries || seen.contains_key(path) {
            bail!("duplicate or excessive export entries");
        }
        // Reject both orders of a non-directory parent/child conflict, including
        // unselected entries. Symlinks are never traversed during publication.
        let mut parent = Path::new(path).parent();
        while let Some(path) = parent {
            if let Some(parent_kind) = seen.get(path.to_str().context("invalid export parent")?) {
                if *parent_kind != tar::EntryType::Directory {
                    bail!("export traverses a non-directory parent");
                }
            }
            parent = path.parent();
        }
        if !kind.is_dir()
            && seen
                .range(format!("{path}/")..)
                .next()
                .is_some_and(|(name, _)| name.starts_with(&format!("{path}/")))
        {
            bail!("export replaces a parent directory");
        }
        seen.insert(path.to_owned(), kind);
        if long_link.is_some() && !kind.is_symlink() && !kind.is_hard_link() {
            bail!("link target on non-link entry");
        }
        let target = if kind.is_symlink() || kind.is_hard_link() {
            let bytes = long_link
                .take()
                .or_else(|| entry.link_name_bytes().map(|name| name.into_owned()))
                .context("export link has no target")?;
            if bytes.is_empty() || bytes.len() > 255 || bytes.contains(&0) {
                bail!("invalid export link target");
            }
            Some(PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)))
        } else {
            None
        };
        let size = entry.size();
        bytes = bytes.checked_add(size).context("export size overflow")?;
        if bytes > max_bytes || (!kind.is_file() && size != 0) {
            bail!("invalid export size");
        }
        let mode = entry.header().mode()? & 0o777;
        let mtime = entry.header().mtime()?;
        let modified = std::time::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(mtime))
            .context("invalid export timestamp")?;
        let payload = if kind.is_file() {
            let payload = payloads.join(regulars.len().to_string());
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&payload)?;
            if std::io::copy(&mut entry, &mut file)? != size {
                bail!("truncated export entry");
            }
            file.set_modified(modified)?;
            file.set_permissions(fs::Permissions::from_mode(mode))?;
            file.sync_all()?;
            regulars.insert(path.to_owned(), (payload.clone(), mode, mtime));
            Some(payload)
        } else if kind.is_hard_link() {
            let target = target
                .as_ref()
                .unwrap()
                .to_str()
                .context("hardlink target must be UTF-8")?;
            validate_path(target)?;
            let (payload, prior_mode, prior_mtime) = regulars
                .get(target)
                .context("hardlink target must be a prior regular file")?
                .clone();
            if mode != prior_mode || mtime != prior_mtime {
                bail!("hardlink metadata disagrees");
            }
            regulars.insert(path.to_owned(), (payload.clone(), mode, mtime));
            Some(payload)
        } else {
            None
        };
        for (name, root) in &selected {
            let suffix = if path == root {
                if kind.is_symlink() {
                    bail!("a named output must be a regular file or directory");
                }
                found.insert(name.clone());
                Some("")
            } else {
                path.strip_prefix(root)
                    .and_then(|rest| rest.strip_prefix('/'))
            };
            let Some(suffix) = suffix else {
                continue;
            };
            let destination = staging.0.path().join(name);
            let destination = if suffix.is_empty() {
                destination
            } else {
                destination.join(suffix)
            };
            if kind.is_symlink() {
                links.insert(destination, (target.as_ref().unwrap().clone(), mtime));
            } else if kind.is_dir() {
                fs::create_dir_all(&destination)?;
                directories.insert(destination, (mode, modified));
            } else {
                fs::create_dir_all(destination.parent().context("missing output parent")?)?;
                fs::hard_link(payload.as_ref().unwrap(), destination)?;
            }
        }
    }
    fs::remove_dir_all(&payloads)?;
    if long_name.is_some() || long_link.is_some() {
        bail!("long export name without an entry");
    }
    if found.len() != selected.len() {
        bail!("a declared output is absent from the completed domain export");
    }
    // Materialize links only after regular bytes. Preflight every parent before
    // creating any link, so even conflicting archive entries cannot redirect an
    // extraction, cleanup, or permission operation outside private staging.
    for destination in links.keys() {
        fs::create_dir_all(destination.parent().context("missing link parent")?)?;
        if fs::symlink_metadata(destination).is_ok() {
            bail!("export symlink conflicts with a file or parent directory");
        }
    }
    for (destination, (target, mtime)) in &links {
        std::os::unix::fs::symlink(target, destination)?;
        let path = CString::new(destination.as_os_str().as_bytes())?;
        let times = [
            libc::timespec {
                tv_sec: 0,
                tv_nsec: libc::UTIME_OMIT,
            },
            libc::timespec {
                tv_sec: (*mtime).try_into().context("invalid symlink timestamp")?,
                tv_nsec: 0,
            },
        ];
        if unsafe {
            libc::utimensat(
                libc::AT_FDCWD,
                path.as_ptr(),
                times.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error())
                .context("cannot preserve symlink timestamp");
        }
    }
    // Rootfs products may contain absolute links meaningful inside their future
    // root. Preserve their bytes; never follow them while publishing.
    for entry in walkdir::WalkDir::new(staging.0.path()).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_dir() {
            File::open(entry.path())?.sync_all()?;
        }
    }
    // Apply directory modes after all children and links have been created.
    for (directory, (mode, modified)) in directories.iter().rev() {
        File::open(directory)?.set_modified(*modified)?;
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
    if path.len() > crate::domain::MAX_PATH
        || path.contains('\0')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        bail!("invalid export path {path:?}");
    }
    if !path
        .split('/')
        .next()
        .is_some_and(|root| crate::domain::WRITABLE_ROOTS.contains(&root))
    {
        bail!("output is outside the writable domain filesystem: {path:?}");
    }
    Ok(())
}

