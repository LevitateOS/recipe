//! HTTP utilities for recipe scripts
//!
//! Provides helpers for checking updates and fetching remote content.
//!
//! ## GitHub Authentication
//!
//! Set `GITHUB_TOKEN` environment variable to increase rate limits:
//! ```bash
//! export GITHUB_TOKEN="ghp_xxxxxxxxxxxxxxxxxxxx"
//! ```

use crate::core::output;
use rhai::EvalAltResult;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

/// Default HTTP timeout in seconds
const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 30;

/// Get HTTP timeout from environment variable or use default.
/// Cached for performance (only reads env var once).
pub(crate) fn get_http_timeout() -> Duration {
    static TIMEOUT: OnceLock<Duration> = OnceLock::new();
    *TIMEOUT.get_or_init(|| {
        let secs = std::env::var("RECIPE_HTTP_TIMEOUT")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_HTTP_TIMEOUT_SECS);
        // Clamp to reasonable range (5-300 seconds)
        Duration::from_secs(secs.clamp(5, 300))
    })
}

/// Default GitHub API base URL
const GITHUB_API_BASE: &str = "https://api.github.com";

/// Get GitHub token from environment, if set.
/// Tokens increase rate limits from 60/hr to 5000/hr.
fn get_github_token() -> Option<String> {
    std::env::var("GITHUB_TOKEN").ok()
}

/// Create a GitHub API request builder with proper headers and optional auth.
fn github_request(url: &str) -> ureq::Request {
    let mut request = ureq::get(url)
        .timeout(get_http_timeout())
        .set("Accept", "application/vnd.github.v3+json")
        .set("User-Agent", "recipe-package-manager");

    if let Some(token) = get_github_token() {
        request = request.set("Authorization", &format!("Bearer {}", token));
    }

    request
}

/// Fetch content from a URL (GET request)
pub fn http_get(url: &str) -> Result<String, Box<EvalAltResult>> {
    ureq::get(url)
        .timeout(get_http_timeout())
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
    github_latest_release_with_base(repo, GITHUB_API_BASE)
}

