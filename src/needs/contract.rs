//! Compile Recipe declarations through the selected Ginux catalog, without
//! starting a workload or resolving a network hostname.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

/// The Ginux runtime Recipe runs its workers on. Each kind has its own pins
/// ([`Pins`]); [`RUNTIME`] is the one in use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeKind {
    /// Upstream's public instance lifecycle (`ginux import`, `inject`,
    /// `start --wait`, `status`, `extract`, `delete`; `crate::launch`), the
    /// native configured runtime under catalog 2.
    PublicLifecycle,
}

/// The runtime kind Recipe uses.
pub const RUNTIME: RuntimeKind = RuntimeKind::PublicLifecycle;

/// What Recipe pins about a runtime: the catalog it must select and the
/// build-base digests it must report for the derived contract, with and
/// without an outbound grant. All three are part of
/// `crate::identity::Declared`.
#[derive(Debug)]
pub struct Pins {
    pub catalog: u32,
    pub base_digest_network: &'static str,
    pub base_digest_offline: &'static str,
}

impl Pins {
    /// The build-base digest for a contract with (`network`) or without an
    /// outbound grant.
    pub fn base_digest(&self, network: bool) -> &'static str {
        if network {
            self.base_digest_network
        } else {
            self.base_digest_offline
        }
    }
}

impl RuntimeKind {
    /// The pins table: one row per runtime kind, the only place the values
    /// are written. Every reader goes through it.
    pub const fn pins(self) -> &'static Pins {
        match self {
            // The build-base digests Ginux must report for the derived contract, with and
            // without an outbound grant, under catalog 2. Each change is reviewed: for every
            // `allow`/`deny` selector, `ginux inspect` and `ginux explain` on the two
            // declaration-only configs ([`inspection_config`]); the review is recorded here.
            // Public-lifecycle cutover (LevitateOS/ginux 8f4d53ba): catalog 1002 → 2; the
            // configured operations lstat, readv, pipe, nanosleep, sendfile, link and
            // rt_sigpending are not in catalog 2; selectors unchanged; both digests change.
            // Derived for catalog 2 against LevitateOS/ginux 5ca4e54b (docs/ginux-cutover-design.md,
            // section 6), twice and independently with the same results: Recipe's
            // managed-integration CI run 45298702489 (Ginux 5ca4e54b, Recipe 66d046f), whose
            // "build-base digest changed" error reported the offline digest for 91 cases and the
            // network digest for 9, and a by-hand render of `inspection_config` against Ginux
            // 1dd255ac (same contract code) validated with both `ginux validate` and
            // `instance-validate`. Port lists do not change the digest.
            RuntimeKind::PublicLifecycle => &Pins {
                catalog: 2,
                base_digest_network: "5c382b108027c3f9c3c2aaa371f4a1f5fad3e2bd60b2a33102a878cfcf4fe12d",
                base_digest_offline: "a52529e93a62357d55d955628eecc5e15feb91802e6f2645ace20dfde11056a9",
            },
        }
    }
}

/// The pins of the runtime in use ([`RUNTIME`]).
pub const fn pins() -> &'static Pins {
    RUNTIME.pins()
}

/// The Ginux operation a `NEEDS.network` endpoint grants.
pub(crate) const TCP_CONNECT: &str = "network.inet.tcp.connect";

/// The Ginux name of an operation a caller asks about; `network.connect` is
/// Recipe's short spelling of [`TCP_CONNECT`].
pub(crate) fn operation(name: &str) -> &str {
    if name == "network.connect" {
        TCP_CONNECT
    } else {
        name
    }
}

