//! Path manipulation helpers

use std::path::Path;

/// Join two path components
pub fn join_path(a: &str, b: &str) -> String {
    Path::new(a).join(b).to_string_lossy().to_string()
}

/// Get the basename (filename) of a path
pub fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Get the directory name (parent) of a path
pub fn dirname(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}
