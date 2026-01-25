//! Recipe lock management
//!
//! Provides exclusive locking to prevent concurrent recipe execution.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs::File;
use std::path::{Path, PathBuf};

/// How old a lock file can be before it's considered stale (24 hours)
const STALE_LOCK_AGE_SECS: u64 = 86400;

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
///
/// If a stale lock is detected (older than 24 hours), it is automatically cleaned up.
pub fn acquire_recipe_lock(recipe_path: &Path) -> Result<RecipeLock> {
    let lock_path = recipe_path.with_extension("rhai.lock");

    // Check for stale lock and clean up if found
    if lock_path.exists() && is_stale_lock(&lock_path) {
        let _ = std::fs::remove_file(&lock_path);
    }

    let lock_file = File::create(&lock_path)
        .with_context(|| format!("Failed to create lock file: {}", lock_path.display()))?;

    if lock_file.try_lock_exclusive().is_err() {
        // Clean up the lock file we created before returning error
        // (the file exists but we couldn't acquire the lock)
        drop(lock_file); // Close the file handle first
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
        // Lock is automatically released when file is dropped
        // Clean up lock file
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviso_cheat_test::cheat_reviewed;
    use tempfile::TempDir;

    #[cheat_reviewed("Lock test - successfully acquires lock")]
    #[test]
    fn test_lock_acquired_successfully() {
        let dir = TempDir::new().unwrap();
        let recipe_path = dir.path().join("test.rhai");
        std::fs::write(&recipe_path, "").unwrap();

        let lock = acquire_recipe_lock(&recipe_path);
        assert!(lock.is_ok());

        // Lock file should exist while lock is held
        let lock_path = recipe_path.with_extension("rhai.lock");
        assert!(lock_path.exists());
    }

    #[cheat_reviewed("Lock test - lock released on drop")]
    #[test]
    fn test_lock_released_on_drop() {
        let dir = TempDir::new().unwrap();
        let recipe_path = dir.path().join("test.rhai");
        std::fs::write(&recipe_path, "").unwrap();

        {
            let _lock = acquire_recipe_lock(&recipe_path).unwrap();
            // Lock should exist
            assert!(recipe_path.with_extension("rhai.lock").exists());
        }

        // Lock file should be cleaned up after drop
        assert!(!recipe_path.with_extension("rhai.lock").exists());
    }

    #[cheat_reviewed("Lock test - stale lock is cleaned up")]
    #[test]
    fn test_stale_lock_cleaned_up() {
        let dir = TempDir::new().unwrap();
        let recipe_path = dir.path().join("test.rhai");
        std::fs::write(&recipe_path, "").unwrap();

        let lock_path = recipe_path.with_extension("rhai.lock");

        // Create a stale lock file with old mtime
        std::fs::write(&lock_path, "stale").unwrap();

        // Set mtime to 25 hours ago (beyond stale threshold)
        let old_time = std::time::SystemTime::now()
            - std::time::Duration::from_secs(STALE_LOCK_AGE_SECS + 3600);
        filetime::set_file_mtime(&lock_path, filetime::FileTime::from_system_time(old_time))
            .unwrap();

        // Should be able to acquire lock (stale lock cleaned up)
        let lock = acquire_recipe_lock(&recipe_path);
        assert!(lock.is_ok());
    }

    #[cheat_reviewed("Lock test - concurrent lock is blocked")]
    #[test]
    fn test_concurrent_lock_blocked() {
        let dir = TempDir::new().unwrap();
        let recipe_path = dir.path().join("test.rhai");
        std::fs::write(&recipe_path, "").unwrap();

        // Acquire first lock
        let _lock1 = acquire_recipe_lock(&recipe_path).unwrap();

        // Second attempt should fail
        let lock2 = acquire_recipe_lock(&recipe_path);
        assert!(lock2.is_err());
        assert!(lock2.unwrap_err().to_string().contains("already being executed"));
    }
}
