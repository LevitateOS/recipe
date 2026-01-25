//! Version comparison utilities
//!
//! Functions for comparing package versions to determine if upgrades are needed.

/// Returns true if an upgrade/install is needed.
///
/// This function compares the installed version against the current recipe version
/// and determines if action is required.
///
/// # Logic
/// - `(None, None)` -> false: Nothing to do
/// - `(Some(_), None)` -> true: Corruption: installed but no current version, reinstall
/// - `(None, Some(_))` -> true: Not installed: need to install
/// - `(Some(inst), Some(curr))` -> true if inst < curr (semver), or inst != curr (fallback)
///
/// # Arguments
/// * `installed` - The version currently installed (from recipe state)
/// * `current` - The version in the recipe file
pub fn is_upgrade_needed(installed: Option<&str>, current: Option<&str>) -> bool {
    match (installed, current) {
        (None, None) => false,      // Nothing to do
        (Some(_), None) => true,    // Corruption: reinstall
        (None, Some(_)) => true,    // Not installed: install
        (Some(inst), Some(curr)) => !version_gte(inst, curr),
    }
}

/// Check if `installed` version is greater than or equal to `current`.
///
/// Tries semver parsing first, falls back to string equality.
fn version_gte(installed: &str, current: &str) -> bool {
    // Try semver parsing first
    if let (Ok(installed_ver), Ok(current_ver)) = (
        semver::Version::parse(installed.trim_start_matches('v')),
        semver::Version::parse(current.trim_start_matches('v')),
    ) {
        // Up to date if installed >= current (no upgrade needed)
        installed_ver >= current_ver
    } else {
        // Fall back to string comparison for non-semver versions
        installed == current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviso_cheat_test::cheat_reviewed;

    #[cheat_reviewed("Version test - both None means no upgrade needed")]
    #[test]
    fn test_upgrade_needed_both_none() {
        assert!(!is_upgrade_needed(None, None));
    }

    #[cheat_reviewed("Version test - installed only means corruption, need reinstall")]
    #[test]
    fn test_upgrade_needed_installed_only() {
        assert!(is_upgrade_needed(Some("1.0.0"), None));
    }

    #[cheat_reviewed("Version test - current only means not installed, need install")]
    #[test]
    fn test_upgrade_needed_current_only() {
        assert!(is_upgrade_needed(None, Some("1.0.0")));
    }

    #[cheat_reviewed("Version test - semver comparison works correctly")]
    #[test]
    fn test_upgrade_needed_semver_compare() {
        // Older installed -> upgrade needed
        assert!(is_upgrade_needed(Some("1.0.0"), Some("2.0.0")));
        assert!(is_upgrade_needed(Some("1.0.0"), Some("1.1.0")));
        assert!(is_upgrade_needed(Some("1.0.0"), Some("1.0.1")));

        // With v prefix
        assert!(is_upgrade_needed(Some("v1.0.0"), Some("v2.0.0")));
    }

    #[cheat_reviewed("Version test - same version means no upgrade needed")]
    #[test]
    fn test_upgrade_not_needed_same_version() {
        assert!(!is_upgrade_needed(Some("1.0.0"), Some("1.0.0")));
        assert!(!is_upgrade_needed(Some("v1.0.0"), Some("v1.0.0")));
    }

    #[cheat_reviewed("Version test - newer installed means no upgrade needed")]
    #[test]
    fn test_upgrade_not_needed_newer_installed() {
        assert!(!is_upgrade_needed(Some("2.0.0"), Some("1.0.0")));
        assert!(!is_upgrade_needed(Some("1.1.0"), Some("1.0.0")));
        assert!(!is_upgrade_needed(Some("1.0.1"), Some("1.0.0")));
    }

    #[cheat_reviewed("Version test - non-semver falls back to string equality")]
    #[test]
    fn test_upgrade_needed_non_semver() {
        // Non-semver versions fall back to string comparison
        assert!(is_upgrade_needed(Some("abc"), Some("def")));
        assert!(!is_upgrade_needed(Some("abc"), Some("abc")));
        assert!(is_upgrade_needed(Some("2024-01"), Some("2024-02")));
    }
}
