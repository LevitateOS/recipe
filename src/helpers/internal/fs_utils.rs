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
