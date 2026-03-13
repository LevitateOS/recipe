//! Filesystem operation helpers

use crate::core::output;
use rhai::EvalAltResult;
use std::path::{Path, PathBuf};

/// Check if a path exists
pub fn exists(path: &str) -> bool {
    // Use symlink_metadata to detect dangling symlinks too
    Path::new(path).symlink_metadata().is_ok()
}

/// Check if a file exists
pub fn file_exists(path: &str) -> bool {
    Path::new(path).is_file()
}

/// Check if a file exists (alias for file_exists)
pub fn is_file(path: &str) -> bool {
    file_exists(path)
}

/// Check if a directory exists
pub fn dir_exists(path: &str) -> bool {
    Path::new(path).is_dir()
}

/// Check if a directory exists (alias for dir_exists)
pub fn is_dir(path: &str) -> bool {
    dir_exists(path)
}

/// Create a directory and all parent directories
pub fn mkdir(path: &str) -> Result<(), Box<EvalAltResult>> {
    output::detail(&format!("mkdir {}", path));
    std::fs::create_dir_all(path).map_err(|e| format!("mkdir failed: {}", e).into())
}

/// Remove files matching a glob pattern
pub fn rm_files(pattern: &str) -> Result<(), Box<EvalAltResult>> {
    for path in glob::glob(pattern).map_err(|e| format!("invalid pattern: {}", e))? {
        let path = path.map_err(|e| format!("glob error: {}", e))?;
        output::detail(&format!("rm {}", path.display()));
        if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        }
        .map_err(|e| format!("rm failed: {}", e))?;
    }
    Ok(())
}

/// Check if a glob pattern matches at least one path
pub fn glob_exists(pattern: &str) -> Result<bool, Box<EvalAltResult>> {
    let mut entries = glob::glob(pattern).map_err(|e| format!("invalid pattern: {}", e))?;
    Ok(entries.any(|entry| entry.is_ok()))
}

fn ensure_parent_dir(path: &Path) -> Result<(), Box<EvalAltResult>> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(parent).map_err(|e| {
        format!(
            "failed to create parent directory for {}: {}",
            path.display(),
            e
        )
        .into()
    })
}

fn copy_file_impl(src: &Path, dest: &Path) -> Result<(), Box<EvalAltResult>> {
    if !src.is_file() {
        return Err(format!("copy_file source is not a file: {}", src.display()).into());
    }
    ensure_parent_dir(dest)?;
    output::detail(&format!("cp {} {}", src.display(), dest.display()));
    std::fs::copy(src, dest).map(|_| ()).map_err(|e| {
        format!(
            "copy_file failed: {} -> {}: {}",
            src.display(),
            dest.display(),
            e
        )
        .into()
    })
}

#[cfg(target_os = "linux")]
fn clone_file_reflink(src: &Path, dest: &Path) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd;

    const FICLONE: libc::c_ulong = 0x4004_9409;

    let src_file = OpenOptions::new().read(true).open(src)?;
    let dest_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(dest)?;

    let request = FICLONE as libc::Ioctl;
    let rc = unsafe { libc::ioctl(dest_file.as_raw_fd(), request, src_file.as_raw_fd()) };
    if rc == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn clone_file_reflink(_src: &Path, _dest: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "reflink not supported on this platform",
    ))
}

fn copy_path_to_dir(src: &Path, dest_dir: &Path) -> Result<(), Box<EvalAltResult>> {
    let Some(name) = src.file_name() else {
        return Err(format!("copy source has no file name: {}", src.display()).into());
    };
    let dest = dest_dir.join(name);
    copy_file_impl(src, &dest)
}

/// Copy files matching a glob pattern into a directory
pub fn copy_into_dir(pattern: &str, dest_dir: &str) -> Result<(), Box<EvalAltResult>> {
    let dest_dir = Path::new(dest_dir);
    if !dest_dir.is_dir() {
        return Err(format!(
            "copy_into_dir destination is not a directory: {}",
            dest_dir.display()
        )
        .into());
    }

    let mut matched = false;
    for path in glob::glob(pattern).map_err(|e| format!("invalid pattern: {}", e))? {
        let path = path.map_err(|e| format!("glob error: {}", e))?;
        if !path.is_file() {
            continue;
        }
        matched = true;
        copy_path_to_dir(&path, dest_dir)?;
    }

    if !matched {
        return Err(format!("copy_into_dir matched no files: {}", pattern).into());
    }

    Ok(())
}

/// Copy a file to an exact destination path
pub fn copy_file(src: &str, dest: &str) -> Result<(), Box<EvalAltResult>> {
    copy_file_impl(Path::new(src), Path::new(dest))
}

/// Copy a file using reflink semantics when available, falling back to a plain copy
pub fn copy_file_reflink(src: &str, dest: &str) -> Result<(), Box<EvalAltResult>> {
    let src = Path::new(src);
    let dest = Path::new(dest);
    if !src.is_file() {
        return Err(format!("copy_file_reflink source is not a file: {}", src.display()).into());
    }
    ensure_parent_dir(dest)?;
    output::detail(&format!(
        "cp --reflink=auto {} {}",
        src.display(),
        dest.display()
    ));
    match clone_file_reflink(src, dest) {
        Ok(()) => Ok(()),
        Err(_) => copy_file_impl(src, dest),
    }
}

#[cfg(unix)]
fn copy_symlink(link_path: &Path, dest_path: &Path) -> Result<(), Box<EvalAltResult>> {
    let target = std::fs::read_link(link_path)
        .map_err(|e| format!("read_link failed for {}: {}", link_path.display(), e))?;
    output::detail(&format!(
        "ln -s {} {}",
        target.display(),
        dest_path.display()
    ));
    match std::fs::remove_file(dest_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "failed to replace existing symlink {}: {}",
                dest_path.display(),
                e
            )
            .into());
        }
    }
    std::os::unix::fs::symlink(&target, dest_path).map_err(|e| {
        format!(
            "copy_tree_contents symlink failed: {} -> {}: {}",
            target.display(),
            dest_path.display(),
            e
        )
        .into()
    })
}

