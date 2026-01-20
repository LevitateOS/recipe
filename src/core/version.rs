//! Version constraint parsing and validation
//!
//! Supports dependency specifications with version constraints:
//!
//! ```rhai
//! let deps = [
//!     "core",                  // Any version
//!     "openssl >= 3.0.0",      // Minimum version
//!     "zlib >= 1.2, < 1.3",    // Range
//!     "readline ^8.0",         // Compatible (>=8.0.0, <9.0.0)
//!     "ncurses ~6.4",          // Patch-level (>=6.4.0, <6.5.0)
//!     "exact = 1.2.3",         // Exact version
//! ];
//! ```

use anyhow::{bail, Result};
use semver::{Version, VersionReq};
use std::fmt;

/// A parsed dependency with optional version constraint
#[derive(Debug, Clone)]
pub struct Dependency {
    /// Package name
    pub name: String,
    /// Optional version constraint (None means any version)
    pub constraint: Option<VersionReq>,
    /// Original constraint string for display
    pub constraint_str: Option<String>,
}

impl Dependency {
    /// Parse a dependency specification string
    ///
    /// Formats supported:
    /// - "package" - any version
    /// - "package >= 1.0" - minimum version
    /// - "package >= 1.0, < 2.0" - range
    /// - "package ^1.0" - compatible version
    /// - "package ~1.0" - patch-level compatible
    /// - "package == 1.0" - exact version
    pub fn parse(spec: &str) -> Result<Self> {
        let spec = spec.trim();

        if spec.is_empty() {
            bail!("Empty dependency specification");
        }

        // Find where the name ends and constraint begins
        // Name is alphanumeric + hyphen + underscore
        let name_end = spec
            .find(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            .unwrap_or(spec.len());

        let name = spec[..name_end].trim().to_string();
        if name.is_empty() {
            bail!("Empty package name in dependency: {}", spec);
        }

        let constraint_part = spec[name_end..].trim();

        if constraint_part.is_empty() {
            // No constraint - any version
            return Ok(Dependency {
                name,
                constraint: None,
                constraint_str: None,
            });
        }

        // Parse the version constraint using semver
        let constraint = VersionReq::parse(constraint_part).map_err(|e| {
            anyhow::anyhow!(
                "Invalid version constraint '{}' for '{}': {}",
                constraint_part,
                name,
                e
            )
        })?;

        Ok(Dependency {
            name,
            constraint: Some(constraint),
            constraint_str: Some(constraint_part.to_string()),
        })
    }

    /// Check if a version satisfies this dependency's constraint
    pub fn satisfied_by(&self, version: &str) -> Result<bool> {
        match &self.constraint {
            None => Ok(true), // Any version satisfies no constraint
            Some(req) => {
                // Try to parse as semver, falling back to simple comparison
                match Version::parse(version) {
                    Ok(v) => Ok(req.matches(&v)),
                    Err(_) => {
                        // Try adding .0 suffixes for partial versions
                        let padded = pad_version(version);
                        match Version::parse(&padded) {
                            Ok(v) => Ok(req.matches(&v)),
                            Err(_) => {
                                // Can't parse version - assume it doesn't satisfy
                                Ok(false)
                            }
                        }
                    }
                }
            }
        }
    }

    /// Get the package name
    pub fn package_name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for Dependency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.constraint_str {
            Some(c) => write!(f, "{} {}", self.name, c),
            None => write!(f, "{}", self.name),
        }
    }
}

/// Pad a version string to be semver-compatible (X.Y.Z)
fn pad_version(version: &str) -> String {
    let parts: Vec<&str> = version.split('.').collect();
    match parts.len() {
        1 => format!("{}.0.0", parts[0]),
        2 => format!("{}.{}.0", parts[0], parts[1]),
        _ => version.to_string(),
    }
}

/// Version conflict information
#[derive(Debug)]
pub struct VersionConflict {
    /// Package that has the conflict
    pub package: String,
    /// Packages requesting different versions: (requester, constraint)
    pub requesters: Vec<(String, String)>,
    /// Actually available version
    pub available: String,
}

