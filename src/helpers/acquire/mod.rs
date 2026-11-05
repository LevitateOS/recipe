//! Verified acquisition of explicit HTTP payloads and pinned Git sources.

pub mod download;
pub mod git;
pub mod http;
pub mod verify;

pub use download::download;
pub use git::git_checkout;
pub use http::{
    github_download_release, github_latest_release, github_latest_tag, http_get, parse_version,
};
pub use verify::{
    FileHashes, compute_hashes, fetch_sha256, verify_blake3, verify_sha256, verify_sha512,
};

use rhai::EvalAltResult;
use std::time::Duration;

fn validate_http_url(url: &str) -> Result<(), Box<EvalAltResult>> {
    if !(url.starts_with("https://") || url.starts_with("http://"))
        || url.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err("acquisition requires an http:// or https:// URL".into());
    }
    Ok(())
}

/// All acquisition HTTP clients share bounded redirects, verified TLS and
/// explicit timeouts. Network authority comes from the configured domain,
/// never an inherited proxy environment or a host-side helper.
fn http_request(url: &str, timeout: Duration) -> Result<ureq::Request, Box<EvalAltResult>> {
    validate_http_url(url)?;
    Ok(ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .https_only(url.starts_with("https://"))
        .redirects(5)
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(timeout)
        .timeout_write(timeout)
        .timeout(timeout)
        .build()
        .get(url))
}
