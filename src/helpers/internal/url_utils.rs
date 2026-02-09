//! URL parsing and validation utilities
//!
//! Provides helpers for extracting filenames from URLs and validating URL schemes.

use rhai::EvalAltResult;

/// Allowed URL schemes for different operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlScheme {
    Http,
    Https,
    Ssh,
    Git,
    Magnet,
}

impl UrlScheme {
    /// Get the scheme prefix string
    pub fn prefix(&self) -> &'static str {
        match self {
            Self::Http => "http://",
            Self::Https => "https://",
            Self::Ssh => "ssh://",
            Self::Git => "git@",
            Self::Magnet => "magnet:",
        }
    }
}

/// Validate that a URL uses one of the allowed schemes.
pub fn validate_url_scheme(url: &str, allowed: &[UrlScheme]) -> Result<(), Box<EvalAltResult>> {
    let url_lower = url.to_lowercase();

    for scheme in allowed {
        if url_lower.starts_with(scheme.prefix()) {
            return Ok(());
        }
    }

    // Special case: git@ URLs
    if allowed.contains(&UrlScheme::Git) && url.starts_with("git@") {
        return Ok(());
    }

    let allowed_str: Vec<_> = allowed.iter().map(|s| s.prefix()).collect();
    Err(format!("URL must use one of: {:?}\n  got: {}", allowed_str, url).into())
}

/// Extract filename from a URL.
///
/// Handles query strings and fragments, returns "download" as fallback.
///
/// # Example
/// ```ignore
/// assert_eq!(extract_filename("https://example.com/foo-1.0.tar.gz"), "foo-1.0.tar.gz");
/// assert_eq!(extract_filename("https://example.com/file?v=1"), "file");
/// ```
pub fn extract_filename(url: &str) -> String {
    // Handle magnet links specially
    if url.starts_with("magnet:") {
        return extract_filename_from_magnet(url);
    }

    // Strip query string and fragment
    let clean_url = url.split('?').next().unwrap_or(url);
    let clean_url = clean_url.split('#').next().unwrap_or(clean_url);

    // Get last path segment
    let filename = clean_url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(sanitize_filename)
        .unwrap_or_else(|| "download".to_string());

    // If it looks like a domain (no extension, or common TLDs), return "download"
    if filename.contains('.') {
        let ext = filename.rsplit('.').next().unwrap_or("");
        let common_tlds = [
            "com", "org", "net", "io", "dev", "co", "uk", "de", "fr", "ru",
        ];
        if common_tlds.contains(&ext) && !filename.contains('_') && !filename.contains('-') {
            return "download".to_string();
        }
    }

    filename
}

/// Extract filename from a magnet link's display name parameter.
fn extract_filename_from_magnet(url: &str) -> String {
    // Look for dn= parameter
    if let Some(dn_start) = url.find("dn=") {
        let after_dn = &url[dn_start + 3..];
        let end = after_dn.find('&').unwrap_or(after_dn.len());
        let name = &after_dn[..end];

        // URL decode the name
        let decoded = percent_decode(name);
        if !decoded.is_empty() {
            return sanitize_filename(&decoded);
        }
    }

    "download".to_string()
}

/// Simple percent-decoding for URL parameters.
fn percent_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            // Try to read two hex digits
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2
                && let Ok(byte) = u8::from_str_radix(&hex, 16)
            {
                result.push(byte as char);
                continue;
            }
            result.push('%');
            result.push_str(&hex);
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }

    result
}

