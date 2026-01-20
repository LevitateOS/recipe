//! Lock file support for reproducible builds
//!
//! A lock file records the exact versions of packages resolved during installation,
//! allowing subsequent installs to reproduce the same dependency tree.
//!
//! ## Format
//!
//! ```toml
//! # recipe.lock - Auto-generated, do not edit manually
//!
//! [packages]
//! openssl = "3.2.1"
//! zlib = "1.3.1"
//! curl = "8.5.0"
//!
//! [metadata]
//! generated = "2024-01-15T10:30:00Z"
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Lock file structure
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LockFile {
    /// Resolved package versions
    pub packages: BTreeMap<String, String>,
    /// Metadata about lock file generation
    #[serde(default)]
    pub metadata: LockMetadata,
}

/// Lock file metadata
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LockMetadata {
    /// Timestamp when lock file was generated
    #[serde(default)]
    pub generated: Option<String>,
}

impl LockFile {
    /// Create a new empty lock file
    pub fn new() -> Self {
        Self::default()
    }

    /// Read lock file from path
    pub fn read(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read lock file: {}", path.display()))?;
        toml::from_str(&content)
            .with_context(|| format!("Failed to parse lock file: {}", path.display()))
    }

    /// Write lock file to path
    pub fn write(&self, path: &Path) -> Result<()> {
        let content = toml::to_string_pretty(self)
            .context("Failed to serialize lock file")?;
        let header = "# recipe.lock - Auto-generated, do not edit manually\n\n";
        std::fs::write(path, format!("{}{}", header, content))
            .with_context(|| format!("Failed to write lock file: {}", path.display()))
    }

    /// Add a package version to the lock file
    pub fn add_package(&mut self, name: String, version: String) {
        self.packages.insert(name, version);
    }

    /// Get the locked version for a package
    pub fn get_version(&self, name: &str) -> Option<&String> {
        self.packages.get(name)
    }

    /// Check if lock file contains a package
    pub fn contains(&self, name: &str) -> bool {
        self.packages.contains_key(name)
    }

    /// Validate that resolved versions match locked versions
    ///
    /// Returns a list of mismatches: (package, locked_version, resolved_version)
    /// - For version mismatches: both locked and resolved are the actual versions
    /// - For packages missing from lock: locked_version is "(not in lock)"
    /// - For packages missing from resolved: resolved_version is "(missing)"
    pub fn validate_against(
        &self,
        resolved: &[(String, String)],
    ) -> Vec<(String, String, String)> {
        let mut mismatches = Vec::new();
        let resolved_map: std::collections::HashMap<&String, &String> =
            resolved.iter().map(|(k, v)| (k, v)).collect();

        // Check resolved packages against lock file
        for (name, version) in resolved {
            if let Some(locked_version) = self.packages.get(name) {
                if locked_version != version {
                    mismatches.push((
                        name.clone(),
                        locked_version.clone(),
                        version.clone(),
                    ));
                }
            }
            // Note: packages in resolved but not in lock are OK (new packages)
        }

        // Check for packages in lock file that are missing from resolved
        // This catches the case where a locked dependency was removed
        for (locked_name, locked_version) in &self.packages {
            if !resolved_map.contains_key(locked_name) {
                mismatches.push((
                    locked_name.clone(),
                    locked_version.clone(),
                    "(missing)".to_string(),
                ));
            }
        }

        mismatches
    }

    /// Update metadata with current timestamp
    pub fn update_metadata(&mut self) {
        use std::time::SystemTime;

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Format as ISO 8601
        let datetime = chrono_lite_format(now);
        self.metadata.generated = Some(datetime);
    }
}

