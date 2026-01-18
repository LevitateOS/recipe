//! HTTP utilities for recipe scripts
//!
//! Provides helpers for checking updates and fetching remote content.

use rhai::EvalAltResult;
use std::time::Duration;

/// Default HTTP timeout in seconds
const HTTP_TIMEOUT_SECS: u64 = 30;

/// Default GitHub API base URL
const GITHUB_API_BASE: &str = "https://api.github.com";

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
    github_latest_release_with_base(repo, GITHUB_API_BASE)
}

/// Internal: Get latest release with configurable base URL (for testing)
fn github_latest_release_with_base(repo: &str, base_url: &str) -> Result<String, Box<EvalAltResult>> {
    let url = format!("{}/repos/{}/releases/latest", base_url, repo);

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
    github_latest_tag_with_base(repo, GITHUB_API_BASE)
}

/// Internal: Get latest tag with configurable base URL (for testing)
fn github_latest_tag_with_base(repo: &str, base_url: &str) -> Result<String, Box<EvalAltResult>> {
    let url = format!("{}/repos/{}/tags", base_url, repo);

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

    // Integration tests - hit real network endpoints

    #[test]
    fn test_http_get_real_url() {
        // Test with a known stable URL
        let result = http_get("https://httpbin.org/get");
        assert!(result.is_ok());
        assert!(result.unwrap().contains("httpbin"));
    }

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

    #[test]
    fn test_github_latest_release_nonexistent_repo() {
        let result = github_latest_release("nonexistent-owner/nonexistent-repo-12345");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
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

    // ==================== Mocked HTTP tests ====================

    mod mock_tests {
        use super::*;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

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

        #[tokio::test]
        async fn test_github_latest_release_success() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/repos/owner/repo/releases/latest"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({
                            "tag_name": "v1.2.3",
                            "name": "Release 1.2.3"
                        })),
                )
                .mount(&mock_server)
                .await;

            let result = github_latest_release_with_base("owner/repo", &mock_server.uri());

            assert!(result.is_ok());
            assert_eq!(result.unwrap(), "1.2.3"); // v prefix stripped
        }

        #[tokio::test]
        async fn test_github_latest_release_no_v_prefix() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/repos/owner/repo/releases/latest"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({
                            "tag_name": "14.1.0"
                        })),
                )
                .mount(&mock_server)
                .await;

            let result = github_latest_release_with_base("owner/repo", &mock_server.uri());

            assert!(result.is_ok());
            assert_eq!(result.unwrap(), "14.1.0");
        }

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

        #[tokio::test]
        async fn test_github_latest_release_no_tag_name() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/repos/owner/repo/releases/latest"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({
                            "name": "Some Release"
                            // Missing tag_name
                        })),
                )
                .mount(&mock_server)
                .await;

            let result = github_latest_release_with_base("owner/repo", &mock_server.uri());

            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("No tag_name"));
        }

        #[tokio::test]
        async fn test_github_latest_tag_success() {
            let mock_server = MockServer::start().await;

            Mock::given(method("GET"))
                .and(path("/repos/torvalds/linux/tags"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!([
                            {"name": "v6.7", "commit": {}},
                            {"name": "v6.6", "commit": {}},
                            {"name": "v6.5", "commit": {}}
                        ])),
                )
                .mount(&mock_server)
                .await;

            let result = github_latest_tag_with_base("torvalds/linux", &mock_server.uri());

            assert!(result.is_ok());
            assert_eq!(result.unwrap(), "6.7"); // v prefix stripped
        }

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
