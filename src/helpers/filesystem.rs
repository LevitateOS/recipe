//! Filesystem operation helpers

use rhai::EvalAltResult;
use std::path::Path;

/// Check if a path exists
pub fn exists(path: &str) -> bool {
    Path::new(path).exists()
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
    println!("     mkdir {}", path);
    std::fs::create_dir_all(path).map_err(|e| format!("mkdir failed: {}", e).into())
}

/// Remove files matching a glob pattern
pub fn rm_files(pattern: &str) -> Result<(), Box<EvalAltResult>> {
    for path in glob::glob(pattern).map_err(|e| format!("invalid pattern: {}", e))? {
        let path = path.map_err(|e| format!("glob error: {}", e))?;
        println!("     rm {}", path.display());
        if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        }
        .map_err(|e| format!("rm failed: {}", e))?;
    }
    Ok(())
}

/// Move/rename a file
pub fn move_file(src: &str, dest: &str) -> Result<(), Box<EvalAltResult>> {
    println!("     mv {} -> {}", src, dest);
    std::fs::rename(src, dest).map_err(|e| format!("mv failed: {}", e).into())
}

/// Create a symbolic link
#[cfg(unix)]
pub fn symlink(src: &str, dest: &str) -> Result<(), Box<EvalAltResult>> {
    println!("     ln -s {} {}", src, dest);
    std::os::unix::fs::symlink(src, dest).map_err(|e| format!("symlink failed: {}", e).into())
}

#[cfg(not(unix))]
pub fn symlink(_src: &str, _dest: &str) -> Result<(), Box<EvalAltResult>> {
    Err("symlinks not supported on this platform".into())
}

/// Change file permissions
#[cfg(unix)]
pub fn chmod_file(path: &str, mode: i64) -> Result<(), Box<EvalAltResult>> {
    use std::os::unix::fs::PermissionsExt;
    println!("     chmod {:o} {}", mode, path);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode as u32))
        .map_err(|e| format!("chmod failed: {}", e).into())
}

#[cfg(not(unix))]
pub fn chmod_file(_path: &str, _mode: i64) -> Result<(), Box<EvalAltResult>> {
    // No-op on non-Unix
    Ok(())
}
