//! Torrent/download helpers for recipe scripts
//!
//! Provides functions to download files via BitTorrent or HTTP with resume support.
//! Uses aria2c as the backend (supports torrents, metalinks, and HTTP resume).
//!
//! ## Example
//!
//! ```rhai
//! fn resolve() {
//!     // Download via torrent (faster for large files)
//!     torrent("https://download.rockylinux.org/pub/rocky/9/isos/x86_64/Rocky-9.5-x86_64-dvd.iso.torrent");
//!     return BUILD_DIR + "/Rocky-9.5-x86_64-dvd.iso";
//! }
//! ```

use crate::core::{output, with_context};
use indicatif::{ProgressBar, ProgressStyle};
use rhai::EvalAltResult;
use std::process::Command;
use std::time::Duration;

/// RAII guard for progress bars - ensures cleanup on any exit path
struct ProgressGuard(ProgressBar);

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        self.0.finish_and_clear();
    }
}

/// Validate that a URL uses an allowed scheme for torrent/download operations.
/// Only http://, https://, and magnet: URLs are supported.
fn validate_download_url(url: &str) -> Result<(), Box<EvalAltResult>> {
    if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("magnet:") {
        Ok(())
    } else {
        Err(format!(
            "Unsupported URL scheme: {}\n\
             Only http://, https://, and magnet: URLs are supported",
            url
        )
        .into())
    }
}