/// Internal: Get latest release with configurable base URL (for testing)
pub(crate) fn github_latest_release_with_base(
    repo: &str,
    base_url: &str,
) -> Result<String, Box<EvalAltResult>> {
    let url = format!("{}/repos/{}/releases/latest", base_url, repo);

    let response = github_request(&url).call().map_err(|e| {
        // Handle rate limiting specifically
        if let ureq::Error::Status(403, _) = e {
            return "GitHub API rate limit exceeded. Try again later or set GITHUB_TOKEN.".into();
        }
        if let ureq::Error::Status(404, _) = e {
            return format!("Repository '{}' not found", repo);
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
    github_latest_tag_with_base(repo, GITHUB_API_BASE)
}

/// Internal: Get latest tag with configurable base URL (for testing)
pub(crate) fn github_latest_tag_with_base(
    repo: &str,
    base_url: &str,
) -> Result<String, Box<EvalAltResult>> {
    let url = format!("{}/repos/{}/tags", base_url, repo);

    let response = github_request(&url).call().map_err(|e| {
        // Handle rate limiting specifically
        if let ureq::Error::Status(403, _) = e {
            return "GitHub API rate limit exceeded. Try again later or set GITHUB_TOKEN.".into();
        }
        if let ureq::Error::Status(404, _) = e {
            return format!("Repository '{}' not found", repo);
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

/// Download a release asset from GitHub
///
/// # Arguments
/// * `repo` - Repository in "owner/repo" format (e.g., "LevitateOS/recstrap")
/// * `asset_pattern` - Glob pattern to match asset name (e.g., "recstrap-*.tar.gz")
/// * `dest_dir` - Directory to save the downloaded file
///
/// # Returns
/// The path to the downloaded file
///
/// # Example
/// ```rhai
/// let path = github_download_release("LevitateOS/recstrap", "recstrap-x86_64-*.tar.gz", BUILD_DIR);
/// ```
pub fn github_download_release(
    repo: &str,
    asset_pattern: &str,
    dest_dir: &str,
) -> Result<String, Box<EvalAltResult>> {
    github_download_release_impl(repo, asset_pattern, dest_dir, GITHUB_API_BASE)
}

/// Internal implementation with configurable base URL (for testing)
fn github_download_release_impl(
    repo: &str,
    asset_pattern: &str,
    dest_dir: &str,
    base_url: &str,
) -> Result<String, Box<EvalAltResult>> {
    // Get latest release info
    let url = format!("{}/repos/{}/releases/latest", base_url, repo);
    let response = github_request(&url).call().map_err(|e| {
        if let ureq::Error::Status(403, _) = e {
            return "GitHub API rate limit exceeded. Try again later or set GITHUB_TOKEN.".into();
        }
        if let ureq::Error::Status(404, _) = e {
            return format!("Repository '{}' not found", repo);
        }
        format!("GitHub API request failed: {}", e)
    })?;

    let json: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Failed to parse GitHub response: {}", e))?;

    // Find matching asset
    let assets = json
        .get("assets")
        .and_then(|a| a.as_array())
        .ok_or("No assets found in release")?;

    let pattern = glob::Pattern::new(asset_pattern)
        .map_err(|e| format!("Invalid asset pattern '{}': {}", asset_pattern, e))?;

    let asset = assets
        .iter()
        .find(|a| {
            a.get("name")
                .and_then(|n| n.as_str())
                .is_some_and(|name| pattern.matches(name))
        })
        .ok_or_else(|| {
            format!(
                "No asset matching '{}' found in release for {}",
                asset_pattern, repo
            )
        })?;

    let asset_name = asset
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or("Asset missing name")?;
    let download_url = asset
        .get("browser_download_url")
        .and_then(|u| u.as_str())
        .ok_or("Asset missing download URL")?;

    output::detail(&format!("downloading {} from {}", asset_name, repo));

    // Download the asset
    let dest_path = Path::new(dest_dir).join(asset_name);

    let response = ureq::get(download_url)
        .timeout(Duration::from_secs(300)) // 5 minute timeout for downloads
        .call()
        .map_err(|e| format!("Download failed: {}", e))?;

    let mut reader = response.into_reader();
    let mut file =
        std::fs::File::create(&dest_path).map_err(|e| format!("Failed to create file: {}", e))?;
    std::io::copy(&mut reader, &mut file).map_err(|e| format!("Failed to write file: {}", e))?;

    output::detail(&format!("downloaded {}", asset_name));
    Ok(dest_path.to_string_lossy().to_string())
}

/// Get release assets metadata for a GitHub repository
///
/// Returns a list of asset names and download URLs for the latest release.
/// Useful for scripting when you need to inspect available assets.
pub fn github_release_assets(repo: &str) -> Result<Vec<(String, String)>, Box<EvalAltResult>> {
    let url = format!("{}/repos/{}/releases/latest", GITHUB_API_BASE, repo);
    let response = github_request(&url).call().map_err(|e| {
        if let ureq::Error::Status(403, _) = e {
            return "GitHub API rate limit exceeded. Try again later or set GITHUB_TOKEN.".into();
        }
        if let ureq::Error::Status(404, _) = e {
            return format!("Repository '{}' not found", repo);
        }
        format!("GitHub API request failed: {}", e)
    })?;

    let json: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("Failed to parse GitHub response: {}", e))?;

    let assets = json
        .get("assets")
        .and_then(|a| a.as_array())
        .ok_or("No assets found in release")?;

    let result: Vec<(String, String)> = assets
        .iter()
        .filter_map(|a| {
            let name = a.get("name")?.as_str()?.to_string();
            let url = a.get("browser_download_url")?.as_str()?.to_string();
            Some((name, url))
        })
        .collect();

    Ok(result)
}

/// Extract a single file from a tarball
///
/// # Arguments
/// * `archive` - Path to the tar.gz archive
/// * `file_pattern` - Pattern to match the file inside (e.g., "*/recstrap" or "bin/tool")
/// * `dest` - Destination path for the extracted file
///
/// # Example
/// ```rhai
/// let output_dir = RECIPE_DIR + "/output";
/// extract_from_tarball("tool-1.0.tar.gz", "*/bin/tool", output_dir + "/bin/tool");
/// ```
pub fn extract_from_tarball(
    archive: &str,
    file_pattern: &str,
    dest: &str,
) -> Result<(), Box<EvalAltResult>> {
    use std::process::Command;

    let archive_path = Path::new(archive);
    if !archive_path.exists() {
        return Err(format!("Archive not found: {}", archive).into());
    }

    // Create parent directory for destination
    let dest_path = Path::new(dest);
    if let Some(parent) = dest_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    // Determine archive type and extract
    let archive_str = archive_path.to_string_lossy();
    let tar_args = if archive_str.ends_with(".tar.gz") || archive_str.ends_with(".tgz") {
        vec!["xzf", archive, "--wildcards", file_pattern, "-O"]
    } else if archive_str.ends_with(".tar.xz") || archive_str.ends_with(".txz") {
        vec!["xJf", archive, "--wildcards", file_pattern, "-O"]
    } else if archive_str.ends_with(".tar.bz2") || archive_str.ends_with(".tbz2") {
        vec!["xjf", archive, "--wildcards", file_pattern, "-O"]
    } else {
        return Err(format!(
            "Unsupported archive format: {}. Use tar.gz, tar.xz, or tar.bz2.",
            archive
        )
        .into());
    };

    let output = Command::new("tar")
        .args(&tar_args)
        .output()
        .map_err(|e| format!("Failed to run tar: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Failed to extract '{}' from {}: {}",
            file_pattern, archive, stderr
        )
        .into());
    }

    // Write the extracted content to destination
    std::fs::write(dest, &output.stdout)
        .map_err(|e| format!("Failed to write to {}: {}", dest, e))?;

    output::detail(&format!("extracted {} -> {}", file_pattern, dest));
    Ok(())
}
