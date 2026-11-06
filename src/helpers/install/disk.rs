//! Disk space utilities for recipe scripts
//!
//! Provides functions to check available disk space before large downloads.

use anyhow::{Result, bail};
use std::path::Path;
use std::process::Command;

/// Check if there's enough disk space for a download.
///
/// # Arguments
/// * `path` - Directory to check (falls back to its parent when missing)
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
fn get_available_space(path: &Path) -> Option<u64> {
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

    if fields.len() >= 4
        && let Ok(kb) = fields[3].parse::<u64>()
    {
        return Some(kb * 1024);
    }

    None
}
