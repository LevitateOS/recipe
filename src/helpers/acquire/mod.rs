//! Verified acquisition of explicit HTTP payloads and pinned Git sources.

pub mod download;
pub mod git;
pub mod http;
pub(crate) mod net;
pub mod verify;

pub use download::download;
pub use git::git_checkout;
pub use http::{
    fetch_sha256, github_download_release, github_latest_release, github_latest_tag, http_get,
    parse_version,
};
pub use verify::{verify_blake3, verify_sha256, verify_sha512};

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
/// explicit timeouts. Network authority comes from the configured domain in
/// a Ginux run, and from the pinned grant [`net`] installs in a host run;
/// never from an inherited proxy environment or a host-side helper.
fn http_request(url: &str, timeout: Duration) -> Result<Fetch, Box<EvalAltResult>> {
    validate_http_url(url)?;
    let (builder, record) = net::agent(
        ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .https_only(url.starts_with("https://"))
            .redirects(5)
            .timeout_connect(Duration::from_secs(30))
            .timeout_read(timeout)
            .timeout_write(timeout)
            .timeout(timeout),
    );
    Ok(Fetch {
        request: builder.build().get(url),
        record,
    })
}

/// One prepared request and the endpoints it reaches ([`net`]).
pub(crate) struct Fetch {
    request: ureq::Request,
    record: Option<net::HopRecord>,
}

impl Fetch {
    pub(crate) fn set(mut self, header: &str, value: &str) -> Self {
        self.request = self.request.set(header, value);
        self
    }
    #[allow(clippy::result_large_err)]
    pub(crate) fn call(self) -> Result<ureq::Response, ureq::Error> {
        net::call(self.request, self.record.as_ref())
    }
}
