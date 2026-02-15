//! Recipe lock management
//!
//! Provides exclusive locking to prevent concurrent recipe execution.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::File;
use std::fs::OpenOptions;
use std::path::Path;

/// Acquire an exclusive lock on a recipe file to prevent concurrent execution.
/// Returns a guard that releases the lock when dropped.
pub fn acquire_recipe_lock(recipe_path: &Path) -> Result<RecipeLock> {
    let lock_path = recipe_path.with_extension("rhai.lock");

    // IMPORTANT:
    // - Do not delete the lock file on contention. Another process may legitimately hold the lock.
    // - Use an advisory exclusive lock; stale lock files are harmless because locks are released on exit.
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("Failed to open lock file: {}", lock_path.display()))?;

    if let Err(e) = lock_file.try_lock_exclusive() {
        // Keep the lock file intact to preserve exclusivity for the current holder.
        return Err(anyhow::anyhow!(
            "Recipe '{}' is already being executed by another process (lock: '{}'): {e}",
            recipe_path.display(),
            lock_path.display()
        ));
    }

    Ok(RecipeLock { _file: lock_file })
}

/// RAII guard for recipe lock - releases lock when dropped
#[derive(Debug)]
pub struct RecipeLock {
    #[allow(dead_code)]
    _file: File,
}

impl Drop for RecipeLock {
    fn drop(&mut self) {
        // File drop releases the advisory lock; we intentionally keep the lock file in place.
        // Deleting a lock file while other processes have it open can enable a "new file, new lock"
        // race in future blocking-lock implementations.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_lock_acquired_successfully() {
        let dir = TempDir::new().unwrap();
        let recipe_path = dir.path().join("test.rhai");
        std::fs::write(&recipe_path, "").unwrap();

        let lock = acquire_recipe_lock(&recipe_path);
        assert!(lock.is_ok());

        let lock_path = recipe_path.with_extension("rhai.lock");
        assert!(lock_path.exists());
    }

    #[test]
    fn test_lock_released_on_drop() {
        let dir = TempDir::new().unwrap();
        let recipe_path = dir.path().join("test.rhai");
        std::fs::write(&recipe_path, "").unwrap();

        {
            let _lock = acquire_recipe_lock(&recipe_path).unwrap();
            assert!(recipe_path.with_extension("rhai.lock").exists());
        }

        // Lock should be released (file may remain on disk).
        assert!(acquire_recipe_lock(&recipe_path).is_ok());
    }

    #[test]
    fn test_concurrent_lock_blocked() {
        let dir = TempDir::new().unwrap();
        let recipe_path = dir.path().join("test.rhai");
        std::fs::write(&recipe_path, "").unwrap();

        let _lock1 = acquire_recipe_lock(&recipe_path).unwrap();
        let lock2 = acquire_recipe_lock(&recipe_path);
        assert!(lock2.is_err());
        assert!(
            lock2
                .unwrap_err()
                .to_string()
                .contains("already being executed")
        );
    }
}
