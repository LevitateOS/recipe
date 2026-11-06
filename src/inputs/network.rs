//! Coordinator-owned outbound grants. Workloads receive pinned addresses and
//! immutable hostname mappings, never a host DNS or proxy capability.
use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct NetworkPolicy {
    pub endpoints: Vec<String>,
    pub max_bytes: u64,
    hosts: String,
}

impl NetworkPolicy {
    pub fn resolve(options: &crate::ExecutionOptions) -> Result<Self> {
        options.validate()?;
        if options.network_hosts.is_empty() {
            return Ok(Self::default());
        }
        let mut endpoints = BTreeSet::new();
        let mut hosts = BTreeSet::new();
        for grant in &options.network_hosts {
            if let Ok(endpoint) = grant.parse::<SocketAddr>() {
                validate_endpoint(endpoint)?;
                endpoints.insert(endpoint.to_string());
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
                    endpoints.insert(endpoint.to_string());
                    hosts.insert(format!("{address} {}\n", host.to_ascii_lowercase()));
                }
            }
            if endpoints.len() > 8 {
                bail!("network grants resolve to more than eight endpoints; select fewer hosts");
            }
        }
        Ok(Self {
            endpoints: endpoints.into_iter().collect(),
            max_bytes: options.network_max_bytes,
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
    use std::io::{Read, Seek, SeekFrom};
    let mut output = tempfile::tempfile()?;
    let mut child = Command::new("/usr/bin/getent")
        .args(["ahosts", host])
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(output.try_clone()?)
        .spawn()
        .context("cannot start bounded hostname resolver")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if crate::cancellation::signal() != 0 {
            let _ = child.kill();
            let _ = child.wait();
            crate::cancellation::check()?;
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline || output.metadata()?.len() > 64 * 1024 {
            let _ = child.kill();
            let _ = child.wait();
            bail!("network hostname resolution exceeded its time or output bound: {host}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !status.success() {
        bail!("cannot resolve network host {host}");
    }
    output.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    output.take(64 * 1024 + 1).read_to_string(&mut text)?;
    if text.len() > 64 * 1024 {
        bail!("hostname resolver output exceeds limit");
    }
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
