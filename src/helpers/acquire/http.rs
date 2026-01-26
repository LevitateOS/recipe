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
fn get_http_timeout() -> Duration {
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
fn github_latest_release_with_base(
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
fn github_latest_tag_with_base(repo: &str, base_url: &str) -> Result<String, Box<EvalAltResult>> {
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
    let mut file = std::fs::File::create(&dest_path)
        .map_err(|e| format!("Failed to create file: {}", e))?;
    std::io::copy(&mut reader, &mut file)
        .map_err(|e| format!("Failed to write file: {}", e))?;

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
        return Err(format!("Failed to extract '{}' from {}: {}", file_pattern, archive, stderr)
            .into());
    }

    // Write the extracted content to destination
    std::fs::write(dest, &output.stdout)
        .map_err(|e| format!("Failed to write to {}: {}", dest, e))?;

    output::detail(&format!("extracted {} -> {}", file_pattern, dest));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviso_cheat_test::cheat_reviewed;

    // ==================== parse_version tests ====================

    #[cheat_reviewed("Parsing test - strips v prefix")]
    #[test]
    fn test_parse_version_strips_v_prefix() {
        assert_eq!(parse_version("v1.0.0"), "1.0.0");
        assert_eq!(parse_version("v14.1.0"), "14.1.0");
    }

    #[cheat_reviewed("Parsing test - strips release- prefix")]
    #[test]
    fn test_parse_version_strips_release_prefix() {
        assert_eq!(parse_version("release-1.0.0"), "1.0.0");
        assert_eq!(parse_version("release-2.5.3"), "2.5.3");
    }

    #[cheat_reviewed("Parsing test - strips version- prefix")]
    #[test]
    fn test_parse_version_strips_version_prefix() {
        assert_eq!(parse_version("version-1.0.0"), "1.0.0");
        assert_eq!(parse_version("version-3.2.1"), "3.2.1");
    }

    #[cheat_reviewed("Parsing test - preserves versions without prefix")]
    #[test]
    fn test_parse_version_no_prefix() {
        assert_eq!(parse_version("1.0.0"), "1.0.0");
        assert_eq!(parse_version("14.1.0"), "14.1.0");
    }

    #[cheat_reviewed("Edge case - empty version string")]
    #[test]
    fn test_parse_version_empty() {
        assert_eq!(parse_version(""), "");
    }

    #[cheat_reviewed("Edge case - version is just 'v'")]
    #[test]
    fn test_parse_version_only_v() {
        assert_eq!(parse_version("v"), "");
    }

    #[cheat_reviewed("Parsing test - preserves semver suffix")]
    #[test]
    fn test_parse_version_preserves_suffix() {
        assert_eq!(parse_version("v1.0.0-beta"), "1.0.0-beta");
        assert_eq!(parse_version("v1.0.0-rc1"), "1.0.0-rc1");
        assert_eq!(parse_version("v1.0.0+build.123"), "1.0.0+build.123");
    }

    #[cheat_reviewed("Parsing test - handles nested prefixes")]
    #[test]
    fn test_parse_version_multiple_prefixes() {
        // Strips prefixes in order: release-, version-, then v
        // So "vv1.0.0" strips one 'v' -> "v1.0.0"
        assert_eq!(parse_version("vv1.0.0"), "v1.0.0");
        // "release-v1.0.0" strips "release-" -> "v1.0.0", then 'v' -> "1.0.0"
        assert_eq!(parse_version("release-v1.0.0"), "1.0.0");
    }

    // ==================== http_get tests ====================

    #[cheat_reviewed("Error handling - invalid URL format rejected")]
    #[test]
    fn test_http_get_invalid_url() {
        let result = http_get("not-a-valid-url");
        assert!(result.is_err());
    }

    #[cheat_reviewed("Error handling - nonexistent domain fails")]
    #[test]
    fn test_http_get_nonexistent_domain() {
        let result = http_get("https://this-domain-does-not-exist-12345.com/");
        assert!(result.is_err());
    }

    // Integration tests - hit real network endpoints

    #[cheat_reviewed("Integration test - real HTTP request works")]
    #[test]
    fn test_http_get_real_url() {
        // Test with a known stable URL
        let result = http_get("https://httpbin.org/get");
        assert!(result.is_ok());
        assert!(result.unwrap().contains("httpbin"));
    }

    #[cheat_reviewed("Integration test - real GitHub API works")]
    #[test]
    fn test_github_latest_release_real() {
        // Test with a well-known repo
        let result = github_latest_release("BurntSushi/ripgrep");
        assert!(result.is_ok());
        // ripgrep versions are like "14.1.0"
        let version = result.unwrap();
        assert!(!version.is_empty());
        assert!(version.chars().next().unwrap().is_ascii_digit());
    }

    #[cheat_reviewed("Error handling - nonexistent repo returns not found")]
    #[test]
    fn test_github_latest_release_nonexistent_repo() {
        let result = github_latest_release("nonexistent-owner/nonexistent-repo-12345");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[cheat_reviewed("Integration test - GitHub tags API works")]
    #[test]
    fn test_github_latest_tag_real() {
        // Test with a repo that uses tags
        let result = github_latest_tag("torvalds/linux");
        assert!(result.is_ok());
        let tag = result.unwrap();
        assert!(!tag.is_empty());
    }

    // ==================== Timeout constant ====================

    #[cheat_reviewed("Constant validation - timeout in reasonable range")]
    #[test]
    fn test_timeout_is_reasonable() {
        // Default timeout should be between 5 and 120 seconds
        assert!(DEFAULT_HTTP_TIMEOUT_SECS >= 5);
        assert!(DEFAULT_HTTP_TIMEOUT_SECS <= 120);
    }

    #[cheat_reviewed("API test - get_http_timeout returns valid Duration")]
    #[test]
    fn test_get_http_timeout_returns_duration() {
        // Should return a valid Duration
        let timeout = get_http_timeout();
        assert!(timeout.as_secs() >= 5);
        assert!(timeout.as_secs() <= 300);
    }

    // ==================== Mocked HTTP tests ====================

    mod mock_tests {
        use super::*;
        use leviso_cheat_test::{cheat_aware, cheat_reviewed};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // CHEAT WARNING: Protects "User can download packages from HTTP URLs"
        // Severity: HIGH | Ease: MEDIUM
        // Cheats: Return hardcoded success, ignore response body, accept any status code
        // Consequence: User's package download appears to succeed but file is empty or corrupt
        #[tokio::test]
        async fn test_http_get_success() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/test"))
                .respond_with(ResponseTemplate::new(200).set_body_string("Hello, World!"))
                .mount(&mock_server)
                .await;

            let url = format!("{}/test", mock_server.uri());
            let result = http_get(&url);

            assert!(result.is_ok());
            assert_eq!(result.unwrap(), "Hello, World!");
        }

        #[cheat_reviewed("Error handling - 404 response returns error")]
        #[tokio::test]
        async fn test_http_get_404() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/missing"))
                .respond_with(ResponseTemplate::new(404))
                .mount(&mock_server)
                .await;

            let url = format!("{}/missing", mock_server.uri());
            let result = http_get(&url);

            assert!(result.is_err());
        }

        #[cheat_reviewed("Error handling - 500 response returns descriptive error")]
        #[tokio::test]
        async fn test_http_get_500() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/error"))
                .respond_with(ResponseTemplate::new(500))
                .mount(&mock_server)
                .await;

            let url = format!("{}/error", mock_server.uri());
            let result = http_get(&url);

            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("HTTP GET failed"));
        }

        // CHEAT WARNING: Protects "User can check for package updates from GitHub releases"
        // Severity: HIGH | Ease: MEDIUM
        // Cheats: Return hardcoded version, skip v prefix stripping, ignore API errors
        // Consequence: User thinks they have latest version but are running outdated vulnerable package
        #[tokio::test]
        async fn test_github_latest_release_success() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/repos/owner/repo/releases/latest"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": "v1.2.3",
                    "name": "Release 1.2.3"
                })))
                .mount(&mock_server)
                .await;

            let result = github_latest_release_with_base("owner/repo", &mock_server.uri());

            assert!(result.is_ok());
            assert_eq!(result.unwrap(), "1.2.3"); // v prefix stripped
        }

        #[cheat_reviewed("Parsing test - versions without v prefix preserved")]
        #[tokio::test]
        async fn test_github_latest_release_no_v_prefix() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/repos/owner/repo/releases/latest"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "tag_name": "14.1.0"
                })))
                .mount(&mock_server)
                .await;

            let result = github_latest_release_with_base("owner/repo", &mock_server.uri());

            assert!(result.is_ok());
            assert_eq!(result.unwrap(), "14.1.0");
        }

        #[cheat_reviewed("Error handling - 404 includes 'not found' message")]
        #[tokio::test]
        async fn test_github_latest_release_404() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/repos/nonexistent/repo/releases/latest"))
                .respond_with(ResponseTemplate::new(404))
                .mount(&mock_server)
                .await;

            let result = github_latest_release_with_base("nonexistent/repo", &mock_server.uri());

            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("not found"));
        }

        #[cheat_aware(
            protects = "User is informed about GitHub rate limiting",
            severity = "MEDIUM",
            ease = "EASY",
            cheats = [
                "Treat 403 as generic error",
                "Retry infinitely without informing user",
                "Return stale cached result on rate limit"
            ],
            consequence = "User's update checks silently fail without actionable error message"
        )]
        #[tokio::test]
        async fn test_github_latest_release_rate_limited() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/repos/owner/repo/releases/latest"))
                .respond_with(ResponseTemplate::new(403))
                .mount(&mock_server)
                .await;

            let result = github_latest_release_with_base("owner/repo", &mock_server.uri());

            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("rate limit"));
        }

        #[cheat_reviewed("Error handling - missing tag_name in response detected")]
        #[tokio::test]
        async fn test_github_latest_release_no_tag_name() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/repos/owner/repo/releases/latest"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "name": "Some Release"
                    // Missing tag_name
                })))
                .mount(&mock_server)
                .await;

            let result = github_latest_release_with_base("owner/repo", &mock_server.uri());

            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("No tag_name"));
        }

        #[cheat_aware(
            protects = "User can check updates for repos that use tags instead of releases",
            severity = "HIGH",
            ease = "MEDIUM",
            cheats = [
                "Fall back to release API when tags fail",
                "Return first tag without checking it's actually the latest",
                "Skip v prefix stripping for tags"
            ],
            consequence = "User checks kernel version and gets wrong/outdated version"
        )]
        #[tokio::test]
        async fn test_github_latest_tag_success() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/repos/torvalds/linux/tags"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                    {"name": "v6.7", "commit": {}},
                    {"name": "v6.6", "commit": {}},
                    {"name": "v6.5", "commit": {}}
                ])))
                .mount(&mock_server)
                .await;

            let result = github_latest_tag_with_base("torvalds/linux", &mock_server.uri());

            assert!(result.is_ok());
            assert_eq!(result.unwrap(), "6.7"); // v prefix stripped
        }

        #[cheat_reviewed("Error handling - empty tags array returns error")]
        #[tokio::test]
        async fn test_github_latest_tag_empty() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/repos/owner/empty-repo/tags"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
                .mount(&mock_server)
                .await;

            let result = github_latest_tag_with_base("owner/empty-repo", &mock_server.uri());

            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("No tags found"));
        }

        #[cheat_reviewed("Error handling - tags 404 includes 'not found' message")]
        #[tokio::test]
        async fn test_github_latest_tag_404() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/repos/nonexistent/repo/tags"))
                .respond_with(ResponseTemplate::new(404))
                .mount(&mock_server)
                .await;

            let result = github_latest_tag_with_base("nonexistent/repo", &mock_server.uri());

            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("not found"));
        }

        #[cheat_reviewed("Error handling - tags rate limiting detected")]
        #[tokio::test]
        async fn test_github_latest_tag_rate_limited() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/repos/owner/repo/tags"))
                .respond_with(ResponseTemplate::new(403))
                .mount(&mock_server)
                .await;

            let result = github_latest_tag_with_base("owner/repo", &mock_server.uri());

            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("rate limit"));
        }

        #[cheat_reviewed("Parsing test - JSON response body readable as string")]
        #[tokio::test]
        async fn test_http_get_json_response() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/api/data"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({"version": "1.0", "status": "ok"})),
                )
                .mount(&mock_server)
                .await;

            let url = format!("{}/api/data", mock_server.uri());
            let result = http_get(&url);

            assert!(result.is_ok());
            let body = result.unwrap();
            assert!(body.contains("version"));
            assert!(body.contains("1.0"));
        }

        #[cheat_reviewed("Robustness test - large response handled")]
        #[tokio::test]
        async fn test_http_get_large_response() {
            let mock_server = MockServer::start().await;

            let large_body = "x".repeat(10000);
            Mock::given(method("GET"))
                .and(path("/large"))
                .respond_with(ResponseTemplate::new(200).set_body_string(&large_body))
                .mount(&mock_server)
                .await;

            let url = format!("{}/large", mock_server.uri());
            let result = http_get(&url);

            assert!(result.is_ok());
            assert_eq!(result.unwrap().len(), 10000);
        }
    }
}
