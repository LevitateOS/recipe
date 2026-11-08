//! HTTP utilities for recipe scripts
//!
//! Provides helpers for checking updates and fetching remote content.
//!
//! Requests use the invocation's endpoint grants and the compiler image's TLS
//! trust. Ordinary invocations do not inherit coordinator environment secrets.

use crate::core::output;
use rhai::EvalAltResult;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

/// Metadata request timeout. Helpers run in the worker, whose environment
/// carries no coordinator settings, so this is fixed.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// Default GitHub API base URL
const GITHUB_API_BASE: &str = "https://api.github.com";

/// Create an unauthenticated GitHub API request (the worker holds no credentials).
fn github_request(url: &str) -> Result<ureq::Request, Box<EvalAltResult>> {
    let request = super::http_request(url, HTTP_TIMEOUT)?
        .set("Accept", "application/vnd.github.v3+json")
        .set("User-Agent", "recipe-package-manager");

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
    let response = super::http_request(url, HTTP_TIMEOUT)?
        .call()
        .map_err(|error| format!("HTTP GET failed: {error}"))?;
    response_text(response)
}

fn github_error(repo: &str, error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(403, _) => "GitHub API rate limit exceeded. Try again later.".into(),
        ureq::Error::Status(404, _) => format!("Repository '{repo}' not found"),
        error => format!("GitHub API request failed: {error}"),
    }
}

/// Get the latest release version from a GitHub repository
///
/// # Arguments
/// * `repo` - Repository in "owner/repo" format (e.g., "BurntSushi/ripgrep")
///
/// # Returns
/// The latest release tag name (often a version like "14.1.0" or "v14.1.0")
pub fn github_latest_release(repo: &str) -> Result<String, Box<EvalAltResult>> {
    let url = format!("{}/repos/{}/releases/latest", GITHUB_API_BASE, repo);

    let response = github_request(&url)?
        .call()
        .map_err(|e| github_error(repo, e))?;

    let json = response_json(response)?;

    json.get("tag_name")
        .and_then(|v| v.as_str())
        .map(|s| s.strip_prefix('v').unwrap_or(s).to_string())
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
    let url = format!("{}/repos/{}/tags", GITHUB_API_BASE, repo);

    let response = github_request(&url)?
        .call()
        .map_err(|e| github_error(repo, e))?;

    let json = response_json(response)?;

    json.as_array()
        .and_then(|arr| arr.first())
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.strip_prefix('v').unwrap_or(s).to_string())
        .ok_or_else(|| "No tags found in GitHub response".into())
}

/// Parse a version string (extract numeric version from string)
pub fn parse_version(version_str: &str) -> String {
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
    super::verify::validate_sha256(sha256)?;
    let url = format!("{}/repos/{}/releases/latest", GITHUB_API_BASE, repo);
    let response = github_request(&url)?
        .call()
        .map_err(|e| github_error(repo, e))?;

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

    super::download::download(download_url, &dest_path.to_string_lossy(), sha256, false)?;

    output::detail(&format!("downloaded {}", asset_name));
    Ok(dest_path.to_string_lossy().to_string())
}

/// Fetch a SHA256 checksum from a remote checksum file.
///
/// Supported formats:
/// - `SHA256 (filename) = <hash>`
/// - `<hash>  filename` (sha256sum)
///
/// Throws if no matching checksum is found.
pub fn fetch_sha256(url: &str, filename: &str) -> Result<String, Box<EvalAltResult>> {
    let filename = filename.trim();
    if filename.is_empty() {
        return Err("filename must not be empty".into());
    }
    if filename.contains('\n') || filename.contains('\r') {
        return Err("filename must not contain newlines".into());
    }

    output::detail(&format!("fetching sha256 for {} from {}", filename, url));
    let content = http_get(url)?;
    super::verify::parse_sha256_from_checksum_file(&content, filename).ok_or_else(|| {
        format!(
            "sha256 not found for '{}' in checksum file {}",
            filename, url
        )
        .into()
    })
}
