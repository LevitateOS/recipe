//! The network rule of a host run: Recipe's HTTP client reaches only the
//! `NEEDS.network` endpoints, at the addresses the coordinator pinned.
//!
//! In a Ginux run the domain enforces the grant and nothing here is installed.
//! In a host run the worker installs [`HostNetwork`] from its request:
//!
//! - every agent gets a resolver that answers only `HOST:PORT` pairs the
//!   recipe declared, with the coordinator's pinned addresses, and refuses
//!   everything else (other hosts, other ports, redirects to either);
//! - response bodies are counted against `network_max_bytes` (bodies only:
//!   request and header bytes are small and not counted);
//! - each declared endpoint records whether it was attempted (resolved for a
//!   connection) and connected (answered), the shape the coordinator reports
//!   as `network_usage`.

use std::collections::BTreeMap;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// What one declared endpoint saw during the run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EndpointUse {
    pub attempted: bool,
    pub connected: bool,
}

pub(crate) struct HostNetwork {
    /// Declared `host:port` (lowercase) → its pinned addresses.
    grants: BTreeMap<String, Vec<SocketAddr>>,
    max_bytes: u64,
    used: AtomicU64,
    usage: Mutex<BTreeMap<String, EndpointUse>>,
}

static NETWORK: OnceLock<HostNetwork> = OnceLock::new();

/// Install the host run's grant: declared endpoint → pinned addresses.
pub(crate) fn install(
    grants: &BTreeMap<String, Vec<String>>,
    max_bytes: u64,
) -> anyhow::Result<()> {
    let mut parsed = BTreeMap::new();
    for (grant, pins) in grants {
        let addresses = pins
            .iter()
            .map(|pin| pin.parse::<SocketAddr>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| anyhow::anyhow!("invalid pinned address for {grant}: {error}"))?;
        parsed.insert(normalize(grant), addresses);
    }
    let usage = parsed
        .keys()
        .map(|grant| (grant.clone(), EndpointUse::default()))
        .collect();
    NETWORK
        .set(HostNetwork {
            grants: parsed,
            max_bytes,
            used: AtomicU64::new(0),
            usage: Mutex::new(usage),
        })
        .map_err(|_| anyhow::anyhow!("host network policy is already installed"))
}

/// The use each declared endpoint saw, keyed as declared (normalized).
pub(crate) fn usage() -> Option<BTreeMap<String, EndpointUse>> {
    NETWORK.get().map(|network| {
        network
            .usage
            .lock()
            .map(|usage| usage.clone())
            .unwrap_or_default()
    })
}

/// `host:port` with the host lowercased and IPv6 brackets kept, so URL
/// network locations and NEEDS spellings compare equal.
fn normalize(netloc: &str) -> String {
    if let Ok(address) = netloc.parse::<SocketAddr>() {
        return address.to_string();
    }
    netloc.to_ascii_lowercase()
}

/// The hops of one request, as the resolver saw them.
#[derive(Default)]
struct Hops(Mutex<Vec<String>>);

struct GrantResolver(Arc<Hops>);

impl ureq::Resolver for GrantResolver {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<SocketAddr>> {
        let network = NETWORK
            .get()
            .expect("resolver exists only with a host network");
        let key = normalize(netloc);
        match network.grants.get(&key) {
            Some(addresses) => {
                if let Ok(mut usage) = network.usage.lock() {
                    usage.entry(key.clone()).or_default().attempted = true;
                }
                if let Ok(mut hops) = self.0.0.lock() {
                    hops.push(key);
                }
                Ok(addresses.clone())
            }
            None => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "network endpoint {netloc} is not declared in NEEDS.network (declared: {}); \
                     a recipe reaches only the HOST:PORT pairs it declares",
                    if network.grants.is_empty() {
                        "none".to_owned()
                    } else {
                        network
                            .grants
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ),
            )),
        }
    }
}

/// An agent builder with the host run's resolver when one is installed.
/// Returns the hop record [`finished`] reads.
pub(crate) fn agent(builder: ureq::AgentBuilder) -> (ureq::AgentBuilder, Option<HopRecord>) {
    if NETWORK.get().is_none() {
        return (builder, None);
    }
    let hops = Arc::new(Hops::default());
    (
        builder.resolver(GrantResolver(hops.clone())),
        Some(HopRecord(hops)),
    )
}

/// The endpoints one request resolved, in order.
pub(crate) struct HopRecord(Arc<Hops>);

/// Record which endpoints answered once a request finished: every hop before
/// the last answered (a redirect), and the last one answered unless the
/// request failed in transport.
pub(crate) fn finished(record: Option<&HopRecord>, answered: bool) {
    let (Some(record), Some(network)) = (record, NETWORK.get()) else {
        return;
    };
    let Ok(hops) = record.0.0.lock() else {
        return;
    };
    let Ok(mut usage) = network.usage.lock() else {
        return;
    };
    let count = hops.len();
    for (index, hop) in hops.iter().enumerate() {
        if index + 1 < count || answered {
            usage.entry(hop.clone()).or_default().connected = true;
        }
    }
}

/// Send a request built on [`agent`], recording the endpoints it reached.
/// The error is ureq's own, unchanged, as callers match on it.
#[allow(clippy::result_large_err)]
pub(crate) fn call(
    request: ureq::Request,
    record: Option<&HopRecord>,
) -> Result<ureq::Response, ureq::Error> {
    let result = request.call();
    finished(record, !matches!(result, Err(ureq::Error::Transport(_))));
    result
}

/// A response body, counted against `network_max_bytes` in a host run.
pub(crate) fn body(response: ureq::Response) -> Box<dyn Read + Send + Sync + 'static> {
    let reader = response.into_reader();
    if NETWORK.get().is_none() {
        return reader;
    }
    Box::new(Counted(reader))
}

struct Counted(Box<dyn Read + Send + Sync + 'static>);

impl Read for Counted {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = self.0.read(buffer)?;
        let network = NETWORK
            .get()
            .expect("counted body exists only with a host network");
        let used = network.used.fetch_add(count as u64, Ordering::Relaxed) + count as u64;
        if used > network.max_bytes {
            return Err(std::io::Error::other(format!(
                "network byte limit reached: this run received {used} bytes, over NEEDS.network_max_bytes {}",
                network.max_bytes
            )));
        }
        Ok(count)
    }
}
