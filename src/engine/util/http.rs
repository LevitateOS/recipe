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
    version_str
        .trim_start_matches('v')
        .trim_start_matches("release-")
        .trim_start_matches("version-")
        .to_string()
}
