//! HTTP utilities for recipe scripts
//!
//! Provides helpers for checking updates and fetching remote content.

use rhai::EvalAltResult;
use std::time::Duration;

/// Default HTTP timeout in seconds
const HTTP_TIMEOUT_SECS: u64 = 30;

/// Fetch content from a URL (GET request)
pub fn http_get(url: &str) -> Result<String, Box<EvalAltResult>> {
    ureq::get(url)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .call()
        .map_err(|e| format!("HTTP GET failed: {}", e))?
        .into_string()
        .map_err(|e| format!("Failed to read response: {}", e).into())
}

/// Get the latest release version from a GitHub repository
///
/// # Arguments
/// * `repo` - Repository in "owner/repo" format (e.g., "BurntSushi/ripgrep")
///
/// # Returns
/// The latest release tag name (often a version like "14.1.0" or "v14.1.0")
pub fn github_latest_release(repo: &str) -> Result<String, Box<EvalAltResult>> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo);

    let response = ureq::get(&url)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .set("Accept", "application/vnd.github.v3+json")
        .set("User-Agent", "recipe-package-manager")
        .call()
        .map_err(|e| {
            // Handle rate limiting specifically
            if let ureq::Error::Status(403, _) = e {
                return "GitHub API rate limit exceeded. Try again later or set GITHUB_TOKEN.".into();
            }
            if let ureq::Error::Status(404, _) = e {
                return format!("Repository '{}' not found", repo).into();
            }
            format!("GitHub API request failed: {}", e)
        })?;

    let json: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Failed to parse GitHub response: {}", e))?;

    json.get("tag_name")
        .and_then(|v| v.as_str())
        .map(|s| s.trim_start_matches('v').to_string())
        .ok_or_else(|| "No tag_name in GitHub response".into())
}

/// Get the latest tag from a GitHub repository (for repos without releases)
///
/// # Arguments
/// * `repo` - Repository in "owner/repo" format
///
/// # Returns
/// The latest tag name
pub fn github_latest_tag(repo: &str) -> Result<String, Box<EvalAltResult>> {
    let url = format!("https://api.github.com/repos/{}/tags", repo);

    let response = ureq::get(&url)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .set("Accept", "application/vnd.github.v3+json")
        .set("User-Agent", "recipe-package-manager")
        .call()
        .map_err(|e| {
            // Handle rate limiting specifically
            if let ureq::Error::Status(403, _) = e {
                return "GitHub API rate limit exceeded. Try again later or set GITHUB_TOKEN.".into();
            }
            if let ureq::Error::Status(404, _) = e {
                return format!("Repository '{}' not found", repo).into();
            }
            format!("GitHub API request failed: {}", e)
        })?;

    let json: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Failed to parse GitHub response: {}", e))?;

    json.as_array()
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim_start_matches('v').to_string())
        .ok_or_else(|| "No tags found in GitHub response".into())
}

/// Parse a version string (extract numeric version from string)
pub fn parse_version(version_str: &str) -> String {
    // Strip common prefixes like "v" or "release-"
    // Check longer prefixes first to avoid partial matches
    let s = version_str;
    let s = s.strip_prefix("release-").unwrap_or(s);
    let s = s.strip_prefix("version-").unwrap_or(s);
    let s = s.strip_prefix('v').unwrap_or(s);
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== parse_version tests ====================

    #[test]
    fn test_parse_version_strips_v_prefix() {
        assert_eq!(parse_version("v1.0.0"), "1.0.0");
        assert_eq!(parse_version("v14.1.0"), "14.1.0");
    }

    #[test]
    fn test_parse_version_strips_release_prefix() {
        assert_eq!(parse_version("release-1.0.0"), "1.0.0");
        assert_eq!(parse_version("release-2.5.3"), "2.5.3");
    }

    #[test]
    fn test_parse_version_strips_version_prefix() {
        assert_eq!(parse_version("version-1.0.0"), "1.0.0");
        assert_eq!(parse_version("version-3.2.1"), "3.2.1");
    }

    #[test]
    fn test_parse_version_no_prefix() {
        assert_eq!(parse_version("1.0.0"), "1.0.0");
        assert_eq!(parse_version("14.1.0"), "14.1.0");
    }

    #[test]
    fn test_parse_version_empty() {
        assert_eq!(parse_version(""), "");
    }

    #[test]
    fn test_parse_version_only_v() {
        assert_eq!(parse_version("v"), "");
    }

    #[test]
    fn test_parse_version_preserves_suffix() {
        assert_eq!(parse_version("v1.0.0-beta"), "1.0.0-beta");
        assert_eq!(parse_version("v1.0.0-rc1"), "1.0.0-rc1");
        assert_eq!(parse_version("v1.0.0+build.123"), "1.0.0+build.123");
    }

    #[test]
    fn test_parse_version_multiple_prefixes() {
        // Strips prefixes in order: release-, version-, then v
        // So "vv1.0.0" strips one 'v' -> "v1.0.0"
        assert_eq!(parse_version("vv1.0.0"), "v1.0.0");
        // "release-v1.0.0" strips "release-" -> "v1.0.0", then 'v' -> "1.0.0"
        assert_eq!(parse_version("release-v1.0.0"), "1.0.0");
    }

    // ==================== http_get tests ====================

    #[test]
    fn test_http_get_invalid_url() {
        let result = http_get("not-a-valid-url");
        assert!(result.is_err());
    }

    #[test]
    fn test_http_get_nonexistent_domain() {
        let result = http_get("https://this-domain-does-not-exist-12345.com/");
        assert!(result.is_err());
    }

    // Integration tests - require network, run with: cargo test -- --ignored

    #[test]
    #[ignore]
    fn test_http_get_real_url() {
        // Test with a known stable URL
        let result = http_get("https://httpbin.org/get");
        assert!(result.is_ok());
        assert!(result.unwrap().contains("httpbin"));
    }

    #[test]
    #[ignore]
    fn test_github_latest_release_real() {
        // Test with a well-known repo
        let result = github_latest_release("BurntSushi/ripgrep");
        assert!(result.is_ok());
        // ripgrep versions are like "14.1.0"
        let version = result.unwrap();
        assert!(!version.is_empty());
        assert!(version.chars().next().unwrap().is_ascii_digit());
    }

    #[test]
    #[ignore]
    fn test_github_latest_release_nonexistent_repo() {
        let result = github_latest_release("nonexistent-owner/nonexistent-repo-12345");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    #[ignore]
    fn test_github_latest_tag_real() {
        // Test with a repo that uses tags
        let result = github_latest_tag("torvalds/linux");
        assert!(result.is_ok());
        let tag = result.unwrap();
        assert!(!tag.is_empty());
    }

    // ==================== Timeout constant ====================

    #[test]
    fn test_timeout_is_reasonable() {
        // Timeout should be between 5 and 120 seconds
        assert!(HTTP_TIMEOUT_SECS >= 5);
        assert!(HTTP_TIMEOUT_SECS <= 120);
    }
}
