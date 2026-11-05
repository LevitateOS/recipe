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
use std::io::Read;
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
fn github_request(url: &str) -> Result<ureq::Request, Box<EvalAltResult>> {
    let mut request = super::super::http_request(url, get_http_timeout())?
        .set("Accept", "application/vnd.github.v3+json")
        .set("User-Agent", "recipe-package-manager");

    if let Some(token) = get_github_token() {
        request = request.set("Authorization", &format!("Bearer {}", token));
    }

    Ok(request)
}

fn response_text(response: ureq::Response) -> Result<String, Box<EvalAltResult>> {
    const LIMIT: u64 = 8 * 1024 * 1024;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read HTTP metadata: {error}"))?;
    if bytes.len() as u64 > LIMIT {
        return Err("HTTP metadata exceeds 8 MiB".into());
    }
    String::from_utf8(bytes).map_err(|error| format!("HTTP metadata is not UTF-8: {error}").into())
}

fn response_json(response: ureq::Response) -> Result<serde_json::Value, Box<EvalAltResult>> {
    serde_json::from_str(&response_text(response)?)
        .map_err(|error| format!("cannot parse HTTP metadata: {error}").into())
}

/// Fetch content from a URL (GET request)
pub fn http_get(url: &str) -> Result<String, Box<EvalAltResult>> {
    let response = super::super::http_request(url, get_http_timeout())?
        .call()
        .map_err(|error| format!("HTTP GET failed: {error}"))?;
    response_text(response)
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

    let response = github_request(&url)?.call().map_err(|e| {
        // Handle rate limiting specifically
        if let ureq::Error::Status(403, _) = e {
            return "GitHub API rate limit exceeded. Try again later or set GITHUB_TOKEN.".into();
        }
        if let ureq::Error::Status(404, _) = e {
            return format!("Repository '{}' not found", repo);
        }
        format!("GitHub API request failed: {}", e)
    })?;

    let json = response_json(response)?;

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

    let response = github_request(&url)?.call().map_err(|e| {
        // Handle rate limiting specifically
        if let ureq::Error::Status(403, _) = e {
            return "GitHub API rate limit exceeded. Try again later or set GITHUB_TOKEN.".into();
        }
        if let ureq::Error::Status(404, _) = e {
            return format!("Repository '{}' not found", repo);
        }
        format!("GitHub API request failed: {}", e)
    })?;

    let json = response_json(response)?;

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
/// let path = github_download_release("owner/tool", "tool-1.0.tar.gz", ctx.runtime.paths.build, ctx.sha256);
/// ```
pub fn github_download_release(
    repo: &str,
    asset_pattern: &str,
    dest_dir: &str,
    sha256: &str,
) -> Result<String, Box<EvalAltResult>> {
    super::super::download::validate_sha256(sha256)?;
    github_download_release_impl(repo, asset_pattern, dest_dir, GITHUB_API_BASE, sha256)
}

/// Internal implementation with configurable base URL (for testing)
fn github_download_release_impl(
    repo: &str,
    asset_pattern: &str,
    dest_dir: &str,
    base_url: &str,
    sha256: &str,
) -> Result<String, Box<EvalAltResult>> {
    // Get latest release info
    let url = format!("{}/repos/{}/releases/latest", base_url, repo);
    let response = github_request(&url)?.call().map_err(|e| {
        if let ureq::Error::Status(403, _) = e {
            return "GitHub API rate limit exceeded. Try again later or set GITHUB_TOKEN.".into();
        }
        if let ureq::Error::Status(404, _) = e {
            return format!("Repository '{}' not found", repo);
        }
        format!("GitHub API request failed: {}", e)
    })?;

    let json = response_json(response)?;

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
    if Path::new(asset_name)
        .file_name()
        .and_then(|name| name.to_str())
        != Some(asset_name)
    {
        return Err(format!("Unsafe GitHub asset filename: {}", asset_name).into());
    }
    let download_url = asset
        .get("browser_download_url")
        .and_then(|u| u.as_str())
        .ok_or("Asset missing download URL")?;

    output::detail(&format!("downloading {} from {}", asset_name, repo));

    // Download the asset
    let dest_path = Path::new(dest_dir).join(asset_name);

    super::super::download::download(download_url, &dest_path.to_string_lossy(), sha256, false)?;

    output::detail(&format!("downloaded {}", asset_name));
    Ok(dest_path.to_string_lossy().to_string())
}

/// Get release assets metadata for a GitHub repository
///
/// Returns a list of asset names and download URLs for the latest release.
/// Useful for scripting when you need to inspect available assets.
pub fn github_release_assets(repo: &str) -> Result<Vec<(String, String)>, Box<EvalAltResult>> {
    let url = format!("{}/repos/{}/releases/latest", GITHUB_API_BASE, repo);
    let response = github_request(&url)?.call().map_err(|e| {
        if let ureq::Error::Status(403, _) = e {
            return "GitHub API rate limit exceeded. Try again later or set GITHUB_TOKEN.".into();
        }
        if let ureq::Error::Status(404, _) = e {
            return format!("Repository '{}' not found", repo);
        }
        format!("GitHub API request failed: {}", e)
    })?;

    let json = response_json(response)?;

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
