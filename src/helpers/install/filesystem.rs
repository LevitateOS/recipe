//! Filesystem operation helpers

use crate::core::output;
use rhai::EvalAltResult;
use std::path::Path;

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

/// Copy files matching a glob pattern into a directory
pub fn copy_into_dir(pattern: &str, dest_dir: &str) -> Result<(), Box<EvalAltResult>> {
    let dest_dir = Path::new(dest_dir);
    if !dest_dir.is_dir() {
        return Err(
            format!(
                "copy_into_dir destination is not a directory: {}",
                dest_dir.display()
            )
            .into(),
        );
    }

    let mut matched = false;
    for path in glob::glob(pattern).map_err(|e| format!("invalid pattern: {}", e))? {
        let path = path.map_err(|e| format!("glob error: {}", e))?;
        if !path.is_file() {
            continue;
        }
        matched = true;
        let Some(name) = path.file_name() else {
            return Err(
                format!("copy_into_dir source has no file name: {}", path.display()).into(),
            );
        };
        let dest = dest_dir.join(name);
        output::detail(&format!("cp {} {}", path.display(), dest.display()));
        std::fs::copy(&path, &dest).map_err(|e| -> Box<EvalAltResult> {
            format!(
                "copy_into_dir failed: {} -> {}: {}",
                path.display(),
                dest.display(),
                e
            )
            .into()
        })?;
    }

    if !matched {
        return Err(format!("copy_into_dir matched no files: {}", pattern).into());
    }

    Ok(())
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
