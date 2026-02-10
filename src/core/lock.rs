//! Recipe lock management
//!
//! Provides exclusive locking to prevent concurrent recipe execution.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::File;
use std::path::{Path, PathBuf};

/// How old a lock file can be before it's considered stale (2 hours)
const STALE_LOCK_AGE_SECS: u64 = 7200;

/// Check if a lock file is stale (older than STALE_LOCK_AGE_SECS)
fn is_stale_lock(lock_path: &Path) -> bool {
    if let Ok(metadata) = std::fs::metadata(lock_path)
        && let Ok(modified) = metadata.modified()
        && let Ok(age) = std::time::SystemTime::now().duration_since(modified)
    {
        return age.as_secs() > STALE_LOCK_AGE_SECS;
    }
    false
}

/// Acquire an exclusive lock on a recipe file to prevent concurrent execution.
/// Returns a guard that releases the lock when dropped.
pub fn acquire_recipe_lock(recipe_path: &Path) -> Result<RecipeLock> {
    let lock_path = recipe_path.with_extension("rhai.lock");

    // Check for stale lock and clean up if found
    if lock_path.exists() && is_stale_lock(&lock_path) {
        let _ = std::fs::remove_file(&lock_path);
    }

    let lock_file = File::create(&lock_path)
        .with_context(|| format!("Failed to create lock file: {}", lock_path.display()))?;

    if lock_file.try_lock_exclusive().is_err() {
        drop(lock_file);
        let _ = std::fs::remove_file(&lock_path);
        return Err(anyhow::anyhow!(
            "Recipe '{}' is already being executed by another process. \
             If this is incorrect, delete '{}'",
            recipe_path.display(),
            lock_path.display()
        ));
    }

    Ok(RecipeLock {
        _file: lock_file,
        path: lock_path,
    })
}

/// RAII guard for recipe lock - releases lock and deletes lock file when dropped
#[derive(Debug)]
pub struct RecipeLock {
    #[allow(dead_code)]
    _file: File,
    path: PathBuf,
}

impl Drop for RecipeLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
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

        assert!(!recipe_path.with_extension("rhai.lock").exists());
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