/// The distinct TCP ports of the endpoints `options` declare, sorted. Known
/// without resolving a host, so it is empty exactly when `options` declare no
/// endpoint (an entry that does not parse is refused where the grant is
/// resolved, `crate::inputs`). A run grants only the ports that kept a pinned
/// address (`crate::inputs::network::NetworkPolicy::outbound`).
pub(crate) fn outbound_ports(options: &crate::ExecutionOptions) -> Vec<u16> {
    options
        .network_hosts
        .iter()
        .filter_map(|grant| match grant.parse::<std::net::SocketAddr>() {
            Ok(address) => Some(address.port()),
            Err(_) => grant.rsplit_once(':')?.1.parse().ok(),
        })
        .filter(|port| *port != 0)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The outbound side of a Ginux config: the TCP ports `[contract.network]
/// outbound` grants, the `[network]` backend they need and the contract they
/// select. The one place this is decided, for the declaration-only config
/// ([`Outbound::declared`], [`inspection_config`]) and for a run's config
/// (`crate::inputs::network::NetworkPolicy::outbound`, the ports that kept a
/// pinned address). Ginux refuses an outbound list on any backend but
/// `shared`, so no ports means backend `none` and the offline contract.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Outbound {
    ports: Vec<u16>,
}

impl Outbound {
    /// The grant of `ports`: distinct, sorted, port 0 dropped.
    pub(crate) fn of(ports: impl IntoIterator<Item = u16>) -> Self {
        Self {
            ports: ports
                .into_iter()
                .filter(|port| *port != 0)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }

    /// The ports `options` declare, unresolved: what `recipe inspect` and the
    /// per-node check validate.
    pub(crate) fn declared(options: &crate::ExecutionOptions) -> Self {
        Self::of(outbound_ports(options))
    }

    /// The granted ports (`[contract.network] outbound`).
    pub(crate) fn ports(&self) -> &[u16] {
        &self.ports
    }

    /// Whether any port is granted: selects the contract and its digest.
    pub(crate) fn network(&self) -> bool {
        !self.ports.is_empty()
    }

    /// The `[network] backend`: `shared` with granted ports, else `none`.
    pub(crate) fn backend(&self) -> &'static str {
        if self.network() { "shared" } else { "none" }
    }

    /// The contract this grant selects.
    pub(crate) fn contract(&self) -> Contract {
        Contract::derived(self.network())
    }

    /// The `[network]` section of a config.
    pub(crate) fn network_section(&self) -> String {
        format!("\n[network]\nbackend = \"{}\"\n", self.backend())
    }
}

/// Finite, version-pinned build base in upstream's hierarchical format.
/// Assignments and endpoint/resource owners still constrain every admitted call.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Contract {
    pub catalog: u32,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

impl Contract {
    pub fn derived(network: bool) -> Self {
        let mut allow = vec![
            "syscall.*",
            "filesystem.path.read",
            "filesystem.path.write",
            "device.console.read",
            "device.console.write",
            "time.clock.read",
            "signal.virtual.deliver",
            "credential.virtual.set",
            "memory.virtual.manage",
        ];
        if network {
            allow.push(TCP_CONNECT);
        }
        // The catalog is pinned. These exclusions preserve the old base's
        // admitted operation set; upstream's explicit denials return EPERM.
        let mut deny = vec![
            "device.mmio.direct",
            "device.dma.direct",
            "network.packet.raw",
            "syscall.accept*",
            "syscall.bind",
            "syscall.listen",
        ];
        if !network {
            deny.extend([
                "syscall.connect",
                "syscall.*msg",
                "syscall.shutdown",
                "syscall.getsockname",
                "syscall.getpeername",
                "syscall.*sockopt",
            ]);
        }
        Self {
            catalog: pins().catalog,
            allow: allow.into_iter().map(str::to_owned).collect(),
            deny: deny.into_iter().map(str::to_owned).collect(),
        }
    }

    pub fn render(&self) -> String {
        format!(
            "\n[contract]\nversion = 1\ncatalog = {}\ndefault = \"deny\"\nallow = {}\ndeny = {}\n",
            self.catalog,
            serde_json::to_string(&self.allow).expect("string array"),
            serde_json::to_string(&self.deny).expect("string array"),
        )
    }

