//! Common filesystem utilities
//!
//! Provides shared filesystem operations used across multiple helpers.

use rhai::EvalAltResult;
use std::path::{Path, PathBuf};

/// Ensure a file's parent directory exists.
///
/// Creates the parent directory (and all ancestors) if it doesn't exist.
///
/// # Example
/// ```ignore
/// ensure_parent_dir(Path::new("/foo/bar/baz.txt"))?;
/// // /foo/bar/ now exists
/// ```
pub fn ensure_parent_dir(path: &Path) -> Result<(), Box<EvalAltResult>> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create directory {}: {}", parent.display(), e))?;
        }
    }
    Ok(())
}

/// Expand a glob pattern and return matching paths.
///
/// Returns an empty Vec if no matches found (doesn't error).
///
/// # Example
/// ```ignore
/// let files = glob_paths("*.rs")?;
/// ```
pub fn glob_paths(pattern: &str) -> Result<Vec<PathBuf>, Box<EvalAltResult>> {
    glob::glob(pattern)
        .map_err(|e| format!("invalid glob pattern '{}': {}", pattern, e))?
        .filter_map(|r| r.ok())
        .collect::<Vec<_>>()
        .pipe(Ok)
}

/// Expand a glob pattern, returning error if no matches.
///
/// # Example
/// ```ignore
/// let files = glob_paths_required("src/*.rs")?;
/// ```
pub fn glob_paths_required(pattern: &str) -> Result<Vec<PathBuf>, Box<EvalAltResult>> {
    let matches = glob_paths(pattern)?;
    if matches.is_empty() {
        return Err(format!("no files match pattern: {}", pattern).into());
    }
    Ok(matches)
}

/// Set file permissions (Unix only).
///
/// No-op on non-Unix platforms.
#[cfg(unix)]
pub fn set_mode(path: &Path, mode: u32) -> Result<(), Box<EvalAltResult>> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| format!("chmod failed for {}: {}", path.display(), e).into())
}

#[cfg(not(unix))]
pub fn set_mode(_path: &Path, _mode: u32) -> Result<(), Box<EvalAltResult>> {
    Ok(()) // No-op on non-Unix
}

/// Copy a file, creating parent directories as needed.
pub fn copy_file(src: &Path, dest: &Path) -> Result<u64, Box<EvalAltResult>> {
    ensure_parent_dir(dest)?;
    std::fs::copy(src, dest).map_err(|e| {
        format!(
            "copy failed: {} -> {}: {}",
            src.display(),
            dest.display(),
            e
        )
        .into()
    })
}

/// Move/rename a file, creating parent directories as needed.
pub fn move_file(src: &Path, dest: &Path) -> Result<(), Box<EvalAltResult>> {
    ensure_parent_dir(dest)?;
    std::fs::rename(src, dest).map_err(|e| {
        format!(
            "move failed: {} -> {}: {}",
            src.display(),
            dest.display(),
            e
        )
        .into()
    })
}

/// Check if path is safe (no path traversal).
///
/// Rejects absolute paths and paths containing "..".
pub fn is_safe_path(path: &Path) -> bool {
    !path.is_absolute()
        && !path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
}

/// Validate a path is safe, returning error if not.
pub fn validate_safe_path(path: &Path) -> Result<(), Box<EvalAltResult>> {
    if !is_safe_path(path) {
        return Err(format!(
            "unsafe path (contains .. or is absolute): {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

// Helper trait for pipe syntax
trait Pipe: Sized {
    fn pipe<F, R>(self, f: F) -> R
    where
        F: FnOnce(Self) -> R,
    {
        f(self)
    }
}

impl<T> Pipe for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_ensure_parent_dir() {
        let temp = tempdir().unwrap();
        let nested = temp.path().join("a/b/c/file.txt");

        ensure_parent_dir(&nested).unwrap();
        assert!(temp.path().join("a/b/c").exists());
    }

    #[test]
    fn test_ensure_parent_dir_already_exists() {
        let temp = tempdir().unwrap();
        let file = temp.path().join("file.txt");

        // Should not error if parent already exists
        ensure_parent_dir(&file).unwrap();
    }

    #[test]
    fn test_glob_paths() {
        let temp = tempdir().unwrap();
        std::fs::write(temp.path().join("a.txt"), "").unwrap();
        std::fs::write(temp.path().join("b.txt"), "").unwrap();

        let pattern = format!("{}/*.txt", temp.path().display());
        let matches = glob_paths(&pattern).unwrap();
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn test_glob_paths_no_matches() {
        let matches = glob_paths("/nonexistent/*.xyz").unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn test_glob_paths_required_fails_on_empty() {
        let result = glob_paths_required("/nonexistent/*.xyz");
        assert!(result.is_err());
    }

    #[test]
    fn test_is_safe_path() {
        assert!(is_safe_path(Path::new("foo/bar/baz")));
        assert!(is_safe_path(Path::new("file.txt")));
        assert!(!is_safe_path(Path::new("/absolute/path")));
        assert!(!is_safe_path(Path::new("../escape")));
        assert!(!is_safe_path(Path::new("foo/../bar")));
    }

    #[test]
    fn test_copy_file_creates_parents() {
        let temp = tempdir().unwrap();
        let src = temp.path().join("src.txt");
        let dest = temp.path().join("a/b/c/dest.txt");

        std::fs::write(&src, "content").unwrap();
        copy_file(&src, &dest).unwrap();

        assert!(dest.exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "content");
    }

    #[cfg(unix)]
    #[test]
    fn test_set_mode() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().unwrap();
        let file = temp.path().join("test.sh");
        std::fs::write(&file, "#!/bin/sh").unwrap();

        set_mode(&file, 0o755).unwrap();

        let perms = std::fs::metadata(&file).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o755);
    }
}