impl fmt::Display for VersionConflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Version conflict for '{}':", self.package)?;
        writeln!(f, "  Available: {}", self.available)?;
        for (requester, req) in &self.requesters {
            writeln!(f, "  {} requires {}", requester, req)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_name() {
        let dep = Dependency::parse("openssl").unwrap();
        assert_eq!(dep.name, "openssl");
        assert!(dep.constraint.is_none());
    }

    #[test]
    fn test_parse_with_hyphen() {
        let dep = Dependency::parse("my-package").unwrap();
        assert_eq!(dep.name, "my-package");
    }

    #[test]
    fn test_parse_with_underscore() {
        let dep = Dependency::parse("my_package").unwrap();
        assert_eq!(dep.name, "my_package");
    }

    #[test]
    fn test_parse_minimum_version() {
        let dep = Dependency::parse("openssl >= 3.0.0").unwrap();
        assert_eq!(dep.name, "openssl");
        assert!(dep.constraint.is_some());
        assert!(dep.satisfied_by("3.0.0").unwrap());
        assert!(dep.satisfied_by("3.1.0").unwrap());
        assert!(!dep.satisfied_by("2.9.0").unwrap());
    }

    #[test]
    fn test_parse_maximum_version() {
        let dep = Dependency::parse("zlib < 2.0.0").unwrap();
        assert_eq!(dep.name, "zlib");
        assert!(dep.satisfied_by("1.9.0").unwrap());
        assert!(!dep.satisfied_by("2.0.0").unwrap());
    }

    #[test]
    fn test_parse_range() {
        let dep = Dependency::parse("ncurses >= 6.0, < 7.0").unwrap();
        assert_eq!(dep.name, "ncurses");
        assert!(dep.satisfied_by("6.0.0").unwrap());
        assert!(dep.satisfied_by("6.5.0").unwrap());
        assert!(!dep.satisfied_by("5.9.0").unwrap());
        assert!(!dep.satisfied_by("7.0.0").unwrap());
    }

    #[test]
    fn test_parse_caret() {
        // ^1.2.3 means >=1.2.3, <2.0.0
        let dep = Dependency::parse("readline ^8.0").unwrap();
        assert_eq!(dep.name, "readline");
        assert!(dep.satisfied_by("8.0.0").unwrap());
        assert!(dep.satisfied_by("8.5.0").unwrap());
        assert!(!dep.satisfied_by("9.0.0").unwrap());
    }

    #[test]
    fn test_parse_tilde() {
        // ~1.2.3 means >=1.2.3, <1.3.0
        let dep = Dependency::parse("ncurses ~6.4").unwrap();
        assert_eq!(dep.name, "ncurses");
        assert!(dep.satisfied_by("6.4.0").unwrap());
        assert!(dep.satisfied_by("6.4.5").unwrap());
        assert!(!dep.satisfied_by("6.5.0").unwrap());
    }

    #[test]
    fn test_parse_exact() {
        // Use = for exact version (semver syntax)
        let dep = Dependency::parse("exact = 1.2.3").unwrap();
        assert_eq!(dep.name, "exact");
        assert!(dep.satisfied_by("1.2.3").unwrap());
        assert!(!dep.satisfied_by("1.2.4").unwrap());
        assert!(!dep.satisfied_by("1.2.2").unwrap());
    }

    #[test]
    fn test_parse_equals_works() {
        let dep = Dependency::parse("pkg = 1.0.0").unwrap();
        assert!(dep.satisfied_by("1.0.0").unwrap());
    }

    #[test]
    fn test_parse_empty_error() {
        assert!(Dependency::parse("").is_err());
    }

    #[test]
    fn test_parse_whitespace_only_error() {
        assert!(Dependency::parse("   ").is_err());
    }

    #[test]
    fn test_parse_invalid_constraint() {
        // Invalid operator
        let result = Dependency::parse("pkg >< 1.0");
        assert!(result.is_err());
    }

    #[test]
    fn test_satisfied_by_partial_version() {
        let dep = Dependency::parse("pkg >= 1.0").unwrap();
        // Should handle versions like "1.0" by padding to "1.0.0"
        assert!(dep.satisfied_by("1.0").unwrap());
        assert!(dep.satisfied_by("1").unwrap());
    }

    #[test]
    fn test_display_with_constraint() {
        let dep = Dependency::parse("openssl >= 3.0.0").unwrap();
        assert_eq!(format!("{}", dep), "openssl >= 3.0.0");
    }

    #[test]
    fn test_display_without_constraint() {
        let dep = Dependency::parse("openssl").unwrap();
        assert_eq!(format!("{}", dep), "openssl");
    }

    #[test]
    fn test_pad_version() {
        assert_eq!(pad_version("1"), "1.0.0");
        assert_eq!(pad_version("1.2"), "1.2.0");
        assert_eq!(pad_version("1.2.3"), "1.2.3");
        assert_eq!(pad_version("1.2.3.4"), "1.2.3.4");
    }

    // ==================== Non-Semver Version Edge Cases ====================

    #[test]
    fn test_non_semver_version_with_hyphen_suffix() {
        // Versions like "5.2.26-1" (common in distro packages) are valid semver pre-release
        // IMPORTANT: semver's VersionReq::matches() has strict pre-release matching rules.
        // See: https://docs.rs/semver/latest/semver/struct.VersionReq.html#method.matches
        //
        // Key behavior: ">= 5.0.0" does NOT match "5.2.26-1" because:
        // 1. Pre-release versions are only matched when the comparator targets the same X.Y.Z
        // 2. This is intentional to prevent accidental use of pre-releases in production
        //
        // For distro-style versions like "5.2.26-1", users should:
        // - Either use exact version constraints: "= 5.2.26-1"
        // - Or strip the suffix before comparing
        let dep = Dependency::parse("bash >= 5.0.0").unwrap();
        assert!(dep.satisfied_by("5.2.26").unwrap());
        // Pre-release versions DON'T satisfy non-pre-release constraints
        assert!(!dep.satisfied_by("5.2.26-1").unwrap());
    }

    #[test]
    fn test_non_semver_version_with_alpha() {
        // Versions like "1.0-rc1" or "1.0.0-alpha"
        let dep = Dependency::parse("pkg >= 1.0.0").unwrap();
        // Pre-release versions are NOT matched by non-pre-release constraints in semver
        // (semver crate behavior - designed to protect against accidental pre-release usage)
        assert!(!dep.satisfied_by("1.0.0-alpha").unwrap());
        assert!(!dep.satisfied_by("1.0.0-rc1").unwrap());
        assert!(dep.satisfied_by("1.0.0").unwrap());
        assert!(dep.satisfied_by("1.0.1").unwrap());
    }

    #[test]
    fn test_completely_invalid_version_returns_false() {
        // Versions that can't be parsed at all should return false, not error
        let dep = Dependency::parse("pkg >= 1.0.0").unwrap();
        // "invalid" can't be parsed as semver, so constraint is not satisfied
        assert!(!dep.satisfied_by("invalid").unwrap());
        assert!(!dep.satisfied_by("").unwrap());
        assert!(!dep.satisfied_by("abc.def.ghi").unwrap());
    }

    #[test]
    fn test_no_constraint_accepts_any_version() {
        // No constraint means any version is accepted, even invalid ones
        let dep = Dependency::parse("pkg").unwrap();
        assert!(dep.satisfied_by("1.0.0").unwrap());
        assert!(dep.satisfied_by("invalid").unwrap());
        assert!(dep.satisfied_by("").unwrap());
    }

    #[test]
    fn test_version_with_build_metadata() {
        // semver supports build metadata like "1.0.0+build123"
        let dep = Dependency::parse("pkg >= 1.0.0").unwrap();
        assert!(dep.satisfied_by("1.0.0+build123").unwrap());
        assert!(dep.satisfied_by("1.0.1+metadata").unwrap());
    }

    #[test]
    fn test_version_with_v_prefix() {
        // Some projects use "v1.0.0" format - semver doesn't accept this
        let dep = Dependency::parse("pkg >= 1.0.0").unwrap();
        // "v1.0.0" is not valid semver, will return false
        assert!(!dep.satisfied_by("v1.0.0").unwrap());
    }

    #[test]
    fn test_four_part_version() {
        // Four-part versions like "1.2.3.4" are not valid semver
        let dep = Dependency::parse("pkg >= 1.2.0").unwrap();
        // pad_version doesn't help here - it's already 4 parts
        // semver will fail to parse "1.2.3.4"
        assert!(!dep.satisfied_by("1.2.3.4").unwrap());
    }
}
