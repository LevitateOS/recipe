//! Git helpers for recipe scripts
//!
//! Pure functions for cloning git repositories.
//! All functions take explicit inputs and return explicit outputs.
//!
//! ## Example
//!
//! ```rhai
//! fn acquire(ctx) {
//!     let repo_dir = git_clone("https://github.com/user/repo.git", BUILD_DIR);
//!     ctx.src_dir = repo_dir;
//!     ctx
//! }
//! ```

use crate::core::output;
use indicatif::{ProgressBar, ProgressStyle};
use rhai::EvalAltResult;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// RAII guard for progress bars - ensures cleanup on any exit path
struct ProgressGuard(ProgressBar);

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        self.0.finish_and_clear();
    }
}

/// Validate that a URL uses an allowed scheme for git operations.
/// Only https://, http://, and git@ (SSH) URLs are supported.
fn validate_git_url(url: &str) -> Result<(), Box<EvalAltResult>> {
    if url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("git@")
        || url.starts_with("ssh://")
    {
        Ok(())
    } else {
        Err(format!(
            "Unsupported git URL scheme: {}\n\
             Only https://, http://, ssh://, and git@ URLs are supported",
            url
        )
        .into())
    }
}

/// Clone a git repository to a specified directory.
///
/// The repository is cloned into dest_dir/{repo-name}. The repo name is
/// extracted from the URL (e.g., "linux" from "https://github.com/torvalds/linux.git").
///
/// Returns the path to the cloned repository.
///
/// # Example
/// ```rhai
/// let repo = git_clone("https://github.com/torvalds/linux.git", BUILD_DIR);
/// // Results in BUILD_DIR/linux/
/// ```
pub fn git_clone(url: &str, dest_dir: &str) -> Result<String, Box<EvalAltResult>> {
    // Validate URL scheme for security
    validate_git_url(url)?;

    let repo_name = extract_repo_name(url)?;
    let dest = Path::new(dest_dir).join(&repo_name);

    // Skip if already cloned AND valid
    if dest.join(".git").exists() {
        // Verify repo is valid before skipping
        let verify = Command::new("git")
            .args(["-C", &dest.to_string_lossy(), "rev-parse", "HEAD"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if verify.map(|s| s.success()).unwrap_or(false) {
            output::detail(&format!("git: {} already cloned", repo_name));
            return Ok(dest.to_string_lossy().to_string());
        }
        // If invalid, warn and re-clone
        output::warning(&format!(
            "git: {} exists but is invalid, re-cloning",
            repo_name
        ));
        // Remove the invalid repo
        let _ = std::fs::remove_dir_all(&dest);
    }

    output::detail(&format!("git clone {}", url));

    // Create progress spinner with RAII guard for cleanup
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("     {spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.set_message(format!("cloning {}", repo_name));
    pb.enable_steady_tick(Duration::from_millis(80));
    let _guard = ProgressGuard(pb);

    // Get destination path as string, handling non-UTF8 gracefully
    let dest_str = dest
        .to_str()
        .ok_or("destination path contains invalid UTF-8")?;

    // Run git clone with stderr capture for better error messages
    let output = Command::new("git")
        .args(["clone", "--progress", url, dest_str])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git clone failed for {}\nDetails: {}", url, stderr.trim()).into());
    }

    output::detail(&format!("cloned {} to {}", repo_name, dest.display()));
    Ok(dest.to_string_lossy().to_string())
}

/// Clone a git repository with a shallow depth.
///
/// Faster than full clone for large repositories. The `depth` parameter
/// specifies how many commits to fetch (1 = only latest).
///
/// Returns the path to the cloned repository.
///
/// # Example
/// ```rhai
/// let repo = git_clone_depth("https://github.com/torvalds/linux.git", BUILD_DIR, 1);
/// ```
pub fn git_clone_depth(
    url: &str,
    dest_dir: &str,
    depth: i64,
) -> Result<String, Box<EvalAltResult>> {
    // Validate depth parameter
    if depth <= 0 || depth > 1_000_000 {
        return Err("depth must be between 1 and 1000000".into());
    }

    // Validate URL scheme for security
    validate_git_url(url)?;

    let repo_name = extract_repo_name(url)?;
    let dest = Path::new(dest_dir).join(&repo_name);

    // Skip if already cloned AND valid
    if dest.join(".git").exists() {
        // Verify repo is valid before skipping
        let verify = Command::new("git")
            .args(["-C", &dest.to_string_lossy(), "rev-parse", "HEAD"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if verify.map(|s| s.success()).unwrap_or(false) {
            output::detail(&format!("git: {} already cloned", repo_name));
            return Ok(dest.to_string_lossy().to_string());
        }
        // If invalid, warn and re-clone
        output::warning(&format!(
            "git: {} exists but is invalid, re-cloning",
            repo_name
        ));
        // Remove the invalid repo
        let _ = std::fs::remove_dir_all(&dest);
    }

    output::detail(&format!("git clone --depth {} {}", depth, url));

    // Create progress spinner with RAII guard for cleanup
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("     {spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.set_message(format!("shallow cloning {}", repo_name));
    pb.enable_steady_tick(Duration::from_millis(80));
    let _guard = ProgressGuard(pb);

    // Get destination path as string, handling non-UTF8 gracefully
    let dest_str = dest
        .to_str()
        .ok_or("destination path contains invalid UTF-8")?;

    // Run git clone with stderr capture for better error messages
    let output = Command::new("git")
        .args([
            "clone",
            "--depth",
            &depth.to_string(),
            "--progress",
            url,
            dest_str,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git clone failed for {}\nDetails: {}", url, stderr.trim()).into());
    }

    output::detail(&format!(
        "shallow cloned {} to {} (depth={})",
        repo_name,
        dest.display(),
        depth
    ));
    Ok(dest.to_string_lossy().to_string())
}

/// Extract repository name from a git URL
fn extract_repo_name(url: &str) -> Result<String, Box<EvalAltResult>> {
    // Strip fragments and query strings first
    // https://github.com/user/repo.git#branch -> https://github.com/user/repo.git
    // https://github.com/user/repo.git?token=xxx -> https://github.com/user/repo.git
    let url = url.split('#').next().unwrap_or(url);
    let url = url.split('?').next().unwrap_or(url);

    // Handle both HTTPS and SSH URLs:
    // https://github.com/user/repo.git -> repo
    // git@github.com:user/repo.git -> repo
    // https://github.com/user/repo -> repo
    let url = url.trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);

    // Need at least a path segment after the host
    // https://example.com -> invalid (no path)
    // https://example.com/repo -> valid
    let path_part = if let Some(after_scheme) = url.strip_prefix("https://") {
        after_scheme
    } else if let Some(after_scheme) = url.strip_prefix("http://") {
        after_scheme
    } else if let Some(after_scheme) = url.strip_prefix("ssh://") {
        after_scheme
    } else if let Some(after_colon) = url.strip_prefix("git@") {
        // SSH URLs like git@github.com:user/repo
        after_colon
    } else {
        url
    };

    // Count path segments after the host
    // "github.com/user/repo" -> ["github.com", "user", "repo"]
    // "github.com" -> ["github.com"]
    let segments: Vec<&str> = path_part.split('/').collect();
    if segments.len() < 2 {
        return Err("URL has no repository path (just a domain)".into());
    }

    url.rsplit('/')
        .next()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "cannot extract repository name from URL".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use leviso_cheat_test::cheat_reviewed;

    // ==================== URL Validation Tests ====================

    #[cheat_reviewed("Security - HTTPS URLs accepted")]
    #[test]
    fn test_validate_git_url_https() {
        assert!(validate_git_url("https://github.com/user/repo.git").is_ok());
    }

    #[cheat_reviewed("Security - HTTP URLs accepted")]
    #[test]
    fn test_validate_git_url_http() {
        assert!(validate_git_url("http://github.com/user/repo.git").is_ok());
    }

    #[cheat_reviewed("Security - SSH URLs with git@ accepted")]
    #[test]
    fn test_validate_git_url_ssh_git_at() {
        assert!(validate_git_url("git@github.com:user/repo.git").is_ok());
    }

    #[cheat_reviewed("Security - SSH URLs with ssh:// accepted")]
    #[test]
    fn test_validate_git_url_ssh_scheme() {
        assert!(validate_git_url("ssh://git@github.com/user/repo.git").is_ok());
    }

    #[cheat_reviewed("Security - file:// URLs rejected")]
    #[test]
    fn test_validate_git_url_file_rejected() {
        let result = validate_git_url("file:///etc/passwd");
        assert!(result.is_err());
    }

    #[cheat_reviewed("Security - ftp:// URLs rejected")]
    #[test]
    fn test_validate_git_url_ftp_rejected() {
        let result = validate_git_url("ftp://example.com/repo.git");
        assert!(result.is_err());
    }

    #[cheat_reviewed("Security - bare paths rejected")]
    #[test]
    fn test_validate_git_url_bare_path_rejected() {
        let result = validate_git_url("/local/path/to/repo");
        assert!(result.is_err());
    }

    // ==================== Repo Name Extraction Tests ====================

    #[cheat_reviewed("URL parsing test - HTTPS URL with .git")]
    #[test]
    fn test_extract_repo_name_https_with_git() {
        let name = extract_repo_name("https://github.com/torvalds/linux.git").unwrap();
        assert_eq!(name, "linux");
    }

    #[cheat_reviewed("URL parsing test - HTTPS URL without .git")]
    #[test]
    fn test_extract_repo_name_https_no_git() {
        let name = extract_repo_name("https://github.com/torvalds/linux").unwrap();
        assert_eq!(name, "linux");
    }

    #[cheat_reviewed("URL parsing test - URL with trailing slash")]
    #[test]
    fn test_extract_repo_name_trailing_slash() {
        let name = extract_repo_name("https://github.com/torvalds/linux/").unwrap();
        assert_eq!(name, "linux");
    }

    #[cheat_reviewed("URL parsing test - short URL")]
    #[test]
    fn test_extract_repo_name_short() {
        let name = extract_repo_name("https://example.com/repo.git").unwrap();
        assert_eq!(name, "repo");
    }

    #[cheat_reviewed("Error handling - empty URL rejected")]
    #[test]
    fn test_extract_repo_name_empty() {
        let result = extract_repo_name("");
        assert!(result.is_err());
    }

    #[cheat_reviewed("Error handling - URL ending in slash only rejected")]
    #[test]
    fn test_extract_repo_name_just_slash() {
        let result = extract_repo_name("https://example.com/");
        assert!(result.is_err());
    }

    #[cheat_reviewed("URL parsing test - SSH URL extracts repo name")]
    #[test]
    fn test_extract_repo_name_ssh() {
        let name = extract_repo_name("git@github.com:user/repo.git").unwrap();
        assert_eq!(name, "repo");
    }

    #[cheat_reviewed("URL parsing test - URL with fragment stripped")]
    #[test]
    fn test_extract_repo_name_with_fragment() {
        let name = extract_repo_name("https://github.com/user/repo.git#branch").unwrap();
        assert_eq!(name, "repo");
    }

    #[cheat_reviewed("URL parsing test - URL with query string stripped")]
    #[test]
    fn test_extract_repo_name_with_query() {
        let name = extract_repo_name("https://github.com/user/repo.git?token=xxx").unwrap();
        assert_eq!(name, "repo");
    }

    #[cheat_reviewed("URL parsing test - URL with both fragment and query")]
    #[test]
    fn test_extract_repo_name_with_fragment_and_query() {
        let name = extract_repo_name("https://github.com/user/repo?ref=main#L10").unwrap();
        assert_eq!(name, "repo");
    }

    // ==================== Depth Validation Tests ====================

    #[cheat_reviewed("Depth validation - zero rejected")]
    #[test]
    fn test_git_clone_depth_zero_rejected() {
        let result = git_clone_depth("https://github.com/user/repo.git", "/tmp", 0);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("between 1 and"));
    }

    #[cheat_reviewed("Depth validation - negative rejected")]
    #[test]
    fn test_git_clone_depth_negative_rejected() {
        let result = git_clone_depth("https://github.com/user/repo.git", "/tmp", -1);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("between 1 and"));
    }

    #[cheat_reviewed("Depth validation - too large rejected")]
    #[test]
    fn test_git_clone_depth_too_large_rejected() {
        let result = git_clone_depth("https://github.com/user/repo.git", "/tmp", 2_000_000);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("between 1 and"));
    }
}