/// Simple ISO 8601 date formatting without chrono dependency
fn chrono_lite_format(unix_secs: u64) -> String {
    // Simple approximation - for production, use chrono or time crate
    let days_since_1970 = unix_secs / 86400;
    let secs_today = unix_secs % 86400;

    let hours = secs_today / 3600;
    let minutes = (secs_today % 3600) / 60;
    let seconds = secs_today % 60;

    // Approximate year/month/day calculation
    let mut year = 1970;
    let mut remaining_days = days_since_1970 as i64;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining_days < days_in_year {
            break;
        }
        remaining_days -= days_in_year;
        year += 1;
    }

    let is_leap = is_leap_year(year);
    let days_in_months: [i64; 12] = [
        31,
        if is_leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];

    let mut month = 1;
    for &days in &days_in_months {
        if remaining_days < days {
            break;
        }
        remaining_days -= days;
        month += 1;
    }

    let day = remaining_days + 1;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cheat_test::{cheat_aware, cheat_reviewed};
    use tempfile::TempDir;

    #[cheat_reviewed("API test - new lockfile is empty")]
    #[test]
    fn test_lockfile_new() {
        let lock = LockFile::new();
        assert!(lock.packages.is_empty());
    }

    #[cheat_reviewed("API test - add_package stores version")]
    #[test]
    fn test_lockfile_add_package() {
        let mut lock = LockFile::new();
        lock.add_package("openssl".to_string(), "3.2.1".to_string());
        assert_eq!(lock.get_version("openssl"), Some(&"3.2.1".to_string()));
    }

    #[cheat_reviewed("API test - contains method")]
    #[test]
    fn test_lockfile_contains() {
        let mut lock = LockFile::new();
        lock.add_package("zlib".to_string(), "1.3.1".to_string());
        assert!(lock.contains("zlib"));
        assert!(!lock.contains("curl"));
    }

    #[cheat_aware(
        protects = "User's lock file survives write-read cycle",
        severity = "HIGH",
        ease = "MEDIUM",
        cheats = [
            "Write but don't actually persist",
            "Read wrong format silently",
            "Lose packages during serialization"
        ],
        consequence = "User's lock file corrupted - reproducible builds broken"
    )]
    #[test]
    fn test_lockfile_write_read() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("recipe.lock");

        let mut lock = LockFile::new();
        lock.add_package("openssl".to_string(), "3.2.1".to_string());
        lock.add_package("zlib".to_string(), "1.3.1".to_string());
        lock.update_metadata();

        lock.write(&path).unwrap();

        let loaded = LockFile::read(&path).unwrap();
        assert_eq!(loaded.packages.len(), 2);
        assert_eq!(loaded.get_version("openssl"), Some(&"3.2.1".to_string()));
        assert_eq!(loaded.get_version("zlib"), Some(&"1.3.1".to_string()));
    }

    #[cheat_reviewed("Validation test - matching versions pass")]
    #[test]
    fn test_lockfile_validate_match() {
        let mut lock = LockFile::new();
        lock.add_package("openssl".to_string(), "3.2.1".to_string());

        let resolved = vec![("openssl".to_string(), "3.2.1".to_string())];
        let mismatches = lock.validate_against(&resolved);
        assert!(mismatches.is_empty());
    }

    #[cheat_aware(
        protects = "User is warned when resolved version doesn't match locked version",
        severity = "HIGH",
        ease = "EASY",
        cheats = [
            "Skip version comparison entirely",
            "Only compare package names",
            "Accept any version as matching"
        ],
        consequence = "User's CI passes with different versions than production - subtle bugs"
    )]
    #[test]
    fn test_lockfile_validate_mismatch() {
        let mut lock = LockFile::new();
        lock.add_package("openssl".to_string(), "3.2.1".to_string());

        let resolved = vec![("openssl".to_string(), "3.3.0".to_string())];
        let mismatches = lock.validate_against(&resolved);
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].0, "openssl");
        assert_eq!(mismatches[0].1, "3.2.1"); // locked
        assert_eq!(mismatches[0].2, "3.3.0"); // resolved
    }

    #[cheat_aware(
        protects = "User is warned when locked package is missing from resolution",
        severity = "HIGH",
        ease = "MEDIUM",
        cheats = [
            "Only check resolved packages, ignore missing locked ones",
            "Treat missing as matching",
            "Skip missing package detection"
        ],
        consequence = "User's dependency removed silently - runtime crash"
    )]
    #[test]
    fn test_lockfile_validate_missing_from_resolved() {
        // BUG FIX: Lock file should detect packages that are locked but missing from resolved
        let mut lock = LockFile::new();
        lock.add_package("openssl".to_string(), "3.2.1".to_string());
        lock.add_package("zlib".to_string(), "1.3.1".to_string());

        // Only openssl is in resolved, zlib is missing
        let resolved = vec![("openssl".to_string(), "3.2.1".to_string())];
        let mismatches = lock.validate_against(&resolved);

        // Should detect that zlib is in lock but missing from resolved
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].0, "zlib");
        assert_eq!(mismatches[0].1, "1.3.1"); // locked version
        assert_eq!(mismatches[0].2, "(missing)"); // marker for missing
    }

    #[cheat_reviewed("Validation test - new packages don't cause mismatch")]
    #[test]
    fn test_lockfile_validate_new_package_ok() {
        // New packages in resolved but not in lock should be OK (not a mismatch)
        let mut lock = LockFile::new();
        lock.add_package("openssl".to_string(), "3.2.1".to_string());

        // curl is new (not in lock)
        let resolved = vec![
            ("openssl".to_string(), "3.2.1".to_string()),
            ("curl".to_string(), "8.5.0".to_string()),
        ];
        let mismatches = lock.validate_against(&resolved);

        // No mismatches - new packages are allowed
        assert!(mismatches.is_empty());
    }

    #[cheat_reviewed("Utility test - timestamp formatting")]
    #[test]
    fn test_chrono_lite_format() {
        // 2024-01-15 10:30:00 UTC
        let ts = 1705315800;
        let formatted = chrono_lite_format(ts);
        assert!(formatted.starts_with("2024-01-15"));
        assert!(formatted.contains("T"));
        assert!(formatted.ends_with("Z"));
    }
}
