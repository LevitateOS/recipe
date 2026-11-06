//! Shared filesystem helpers for recipe-facing helpers.

use rhai::EvalAltResult;
use std::path::Path;

/// Ensure a file's parent directory exists.
pub fn ensure_parent_dir(path: &Path) -> Result<(), Box<EvalAltResult>> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create directory {}: {}", parent.display(), e))?;
    }
    Ok(())
}

/// Remove `path` if it exists (a missing path is not an error). Callers use
/// this before creating a replacement so they never write through a link or
/// into an inode another name shares.
pub fn remove_existing(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Point `dest` at `target` atomically: the link is created under a temporary
/// sibling name and renamed over `dest`, so a failure leaves `dest` unchanged.
pub fn replace_symlink(target: &Path, dest: &Path) -> std::io::Result<()> {
    let name = dest
        .file_name()
        .ok_or_else(|| std::io::Error::other("link destination has no file name"))?;
    let mut staged = std::ffi::OsString::from(".recipe-link.");
    staged.push(std::process::id().to_string());
    staged.push(".");
    staged.push(name);
    let staged = dest.with_file_name(staged);
    let _ = std::fs::remove_file(&staged);
    std::os::unix::fs::symlink(target, &staged)?;
    std::fs::rename(&staged, dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })
}
