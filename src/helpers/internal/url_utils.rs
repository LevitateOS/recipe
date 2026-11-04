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
/// A URL such as `https://example.com/foo-1.0.tar.gz` yields `foo-1.0.tar.gz`.
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
/// HTTPS and SSH URLs ending in `foo/bar.git` yield `bar`.
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