#[cfg(not(unix))]
fn copy_symlink(_link_path: &Path, _dest_path: &Path) -> Result<(), Box<EvalAltResult>> {
    Err("copy_tree_contents symlink support requires unix".into())
}

/// Recursively copy the contents of src_dir into dst_dir
pub fn copy_tree_contents(src_dir: &str, dst_dir: &str) -> Result<(), Box<EvalAltResult>> {
    use walkdir::WalkDir;

    let src_root = Path::new(src_dir);
    let dst_root = Path::new(dst_dir);

    if !src_root.is_dir() {
        return Err(format!(
            "copy_tree_contents source is not a directory: {}",
            src_root.display()
        )
        .into());
    }
    if !dst_root.is_dir() {
        return Err(format!(
            "copy_tree_contents destination is not a directory: {}",
            dst_root.display()
        )
        .into());
    }

    for entry in WalkDir::new(src_root).min_depth(1) {
        let entry = entry.map_err(|e| format!("copy_tree_contents walk failed: {}", e))?;
        let path = entry.path();
        let relative = path.strip_prefix(src_root).map_err(|e| {
            format!(
                "copy_tree_contents strip_prefix failed for {}: {}",
                path.display(),
                e
            )
        })?;
        let dest_path: PathBuf = dst_root.join(relative);
        let metadata = std::fs::symlink_metadata(path).map_err(|e| {
            format!(
                "copy_tree_contents metadata failed for {}: {}",
                path.display(),
                e
            )
        })?;

        if metadata.is_dir() {
            output::detail(&format!("mkdir {}", dest_path.display()));
            std::fs::create_dir_all(&dest_path).map_err(|e| -> Box<EvalAltResult> {
                format!(
                    "copy_tree_contents mkdir failed for {}: {}",
                    dest_path.display(),
                    e
                )
                .into()
            })?;
            continue;
        }

        ensure_parent_dir(&dest_path)?;
        if metadata.file_type().is_symlink() {
            copy_symlink(path, &dest_path)?;
            continue;
        }

        copy_file_impl(path, &dest_path)?;
        std::fs::set_permissions(&dest_path, metadata.permissions()).map_err(
            |e| -> Box<EvalAltResult> {
                format!(
                    "copy_tree_contents set_permissions failed for {}: {}",
                    dest_path.display(),
                    e
                )
                .into()
            },
        )?;
    }

    Ok(())
}

/// Copy the first existing source file to an exact destination path
pub fn copy_first_existing(sources: rhai::Array, dest: &str) -> Result<String, Box<EvalAltResult>> {
    let dest = Path::new(dest);
    let mut candidates = Vec::new();

    for item in sources {
        let source = item.to_string();
        let trimmed = source.trim();
        if trimmed.is_empty() {
            continue;
        }
        candidates.push(trimmed.to_owned());
        let src_path = Path::new(trimmed);
        if src_path.is_file() {
            copy_file_impl(src_path, dest)?;
            return Ok(trimmed.to_owned());
        }
    }

    Err(format!(
        "copy_first_existing found no existing file for destination {} in {:?}",
        dest.display(),
        candidates
    )
    .into())
}

/// Move/rename a file
pub fn move_file(src: &str, dest: &str) -> Result<(), Box<EvalAltResult>> {
    output::detail(&format!("mv {} -> {}", src, dest));
    std::fs::rename(src, dest).map_err(|e| format!("mv failed: {}", e).into())
}

/// Create a symbolic link
#[cfg(unix)]
pub fn symlink(src: &str, dest: &str) -> Result<(), Box<EvalAltResult>> {
    output::detail(&format!("ln -s {} {}", src, dest));
    std::os::unix::fs::symlink(src, dest).map_err(|e| format!("symlink failed: {}", e).into())
}

/// Create or replace a symbolic link
#[cfg(unix)]
pub fn symlink_force(src: &str, dest: &str) -> Result<(), Box<EvalAltResult>> {
    let dest_path = Path::new(dest);
    output::detail(&format!("ln -sfn {} {}", src, dest));

    match std::fs::remove_file(dest_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) if e.kind() == std::io::ErrorKind::IsADirectory => {
            return Err(format!("ln_force destination is an existing directory: {}", dest).into());
        }
        Err(e) => {
            return Err(format!("failed to remove existing link target {}: {}", dest, e).into());
        }
    }

    std::os::unix::fs::symlink(src, dest).map_err(|e| format!("symlink failed: {}", e).into())
}

#[cfg(not(unix))]
pub fn symlink(_src: &str, _dest: &str) -> Result<(), Box<EvalAltResult>> {
    Err("symlinks not supported on this platform".into())
}

#[cfg(not(unix))]
pub fn symlink_force(_src: &str, _dest: &str) -> Result<(), Box<EvalAltResult>> {
    Err("symlinks not supported on this platform".into())
}

/// Change file permissions
#[cfg(unix)]
pub fn chmod_file(path: &str, mode: i64) -> Result<(), Box<EvalAltResult>> {
    use std::os::unix::fs::PermissionsExt;
    output::detail(&format!("chmod {:o} {}", mode, path));
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode as u32))
        .map_err(|e| format!("chmod failed: {}", e).into())
}

#[cfg(not(unix))]
pub fn chmod_file(_path: &str, _mode: i64) -> Result<(), Box<EvalAltResult>> {
    // No-op on non-Unix
    Ok(())
}
