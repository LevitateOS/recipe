//! Disk space utilities for recipe scripts
//!
//! Provides functions to check available disk space before large downloads.

use anyhow::{Result, bail};
use std::path::Path;

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
            crate::core::output::warning(&format!(
                "WARNING: Could not check disk space for {}. Ensure at least {:.1} GB is available.",
                path.display(),
                required_gb
            ));
            Ok(())
        }
    }
}

/// Get available disk space in bytes. Returns None if check fails.
fn get_available_space(path: &Path) -> Option<u64> {
    // statvfs needs an existing path.
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

    let path = std::ffi::CString::new(check_path.as_os_str().as_encoded_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // statvfs only writes the provided struct.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    let stats = unsafe { stats.assume_init() };
    Some(stats.f_bavail.saturating_mul(stats.f_frsize))
}
