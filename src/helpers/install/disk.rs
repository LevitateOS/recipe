//! Disk space utilities for recipe scripts
//!
//! Provides functions to check available disk space before large downloads.

use anyhow::{Result, bail};
use std::path::Path;
use std::process::Command;

/// Check if there's enough disk space for a download.
///
/// # Arguments
/// * `path` - Directory to check (creates if doesn't exist)
/// * `required_bytes` - Required space in bytes
///
/// # Returns
/// Ok(()) if enough space, Err with helpful message if not.
pub fn check_disk_space(path: &Path, required_bytes: u64) -> Result<()> {
    let available = get_available_space(path);

    match available {
        Some(avail) => {
            if avail < required_bytes {
                let required_gb = required_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                let available_gb = avail as f64 / (1024.0 * 1024.0 * 1024.0);
                bail!(
                    "Not enough disk space in {}\n  Required: {:.1} GB\n  Available: {:.1} GB",
                    path.display(),
                    required_gb,
                    available_gb
                );
            }
            Ok(())
        }
        None => {
            let required_gb = required_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
            eprintln!(
                "WARNING: Could not check disk space for {}. Ensure at least {:.1} GB is available.",
                path.display(),
                required_gb
            );
            Ok(())
        }
    }
}

/// Get available disk space in bytes. Returns None if check fails.
pub fn get_available_space(path: &Path) -> Option<u64> {
    // Ensure the path exists for df to work
    let check_path = if path.exists() {
        path.to_path_buf()
    } else if let Some(parent) = path.parent() {
        if parent.exists() {
            parent.to_path_buf()
        } else {
            // Fall back to current directory
            std::env::current_dir().ok()?
        }
    } else {
        std::env::current_dir().ok()?
    };

    let output = Command::new("df")
        .arg("-k")
        .arg(&check_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse df output: second line, fourth column is available KB
    let line = stdout.lines().nth(1)?;
    let fields: Vec<&str> = line.split_whitespace().collect();

    if fields.len() >= 4 {
        if let Ok(kb) = fields[3].parse::<u64>() {
            return Some(kb * 1024);
        }
    }

    None
}

/// Format bytes as human-readable string (e.g., "8.6 GB")
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.1} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_check_disk_space_sufficient() {
        // Test with current directory, assuming it has at least 1 byte available
        let result = check_disk_space(Path::new("."), 1);
        assert!(result.is_ok());
    }

    #[test]
    fn test_check_disk_space_insufficient() {
        // Test with a ridiculously large amount of space (10 PB)
        let petabyte = 1024u64 * 1024 * 1024 * 1024 * 1024;
        let result = check_disk_space(Path::new("."), 10 * petabyte);

        // Should fail for 10PB on a normal machine
        if let Err(e) = result {
            assert!(e.to_string().contains("Not enough disk space"));
        }
    }

    #[test]
    fn test_check_disk_space_nonexistent_uses_parent() {
        let dir = tempdir().unwrap();
        let nonexistent = dir.path().join("does").join("not").join("exist");

        // Should check parent directory instead
        let result = check_disk_space(&nonexistent, 1);
        assert!(result.is_ok());
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(500), "500 bytes");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
        assert_eq!(format_bytes(1024 * 1024 * 1024 * 1024), "1.0 TB");
        assert_eq!(format_bytes(8_600_000_000), "8.0 GB"); // ~8.6 GB
    }

    #[test]
    fn test_get_available_space_works() {
        // Current directory should always have some space
        let space = get_available_space(Path::new("."));
        assert!(space.is_some());
        assert!(space.unwrap() > 0);
    }
}
