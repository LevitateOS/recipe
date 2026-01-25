//! Directory cleanup utilities
//!
//! Functions for cleaning up empty directories after package removal.

use std::collections::HashSet;
use std::path::Path;

/// Clean up empty directories after file removal
///
/// After removing installed files, this function walks up the directory tree
/// and removes any directories that are now empty. It stops at the prefix
/// directory to avoid accidentally removing system directories.
pub fn cleanup_empty_dirs(files: &[String], prefix: &Path) {
    // Collect all parent directories
    let mut dirs: HashSet<std::path::PathBuf> = HashSet::new();
    for file in files {
        let mut path = std::path::Path::new(file).to_path_buf();
        while let Some(parent) = path.parent() {
            if !parent.starts_with(prefix) || parent == prefix {
                break;
            }
            dirs.insert(parent.to_path_buf());
            path = parent.to_path_buf();
        }
    }

    // Sort by depth (deepest first) and try to remove empty ones
    let mut dirs: Vec<_> = dirs.into_iter().collect();
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));

    for dir in dirs {
        if dir.exists()
            && let Ok(entries) = std::fs::read_dir(&dir)
            && entries.count() == 0
        {
            let _ = std::fs::remove_dir(&dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviso_cheat_test::cheat_reviewed;
    use tempfile::TempDir;

    fn create_test_prefix() -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let prefix = dir.path().join("prefix");
        std::fs::create_dir_all(&prefix).unwrap();
        (dir, prefix)
    }

    #[cheat_reviewed("Cleanup test - empty directories removed")]
    #[test]
    fn test_cleanup_empty_dirs_removes_empty() {
        let (_dir, prefix) = create_test_prefix();

        // Create nested empty directories
        let nested = prefix.join("a/b/c");
        std::fs::create_dir_all(&nested).unwrap();

        let files = vec![nested.join("file.txt").to_string_lossy().to_string()];

        cleanup_empty_dirs(&files, &prefix);

        // All empty directories should be removed
        assert!(!prefix.join("a/b/c").exists());
        assert!(!prefix.join("a/b").exists());
        assert!(!prefix.join("a").exists());
    }

    #[cheat_reviewed("Cleanup test - non-empty directories preserved")]
    #[test]
    fn test_cleanup_empty_dirs_preserves_nonempty() {
        let (_dir, prefix) = create_test_prefix();

        // Create directories with one containing a file
        let a = prefix.join("a");
        let b = a.join("b");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("keep.txt"), "content").unwrap();

        let files = vec![b.join("deleted.txt").to_string_lossy().to_string()];

        cleanup_empty_dirs(&files, &prefix);

        // "a" should still exist (has keep.txt), "b" should be removed
        assert!(a.exists());
        assert!(!b.exists());
    }

    #[cheat_reviewed("Cleanup test - stops at prefix directory, doesn't delete it")]
    #[test]
    fn test_cleanup_empty_dirs_stops_at_prefix() {
        let (_dir, prefix) = create_test_prefix();

        let nested = prefix.join("a");
        std::fs::create_dir_all(&nested).unwrap();

        let files = vec![nested.join("file.txt").to_string_lossy().to_string()];

        cleanup_empty_dirs(&files, &prefix);

        // "a" removed but prefix itself should remain
        assert!(!nested.exists());
        assert!(prefix.exists());
    }
}