/// Check if aria2c is available
fn has_aria2c() -> bool {
    Command::new("aria2c")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Download a file via BitTorrent
///
/// Takes a .torrent file URL or magnet link. Downloads the content to BUILD_DIR.
/// Returns the path to the downloaded file.
///
/// Requires `aria2c` to be installed on the system.
///
/// # Example
/// ```rhai
/// torrent("https://example.com/file.torrent");
/// ```
pub fn torrent(url: &str) -> Result<String, Box<EvalAltResult>> {
    with_context(|ctx| {
        // Validate URL scheme for security
        validate_download_url(url)?;

        if !has_aria2c() {
            return Err(
                "aria2c not found. Install it with: dnf install aria2 (or apt install aria2)"
                    .into(),
            );
        }

        let build_dir = &ctx.build_dir;

        // Get build directory path as string, handling non-UTF8 gracefully
        let build_dir_str = build_dir
            .to_str()
            .ok_or("build directory path contains invalid UTF-8")?;

        output::detail(&format!("torrent download: {}", url));

        // Create progress spinner with RAII guard for cleanup
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("     {spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
        );
        pb.set_message("downloading via torrent...");
        pb.enable_steady_tick(Duration::from_millis(80));
        let _guard = ProgressGuard(pb);

        // Use aria2c for torrent download with stderr capture
        // --seed-time=0 means don't seed after download (we're not being a good peer, but this is a build tool)
        // --continue=true enables resume
        // --max-connection-per-server=16 improves HTTP download speed
        let output = Command::new("aria2c")
            .args([
                "--dir",
                build_dir_str,
                "--continue=true",
                "--seed-time=0",
                "--max-connection-per-server=16",
                "--summary-interval=0",
                "--console-log-level=warn",
                url,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
            .map_err(|e| format!("failed to run aria2c: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(
                format!("torrent download failed for {}\nDetails: {}", url, stderr.trim()).into(),
            );
        }

        // Try to determine the downloaded filename
        // For .torrent URLs, extract the base name
        let filename = extract_filename_from_torrent_url(url);
        let path = build_dir.join(&filename);

        output::detail(&format!("downloaded {}", filename));
        Ok(path.to_string_lossy().to_string())
    })
}

/// Download a file via HTTP with resume support
///
/// Uses aria2c for better performance and resume capability.
/// Falls back to curl if aria2c is not available.
///
/// # Example
/// ```rhai
/// download_with_resume("https://example.com/large-file.iso");
/// ```
pub fn download_with_resume(url: &str) -> Result<String, Box<EvalAltResult>> {
    with_context(|ctx| {
        // Validate URL scheme for security
        validate_download_url(url)?;

        let filename = extract_filename_from_url(url);
        let dest = ctx.build_dir.join(&filename);

        // Get paths as strings, handling non-UTF8 gracefully
        let build_dir_str = ctx
            .build_dir
            .to_str()
            .ok_or("build directory path contains invalid UTF-8")?;
        let dest_str = dest
            .to_str()
            .ok_or("destination path contains invalid UTF-8")?;

        // If file already exists, verify size or skip
        if dest.exists() {
            output::detail(&format!("{} exists, will resume if incomplete", filename));
        }

        output::detail(&format!("downloading with resume: {}", url));

        // Create progress spinner with RAII guard for cleanup
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("     {spinner:.cyan} {msg}")
                .unwrap()
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
        );
        pb.set_message(format!("downloading {}", filename));
        pb.enable_steady_tick(Duration::from_millis(80));
        let _guard = ProgressGuard(pb);

        let result = if has_aria2c() {
            // Prefer aria2c with stderr capture
            Command::new("aria2c")
                .args([
                    "--dir",
                    build_dir_str,
                    "--out",
                    &filename,
                    "--continue=true",
                    "--max-connection-per-server=16",
                    "--summary-interval=0",
                    "--console-log-level=warn",
                    url,
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output()
        } else {
            // Fall back to curl with resume
            output::detail("aria2c not found, using curl");
            Command::new("curl")
                .args([
                    "-L", // Follow redirects
                    "-C",
                    "-", // Resume from where we left off
                    "-o",
                    dest_str,
                    url,
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output()
        };

        match result {
            Ok(output) if output.status.success() => {
                output::detail(&format!("downloaded {}", dest.display()));
                Ok(dest.to_string_lossy().to_string())
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(format!("download failed for {}\nDetails: {}", url, stderr.trim()).into())
            }
            Err(e) => Err(format!("failed to run downloader: {}", e).into()),
        }
    })
}

/// Extract filename from a URL, handling query strings and sanitizing
fn extract_filename_from_url(url: &str) -> String {
    // Strip query parameters and fragments
    let url_without_query = url.split('?').next().unwrap_or(url);
    let url_without_fragment = url_without_query.split('#').next().unwrap_or(url_without_query);

    let filename = url_without_fragment.rsplit('/').next().unwrap_or("download");

    // Strip .torrent extension if present
    let name = filename.strip_suffix(".torrent").unwrap_or(filename);

    // Sanitize: ensure non-empty and not a path traversal
    if name.is_empty() || name == "." || name == ".." {
        return "download".to_string();
    }

    name.to_string()
}

/// Extract the expected filename from a torrent URL
///
/// For URLs like "Rocky-9.5-x86_64-dvd.iso.torrent", returns "Rocky-9.5-x86_64-dvd.iso"
/// For magnet links, generates a unique name based on timestamp
fn extract_filename_from_torrent_url(url: &str) -> String {
    // Handle magnet links specially
    if url.starts_with("magnet:") {
        // Try to extract display name from magnet link (dn= parameter)
        if let Some(dn_start) = url.find("dn=") {
            let dn_value = &url[dn_start + 3..];
            let dn_end = dn_value.find('&').unwrap_or(dn_value.len());
            let name = &dn_value[..dn_end];
            // URL decode basic escapes
            let name = name.replace("%20", " ").replace("+", " ");
            if !name.is_empty() && name != "." && name != ".." {
                return name;
            }
        }
        // Fall back to timestamp-based name for magnet links
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        return format!("magnet_{}", timestamp);
    }

    // Strip query parameters and fragments first
    let url_without_query = url.split('?').next().unwrap_or(url);
    let url_without_fragment = url_without_query.split('#').next().unwrap_or(url_without_query);

    let filename = url_without_fragment.rsplit('/').next().unwrap_or("download");

    // Strip .torrent extension if present
    let name = filename.strip_suffix(".torrent").unwrap_or(filename);

    // Sanitize: ensure non-empty and not a path traversal
    if name.is_empty() || name == "." || name == ".." {
        return "download".to_string();
    }

    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviso_cheat_test::cheat_reviewed;

    // ==================== URL Validation Tests ====================

    #[cheat_reviewed("Security - HTTPS URLs accepted")]
    #[test]
    fn test_validate_download_url_https() {
        assert!(validate_download_url("https://example.com/file.iso").is_ok());
    }

    #[cheat_reviewed("Security - HTTP URLs accepted")]
    #[test]
    fn test_validate_download_url_http() {
        assert!(validate_download_url("http://example.com/file.iso").is_ok());
    }

    #[cheat_reviewed("Security - magnet URLs accepted")]
    #[test]
    fn test_validate_download_url_magnet() {
        assert!(validate_download_url("magnet:?xt=urn:btih:abc123").is_ok());
    }

    #[cheat_reviewed("Security - file:// URLs rejected")]
    #[test]
    fn test_validate_download_url_file_rejected() {
        let result = validate_download_url("file:///etc/passwd");
        assert!(result.is_err());
    }

    #[cheat_reviewed("Security - ftp:// URLs rejected")]
    #[test]
    fn test_validate_download_url_ftp_rejected() {
        let result = validate_download_url("ftp://example.com/file.iso");
        assert!(result.is_err());
    }

    #[cheat_reviewed("Security - bare paths rejected")]
    #[test]
    fn test_validate_download_url_bare_path_rejected() {
        let result = validate_download_url("/local/path/to/file");
        assert!(result.is_err());
    }

    // ==================== Filename Extraction Tests ====================

    #[cheat_reviewed("URL parsing test - torrent URL extracts base filename")]
    #[test]
    fn test_extract_filename_from_torrent_url() {
        let name =
            extract_filename_from_torrent_url("https://example.com/Rocky-9.5-x86_64-dvd.iso.torrent");
        assert_eq!(name, "Rocky-9.5-x86_64-dvd.iso");
    }

    #[cheat_reviewed("URL parsing test - non-torrent URL preserved")]
    #[test]
    fn test_extract_filename_regular_url() {
        let name = extract_filename_from_torrent_url("https://example.com/file.iso");
        assert_eq!(name, "file.iso");
    }

    #[cheat_reviewed("URL parsing test - simple URL")]
    #[test]
    fn test_extract_filename_simple() {
        let name = extract_filename_from_torrent_url("https://example.com/archive.tar.gz");
        assert_eq!(name, "archive.tar.gz");
    }

    #[cheat_reviewed("URL parsing test - query string stripped")]
    #[test]
    fn test_extract_filename_with_query() {
        let name = extract_filename_from_torrent_url(
            "https://example.com/file.iso.torrent?token=abc&sig=xyz",
        );
        assert_eq!(name, "file.iso");
    }

    #[cheat_reviewed("URL parsing test - fragment stripped")]
    #[test]
    fn test_extract_filename_with_fragment() {
        let name = extract_filename_from_torrent_url("https://example.com/file.iso#section");
        assert_eq!(name, "file.iso");
    }

    #[cheat_reviewed("URL parsing test - magnet link generates unique name")]
    #[test]
    fn test_extract_filename_magnet() {
        let name = extract_filename_from_torrent_url("magnet:?xt=urn:btih:abc123");
        assert!(name.starts_with("magnet_"));
    }

    #[cheat_reviewed("URL parsing test - magnet link with display name")]
    #[test]
    fn test_extract_filename_magnet_with_dn() {
        let name = extract_filename_from_torrent_url(
            "magnet:?xt=urn:btih:abc123&dn=Ubuntu.22.04.iso&tr=udp://tracker",
        );
        assert_eq!(name, "Ubuntu.22.04.iso");
    }

    #[cheat_reviewed("URL parsing test - empty filename sanitized")]
    #[test]
    fn test_extract_filename_empty_sanitized() {
        let name = extract_filename_from_torrent_url("https://example.com/");
        assert_eq!(name, "download");
    }

    #[cheat_reviewed("URL parsing test - dot filename sanitized")]
    #[test]
    fn test_extract_filename_dot_sanitized() {
        // A URL like "https://example.com/." should not result in "."
        let name = extract_filename_from_torrent_url("https://example.com/.");
        assert_eq!(name, "download");
    }

    #[cheat_reviewed("URL parsing test - double dot filename sanitized")]
    #[test]
    fn test_extract_filename_dotdot_sanitized() {
        // A URL like "https://example.com/.." should not result in ".."
        let name = extract_filename_from_torrent_url("https://example.com/..");
        assert_eq!(name, "download");
    }

    // ==================== extract_filename_from_url Tests ====================

    #[cheat_reviewed("URL filename extraction - basic case")]
    #[test]
    fn test_extract_filename_from_url_basic() {
        let name = extract_filename_from_url("https://example.com/file.iso");
        assert_eq!(name, "file.iso");
    }

    #[cheat_reviewed("URL filename extraction - query string stripped")]
    #[test]
    fn test_extract_filename_from_url_query() {
        let name = extract_filename_from_url("https://example.com/file.iso?token=abc");
        assert_eq!(name, "file.iso");
    }

    #[cheat_reviewed("URL filename extraction - fragment stripped")]
    #[test]
    fn test_extract_filename_from_url_fragment() {
        let name = extract_filename_from_url("https://example.com/file.iso#section");
        assert_eq!(name, "file.iso");
    }
}