/// Sanitize a filename for safe filesystem use.
///
/// Replaces problematic characters and handles special names.
pub fn sanitize_filename(name: &str) -> String {
    // Handle empty or special names
    if name.is_empty() || name == "." || name == ".." {
        return "download".to_string();
    }

    // Replace problematic characters
    let sanitized: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();

    // Trim leading/trailing whitespace and dots
    let trimmed = sanitized.trim().trim_matches('.');

    if trimmed.is_empty() {
        "download".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Extract repository name from a git URL.
///
/// # Example
/// ```ignore
/// assert_eq!(extract_repo_name("https://github.com/foo/bar.git"), "bar");
/// assert_eq!(extract_repo_name("git@github.com:foo/bar.git"), "bar");
/// ```
pub fn extract_repo_name(url: &str) -> String {
    // Strip trailing slashes and .git suffix
    let clean = url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .split('#')
        .next()
        .unwrap_or(url)
        .split('?')
        .next()
        .unwrap_or(url);

    // Handle git@host:user/repo format
    if let Some(colon_pos) = clean.rfind(':')
        && !clean[..colon_pos].contains('/')
    {
        // This is git@host:user/repo format
        let after_colon = &clean[colon_pos + 1..];
        return after_colon.rsplit('/').next().unwrap_or("repo").to_string();
    }

    // Handle https:// format
    clean
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("repo")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_filename_simple() {
        assert_eq!(
            extract_filename("https://example.com/foo-1.0.tar.gz"),
            "foo-1.0.tar.gz"
        );
    }

    #[test]
    fn test_extract_filename_with_query() {
        assert_eq!(
            extract_filename("https://example.com/file.tar.gz?token=abc"),
            "file.tar.gz"
        );
    }

    #[test]
    fn test_extract_filename_with_fragment() {
        assert_eq!(
            extract_filename("https://example.com/file.tar.gz#section"),
            "file.tar.gz"
        );
    }

    #[test]
    fn test_extract_filename_fallback() {
        assert_eq!(extract_filename("https://example.com/"), "download");
        assert_eq!(extract_filename("https://example.com"), "download");
    }

    #[test]
    fn test_extract_filename_magnet() {
        let url = "magnet:?xt=urn:btih:abc&dn=My+File.torrent&tr=http://tracker";
        assert_eq!(extract_filename(url), "My File.torrent");
    }

    #[test]
    fn test_sanitize_filename() {
        assert_eq!(sanitize_filename("foo/bar"), "foo_bar");
        assert_eq!(sanitize_filename("file:name"), "file_name");
        assert_eq!(sanitize_filename(".."), "download");
        assert_eq!(sanitize_filename(""), "download");
        assert_eq!(sanitize_filename("  test  "), "test");
    }

    #[test]
    fn test_extract_repo_name_https() {
        assert_eq!(extract_repo_name("https://github.com/foo/bar.git"), "bar");
        assert_eq!(extract_repo_name("https://github.com/foo/bar"), "bar");
    }

    #[test]
    fn test_extract_repo_name_ssh() {
        assert_eq!(extract_repo_name("git@github.com:foo/bar.git"), "bar");
        assert_eq!(extract_repo_name("git@github.com:foo/bar"), "bar");
    }

    #[test]
    fn test_extract_repo_name_trailing_slash() {
        assert_eq!(extract_repo_name("https://github.com/foo/bar/"), "bar");
    }

    #[test]
    fn test_validate_url_scheme_https() {
        let allowed = &[UrlScheme::Http, UrlScheme::Https];
        assert!(validate_url_scheme("https://example.com", allowed).is_ok());
        assert!(validate_url_scheme("http://example.com", allowed).is_ok());
        assert!(validate_url_scheme("ftp://example.com", allowed).is_err());
    }

    #[test]
    fn test_validate_url_scheme_git() {
        let allowed = &[UrlScheme::Https, UrlScheme::Ssh, UrlScheme::Git];
        assert!(validate_url_scheme("https://github.com/foo/bar", allowed).is_ok());
        assert!(validate_url_scheme("git@github.com:foo/bar", allowed).is_ok());
        assert!(validate_url_scheme("ssh://git@github.com/foo/bar", allowed).is_ok());
    }

    #[test]
    fn test_percent_decode() {
        assert_eq!(percent_decode("hello%20world"), "hello world");
        assert_eq!(percent_decode("foo+bar"), "foo bar");
        assert_eq!(percent_decode("100%25"), "100%");
    }
}