    /// The contract's filesystem and network lists: everything readable, only
    /// the writable trees written, and outbound TCP to `ports` (none: no
    /// network section). Ginux derives grants from whether each list is
    /// present, not from its entries, so these sections enter the contract
    /// digest only by being there; [`inspection_config`] renders them as an
    /// invocation config does.
    pub fn render_lists(ports: &[u16]) -> String {
        let mut out = format!(
            "\n[contract.filesystem]\ndefault = \"deny\"\nread = [\"/\"]\nwrite = [\"{work}\", \"{out}\"]\ncreate = [\"{work}\", \"{out}\"]\n",
            work = crate::domain::WORK,
            out = crate::domain::OUT,
        );
        if !ports.is_empty() {
            let outbound = ports
                .iter()
                .map(|port| format!("tcp:{port}"))
                .collect::<Vec<_>>();
            out.push_str(&format!(
                "\n[contract.network]\ndefault = \"deny\"\noutbound = {}\n",
                serde_json::to_string(&outbound).expect("string array")
            ));
        }
        out
    }

    /// Map the effective upstream rule back to its declaration/base owner.
    pub fn provenance(&self, source: &str, index: usize, operation: &str) -> String {
        if operation == TCP_CONNECT {
            return if source == "default-deny" {
                "NEEDS.network has no effective outbound TCP grant (including caller narrowing)"
                    .into()
            } else {
                "NEEDS.network: outbound TCP on the shared host network to the ports of the \
                 declared endpoints (caller narrowing included); only Recipe's own downloads are \
                 held to the pinned addresses"
                    .into()
            };
        }
        if source == "exact-deny" {
            return format!(
                "Recipe fixed exclusion: contract.deny[{index}] = {}; assignments and runtime bounds still apply",
                self.deny
                    .get(index)
                    .map(String::as_str)
                    .unwrap_or("unknown rule")
            );
        }
        if source == "default-deny" {
            return if operation.starts_with("network.") {
                "Recipe fixed exclusion: no inbound or UDP authority is granted by NEEDS".into()
            } else {
                "Ginux capability/build-base question: no NEEDS field grants this operation".into()
            };
        }
        format!(
            "Recipe catalog-{} build base: contract.allow[{index}]; assignments and runtime bounds still apply",
            self.catalog
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    pub catalog: u32,
    pub digest: String,
    pub contract: Contract,
}

/// A declaration-only config: no image opening, DNS, staging or recipe setup.
/// Actual invocation constraints are rendered and validated separately
/// (`crate::inputs::InstanceConfig::render`); the contract sections are the
/// same text, so both give the same digest.
pub fn inspection_config(options: &crate::ExecutionOptions) -> String {
    let outbound = Outbound::declared(options);
    format!(
        "schema_version = 1\n[instance]\nid = \"recipe-contract\"\nroot = \"local/recipe-contract@sha256:{}\"\ninit = \"{}\"\nargs = []\nenvironment = []\n[resources]\nvcpus = {}\nmemory_mib = {}\n[execution]\nprofile = \"balanced\"\nbackend = \"ptrace-native\"\n[access]\nuid = {}\ngid = {}\n{}{}\n[storage]\nassignments = []\n[devices]\nassignments = []\n{}\n[audit]\nenabled = true\n[failure]\npolicy = \"stop\"\n",
        "0".repeat(64),
        crate::domain::WORKER,
        options.jobs,
        options.memory_mib,
        crate::domain::UID,
        crate::domain::UID,
        outbound.contract().render(),
        Contract::render_lists(outbound.ports()),
        outbound.network_section(),
    )
}

/// Run `ginux validate FILE`, or `ginux explain FILE OPERATION` when
/// `operation` is given, on `config` written to a private file. Both parse
/// the file locally, unprivileged, and start nothing. A non-zero exit is an
/// error carrying Ginux's own text.
pub(crate) fn command(
    ginux: &crate::ginux::Ginux,
    config: &str,
    operation: Option<&str>,
) -> Result<String> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(config.as_bytes())?;
    let (verb, call) = match operation {
        None => ("validate", ginux.validate(file.path())?),
        Some(operation) => ("explain", ginux.explain(file.path(), operation)?),
    };
    ensure!(
        call.ok(),
        "ginux {verb} rejected the derived contract: {} {}",
        String::from_utf8_lossy(&call.stdout),
        call.message()
    );
    String::from_utf8(call.stdout).context("ginux contract output is not UTF-8")
}

/// Validate the declaration-only contract of `options` ([`inspection_config`]).
pub(crate) fn inspect(
    ginux: &crate::ginux::Ginux,
    options: &crate::ExecutionOptions,
) -> Result<Receipt> {
    validate(ginux, &inspection_config(options), options)
}

/// Have Ginux validate `config`, a contract Recipe derived for `options`.
///
/// The outbound grant is read from `config` itself (a run grants only the
/// declared ports that kept a pinned address, [`Outbound`]): its ports must be
/// declared by `options`, and its contract and backend must be the ones that
/// grant selects. The catalog is read from `config` too: `ginux validate`
/// prints the catalog compiled into the CLI, whatever the config selects, so
/// only its `digest=` is trusted.
pub(crate) fn validate(
    ginux: &crate::ginux::Ginux,
    config: &str,
    options: &crate::ExecutionOptions,
) -> Result<Receipt> {
    let pins = pins();
    let parsed: toml::Value = toml::from_str(config).context("invalid rendered Ginux config")?;
    let contract = parsed
        .get("contract")
        .context("the rendered Ginux config has no contract")?;
    let catalog = contract
        .get("catalog")
        .and_then(toml::Value::as_integer)
        .context("the rendered Ginux config selects no catalog")?;
    ensure!(
        catalog == i64::from(pins.catalog),
        "the rendered Ginux config selects catalog {catalog}, expected {}",
        pins.catalog
    );
    let ports = match contract
        .get("network")
        .and_then(|network| network.get("outbound"))
    {
        None => Vec::new(),
        Some(list) => list
            .as_array()
            .context("the rendered Ginux config's outbound list is not a list")?
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .and_then(|entry| entry.strip_prefix("tcp:"))
                    .and_then(|port| port.parse::<u16>().ok())
                    .with_context(|| {
                        format!("the rendered Ginux config grants {entry}, not a TCP port")
                    })
            })
            .collect::<Result<_>>()?,
    };
    let outbound = Outbound::of(ports);
    let declared = outbound_ports(options);
    ensure!(
        outbound.ports().iter().all(|port| declared.contains(port)),
        "the rendered Ginux config grants outbound ports {:?}, NEEDS.network declares {declared:?}",
        outbound.ports()
    );
    let derived = outbound.contract();
    let list = |key: &str| -> Option<Vec<String>> {
        contract
            .get(key)?
            .as_array()?
            .iter()
            .map(|rule| rule.as_str().map(str::to_owned))
            .collect()
    };
    ensure!(
        list("allow").as_ref() == Some(&derived.allow)
            && list("deny").as_ref() == Some(&derived.deny),
        "the rendered Ginux config's contract is not the one its outbound grant selects"
    );
    let backend = parsed
        .get("network")
        .and_then(|network| network.get("backend"))
        .and_then(toml::Value::as_str);
    ensure!(
        backend == Some(outbound.backend()),
        "the rendered Ginux config selects network backend {backend:?}, its outbound grant needs {:?}",
        outbound.backend()
    );
    let network = outbound.network();
    let text = command(ginux, config, None)?;
    ensure!(
        text.starts_with("Contract valid:"),
        "ginux validate printed no contract: {text}"
    );
    let digest = text
        .split_whitespace()
        .find_map(|word| word.strip_prefix("digest="))
        .context("ginux validate omitted the contract digest")?
        .to_owned();
    ensure!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid Ginux contract digest"
    );
    let expected = pins.base_digest(network);
    ensure!(
        digest == expected,
        "Ginux catalog-{} build-base digest changed: expected {expected}, got {digest}; \
         review the catalog and derived grants explicitly",
        pins.catalog,
    );
    Ok(Receipt {
        catalog: pins.catalog,
        digest,
        contract: derived,
    })
}
