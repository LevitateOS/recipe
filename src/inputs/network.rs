//! Coordinator-owned outbound grants. Workloads receive pinned addresses and
//! immutable hostname mappings, never a host DNS or proxy capability.
use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

#[derive(Debug, Clone, Default)]
pub(crate) struct NetworkPolicy {
    pub endpoints: Vec<String>,
    pub max_bytes: u64,
    hosts: String,
    pub host_endpoints: BTreeMap<String, Vec<String>>,
}

impl NetworkPolicy {
    pub fn resolve(options: &crate::ExecutionOptions) -> Result<Self> {
        if options.network_hosts.is_empty() {
            return Ok(Self::default());
        }
        let mut endpoints = BTreeSet::new();
        let mut candidates = BTreeMap::new();
        let mut names = BTreeMap::new();
        for grant in &options.network_hosts {
            let mut resolved = BTreeSet::new();
            if let Ok(endpoint) = grant.parse::<SocketAddr>() {
                validate_endpoint(endpoint)?;
                // An explicit numeric grant must never be displaced by DNS.
                endpoints.insert(endpoint);
                resolved.insert(endpoint);
            } else {
                let (host, port) = grant
                    .rsplit_once(':')
                    .context("network host requires HOST:PORT")?;
                let port: u16 = port
                    .parse()
                    .context("network port must be between 1 and 65535")?;
                if host.len() > 253
                    || host.is_empty()
                    || !host.split('.').all(|label| {
                        !label.is_empty()
                            && label.len() <= 63
                            && !label.starts_with('-')
                            && !label.ends_with('-')
                            && label
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    })
                {
                    bail!("invalid network hostname {host:?}");
                }
                for address in resolve_host(host)? {
                    let endpoint = SocketAddr::new(address, port);
                    validate_endpoint(endpoint)?;
                    resolved.insert(endpoint);
                }
                names.insert(grant.clone(), host.to_ascii_lowercase());
            }
            candidates.insert(grant.clone(), resolved);
        }
        let limit = crate::domain::MAX_NETWORK_ENDPOINTS;
        if endpoints.len() > limit {
            bail!("network grants require more than {limit} numeric endpoints");
        }
        // Reserve at least one address per declared grant, then distribute the
        // remaining slots round-robin. Sorted grants/addresses make selection
        // independent of NSS output order; SocketAddr orders IPv4 before IPv6.
        for (grant, resolved) in &candidates {
            if !resolved.iter().any(|endpoint| endpoints.contains(endpoint)) {
                if endpoints.len() == limit {
                    bail!(
                        "network grants cannot fit one address per host in {limit} endpoints; select fewer hosts (at {grant})"
                    );
                }
                endpoints.insert(*resolved.first().context("network grant has no addresses")?);
            }
        }
        while endpoints.len() < limit {
            let mut changed = false;
            for resolved in candidates.values() {
                if let Some(endpoint) = resolved
                    .iter()
                    .find(|endpoint| !endpoints.contains(endpoint))
                {
                    endpoints.insert(*endpoint);
                    changed = true;
                    if endpoints.len() == limit {
                        break;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let mut hosts = BTreeSet::new();
        let mut host_endpoints = BTreeMap::new();
        for (grant, resolved) in &candidates {
            let selected = resolved
                .intersection(&endpoints)
                .copied()
                .collect::<Vec<_>>();
            if selected.len() < resolved.len() {
                crate::diagnostics::diag!(
                    "recipe: network grant {grant}: selected {} of {} resolved addresses (endpoint limit {limit}); exact pins are recorded in authority.json",
                    selected.len(),
                    resolved.len()
                );
            }
            if let Some(host) = names.get(grant) {
                for endpoint in &selected {
                    hosts.insert(format!("{} {host}\n", endpoint.ip()));
                }
            }
            host_endpoints.insert(
                grant.clone(),
                selected.iter().map(ToString::to_string).collect(),
            );
        }
        Ok(Self {
            endpoints: endpoints.iter().map(ToString::to_string).collect(),
            max_bytes: options.network_max_bytes,
            host_endpoints,
            hosts: hosts.into_iter().collect(),
        })
    }
}

fn validate_endpoint(endpoint: SocketAddr) -> Result<()> {
    if endpoint.port() == 0
        || endpoint.ip().is_unspecified()
        || endpoint.ip().is_multicast()
        || endpoint.ip() == IpAddr::V4(std::net::Ipv4Addr::BROADCAST)
    {
        bail!("invalid outbound network endpoint {endpoint}");
    }
    Ok(())
}

/// NSS runs in a killable subprocess: an unavailable resolver cannot block
/// coordinator startup indefinitely. Output goes to a private bounded-size
/// result, avoiding pipe capacity deadlocks while the parent waits.
fn resolve_host(host: &str) -> Result<BTreeSet<IpAddr>> {
    let mut command = Command::new("/usr/bin/getent");
    command
        .args(["ahosts", host])
        .env_clear()
        .env("LC_ALL", "C");
    let finished = crate::process::Run::new(command)
        .keep(crate::process::Keep::Limit(64 * 1024))
        .deadline(Duration::from_secs(10))
        .run();
    crate::cancellation::check()?;
    let finished = match finished {
        Ok(finished) => finished,
        Err(crate::process::Error::Spawn(error)) => {
            bail!("cannot start bounded hostname resolver: {error}")
        }
        Err(crate::process::Error::Failed(_)) => {
            bail!("network hostname resolution exceeded its time or output bound: {host}")
        }
    };
    if !finished.exit.success() {
        bail!("cannot resolve network host {host}");
    }
    let text =
        String::from_utf8(finished.stdout).context("hostname resolver output is not UTF-8")?;
    let addresses = text
        .lines()
        .map(|line| {
            line.split_whitespace()
                .next()
                .context("empty resolver row")?
                .parse::<IpAddr>()
                .context("resolver returned invalid address")
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if addresses.is_empty() {
        bail!("network host {host} has no addresses");
    }
    Ok(addresses)
}

/// The host-run counterpart of [`install_hosts`]: what a host worker's
/// resolver may answer (`crate::helpers::acquire::net`).
impl NetworkPolicy {
    pub(crate) fn host_grants(&self) -> BTreeMap<String, Vec<String>> {
        self.host_endpoints.clone()
    }

    /// Refuse a host run whose pinned addresses reach this machine or its
    /// private networks: loopback, link-local, private, CGNAT and
    /// unique-local addresses (and their IPv4-mapped forms), unless the
    /// `NEEDS.network` entry is that address itself. A Ginux run keeps its
    /// own rules.
    pub(crate) fn check_host_reachable(&self) -> Result<()> {
        for (grant, pins) in &self.host_endpoints {
            let literal = grant.parse::<SocketAddr>().ok();
            for pin in pins {
                let address: SocketAddr = pin
                    .parse()
                    .with_context(|| format!("invalid pinned address {pin} for {grant}"))?;
                if literal == Some(address) {
                    continue;
                }
                if let Some(rule) = internal_address(address.ip()) {
                    bail!(
                        "network endpoint {grant} resolves to {}, which is {rule}; a recipe running \
                         on the host may reach such an address only when NEEDS.network names it \
                         as that address itself (e.g. \"{address}\")",
                        address.ip()
                    );
                }
            }
        }
        Ok(())
    }
}

/// Which internal or special range `address` lies in, if any. IPv6 forms
/// that carry an IPv4 address (mapped, compatible, NAT64, 6to4) are judged by
/// that address.
fn internal_address(address: IpAddr) -> Option<&'static str> {
    let v4 = |ip: std::net::Ipv4Addr| -> Option<&'static str> {
        let [a, b, c, _] = ip.octets();
        if a == 0 {
            Some("a \"this network\" address (0.0.0.0/8)")
        } else if ip.is_loopback() {
            Some("a loopback address (127.0.0.0/8)")
        } else if ip.is_link_local() {
            Some("a link-local address (169.254.0.0/16)")
        } else if a == 10 {
            Some("a private address (10.0.0.0/8)")
        } else if a == 172 && (16..32).contains(&b) {
            Some("a private address (172.16.0.0/12)")
        } else if a == 192 && b == 168 {
            Some("a private address (192.168.0.0/16)")
        } else if a == 100 && (64..128).contains(&b) {
            Some("a carrier-grade NAT address (100.64.0.0/10)")
        } else if a == 198 && (b == 18 || b == 19) {
            Some("a benchmarking address (198.18.0.0/15)")
        } else if a == 192 && b == 0 && c == 0 {
            Some("an IETF protocol assignment (192.0.0.0/24)")
        } else if a >= 240 {
            Some("a reserved address (240.0.0.0/4)")
        } else {
            None
        }
    };
    let embedded = |ip: std::net::Ipv4Addr, form: &'static str| -> Option<&'static str> {
        v4(ip).map(|_| form)
    };
    match address {
        IpAddr::V4(ip) => v4(ip),
        IpAddr::V6(ip) => {
            let s = ip.segments();
            let low = std::net::Ipv4Addr::new(
                (s[6] >> 8) as u8,
                s[6] as u8,
                (s[7] >> 8) as u8,
                s[7] as u8,
            );
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return embedded(
                    mapped,
                    "an IPv4-mapped form of an internal or reserved address",
                );
            }
            if ip.is_loopback() {
                return Some("the IPv6 loopback address (::1)");
            }
            if s[..6] == [0; 6] {
                // ::a.b.c.d, IPv4-compatible (deprecated); `::` is unspecified.
                return embedded(
                    low,
                    "an IPv4-compatible form of an internal or reserved address",
                );
            }
            if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                return embedded(
                    low,
                    "a NAT64 form (64:ff9b::/96) of an internal or reserved address",
                );
            }
            if s[0] == 0x64 && s[1] == 0xff9b && s[2] == 1 {
                // 64:ff9b:1::/48, local-use NAT64: any embedding is local.
                return Some("a local-use NAT64 address (64:ff9b:1::/48)");
            }
            if s[0] == 0x2002 {
                let inner = std::net::Ipv4Addr::new(
                    (s[1] >> 8) as u8,
                    s[1] as u8,
                    (s[2] >> 8) as u8,
                    s[2] as u8,
                );
                return embedded(
                    inner,
                    "a 6to4 form (2002::/16) of an internal or reserved address",
                );
            }
            if s[0] == 0x2001 && s[1] == 0 {
                Some("a Teredo address (2001::/32)")
            } else if s[0] & 0xffc0 == 0xfe80 {
                Some("a link-local address (fe80::/10)")
            } else if s[0] & 0xffc0 == 0xfec0 {
                Some("a site-local address (fec0::/10)")
            } else if s[0] & 0xfe00 == 0xfc00 {
                Some("a unique-local address (fc00::/7)")
            } else {
                None
            }
        }
    }
}

pub(crate) fn install_hosts(policy: &NetworkPolicy, root: &Path) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if policy.endpoints.is_empty() {
        return Ok(());
    }
    let etc = root.join("etc");
    if !std::fs::symlink_metadata(&etc)?.is_dir() {
        bail!("image /etc must be a directory");
    }
    // Image construction is finished and /etc is read-only. Replace a single
    // leaf without following image symlinks, then seal it before launch.
    let mode = std::fs::metadata(&etc)?.permissions().mode();
    std::fs::set_permissions(&etc, std::fs::Permissions::from_mode(mode | 0o700))?;
    let path = etc.join("hosts");
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(format!("127.0.0.1 localhost\n::1 localhost\n{}", policy.hosts).as_bytes())?;
    file.set_permissions(std::fs::Permissions::from_mode(0o444))?;
    std::fs::set_permissions(etc, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}
