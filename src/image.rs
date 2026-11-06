//! The root image contract shared by the image builder (`recipe-image`) and
//! the coordinator that unpacks it into a domain root.
//!
//! The producer runs every member it writes through the same [`Validator`]
//! the consumer uses, so it cannot publish an image the consumer rejects.

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// Largest image archive.
pub const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Largest sum of regular-file sizes in an image.
pub const MAX_UNPACKED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// Most members in an image.
pub const MAX_ENTRIES: usize = 500_000;
/// Longest member path or link target.
pub const MAX_PATH_BYTES: usize = 4096;

/// Permission bits an unpacked member keeps: the domain root is read-only.
pub fn installed_mode(mode: u32) -> u32 {
    mode & 0o555
}

/// Normalize a header for a reproducible image: root ownership, epoch mtime.
pub fn normalize_header(header: &mut tar::Header) {
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();
}

/// Normalized relative member path: `.` components dropped, anything that is
/// not a plain name rejected. The empty path is the image root.
pub fn member_path(path: &Path) -> Result<PathBuf> {
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => relative.push(name),
            _ => bail!("unsafe rootfs image path: {}", path.display()),
        }
    }
    if relative.as_os_str().len() > MAX_PATH_BYTES {
        bail!("rootfs image path exceeds {MAX_PATH_BYTES} bytes");
    }
    Ok(relative)
}

/// Checks members in archive order against the image rules.
#[derive(Default)]
pub struct Validator {
    /// Members seen so far → regular-file installed mode (None for non-files).
    members: HashMap<PathBuf, Option<u32>>,
    unpacked: u64,
}

impl Validator {
    /// Validate the next member. `path` must come from [`member_path`] and be
    /// non-empty; `link` is the target of a symlink or hardlink.
    pub fn check(
        &mut self,
        path: &Path,
        kind: tar::EntryType,
        size: u64,
        mode: u32,
        link: Option<&Path>,
    ) -> Result<()> {
        if self.members.len() >= MAX_ENTRIES {
            bail!("rootfs image entry limit exceeded");
        }
        if crate::domain::RESERVED_ROOTS
            .iter()
            .any(|name| path.starts_with(name))
        {
            bail!("rootfs image uses reserved invocation path: {}", path.display());
        }
        if self.members.contains_key(path) {
            bail!("duplicate rootfs image path: {}", path.display());
        }
        let file_mode = if kind.is_file() {
            self.unpacked = self
                .unpacked
                .checked_add(size)
                .filter(|total| *total <= MAX_UNPACKED_BYTES)
                .context("rootfs unpacked size limit exceeded")?;
            Some(installed_mode(mode))
        } else if kind.is_dir() {
            None
        } else if kind.is_symlink() {
            let target = link.context("rootfs symlink has no target")?;
            if target.as_os_str().is_empty() || target.as_os_str().len() > MAX_PATH_BYTES {
                bail!("invalid rootfs symlink target: {}", path.display());
            }
            // Absolute targets are meaningful inside the chroot; relative ones
            // must not climb above the image root.
            if !target.is_absolute() {
                let mut depth = path.components().count() - 1;
                for part in target.components() {
                    match part {
                        Component::Normal(_) => depth += 1,
                        Component::ParentDir if depth > 0 => depth -= 1,
                        Component::ParentDir => {
                            bail!("rootfs symlink escapes the image root: {}", path.display())
                        }
                        _ => {}
                    }
                }
            }
            None
        } else if kind.is_hard_link() {
            let target = member_path(link.context("rootfs hardlink has no target")?)?;
            match self.members.get(&target) {
                Some(Some(target_mode)) if *target_mode == installed_mode(mode) => {}
                Some(Some(_)) => bail!("rootfs hardlink target kind or mode disagrees: {}", path.display()),
                _ => bail!("rootfs hardlink target must be a prior file: {}", path.display()),
            }
            None
        } else {
            bail!("unsupported rootfs image entry type at {}", path.display());
        };
        self.members.insert(path.to_owned(), file_mode);
        Ok(())
    }
}
