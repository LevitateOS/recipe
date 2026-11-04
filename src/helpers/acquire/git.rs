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

/// Git clone timeout (10 minutes). Prevents hangs on network issues.
const GIT_CLONE_TIMEOUT: Duration = Duration::from_secs(600);

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

    // Run git clone with stderr capture and network timeout
    let timeout_secs = GIT_CLONE_TIMEOUT.as_secs().to_string();
    let output = Command::new("git")
        .args([
            "-c",
            "http.lowSpeedLimit=1000",
            "-c",
            &format!("http.lowSpeedTime={}", timeout_secs),
            "clone",
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

    // Run git clone with stderr capture and network timeout
    let timeout_secs = GIT_CLONE_TIMEOUT.as_secs().to_string();
    let output = Command::new("git")
        .args([
            "-c",
            "http.lowSpeedLimit=1000",
            "-c",
            &format!("http.lowSpeedTime={}", timeout_secs),
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
