//! Compile Recipe declarations through the selected Ginux catalog, without
//! starting a workload or resolving a network hostname.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

pub const CATALOG: u32 = 1002;
// The build-base digests Ginux must report for the derived contract, with and
// without an outbound grant. Reviewed after upstream cfebfdab: configured
// rt_sigpending and sigaltstack affect only the calling thread. Fixed
// exclusions remain. Both are part of `crate::identity::Declared`.
pub(crate) const BASE_DIGEST_NETWORK: &str =
    "cce1895cb6da15e246154bf0ee41dfe639a9a338508091da4edb762a64b8bbde";
pub(crate) const BASE_DIGEST_OFFLINE: &str =
    "0c8e73d32ee6d54ddfed2cc42a451f517f781dc1837b49555414b9ff7fe97359";
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

/// Whether `options` grant any outbound endpoint, which selects the contract.
fn connects(options: &crate::ExecutionOptions) -> bool {
    !options.network_hosts.is_empty()
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
    /// The contract Recipe derives for resolved `options`.
    pub(crate) fn of(options: &crate::ExecutionOptions) -> Self {
        Self::derived(connects(options))
    }

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
            catalog: CATALOG,
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

    /// Map the effective upstream rule back to its declaration/base owner.
    pub fn provenance(&self, source: &str, index: usize, operation: &str) -> String {
        if operation == TCP_CONNECT {
            return if source == "default-deny" {
                "NEEDS.network has no effective outbound TCP grant (including caller narrowing)"
                    .into()
            } else {
                "NEEDS.network (restricted by caller subset and pinned numeric endpoints)".into()
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
/// Actual invocation constraints are rendered and validated separately.
pub fn inspection_config(options: &crate::ExecutionOptions) -> String {
    format!(
        "schema_version = 1\n[instance]\nid = \"recipe-contract\"\nroot = \"oci:recipe-contract@sha256:{}\"\ninit = \"{}\"\nargs = []\nenvironment = []\n[resources]\nvcpus = {}\nmemory_mib = {}\n[execution]\nprofile = \"balanced\"\nbackend = \"ptrace-native\"\n{}\n[storage]\nassignments = []\n[devices]\nassignments = []\n[network]\nbackend = \"none\"\n[audit]\nenabled = true\n[failure]\npolicy = \"stop\"\n",
        "0".repeat(64),
        crate::domain::WORKER,
        options.jobs,
        options.memory_mib,
        Contract::of(options).render()
    )
}

pub(crate) fn command(
    launcher: &Path,
    config: &str,
    operation: &str,
    argument: Option<&str>,
) -> Result<String> {
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new()?;
    file.write_all(config.as_bytes())?;
    let mut command = Command::new(launcher);
    command.arg(operation).arg(file.path());
    if let Some(argument) = argument {
        command.arg(argument);
    }
    let output = crate::process::Run::new(command)
        .keep(crate::process::Keep::Limit(256 * 1024))
        .deadline(Duration::from_secs(10))
        .run()
        .map_err(|error| anyhow::anyhow!("Ginux declaration validation failed: {error:?}"))?;
    ensure!(
        output.exit.success(),
        "Ginux {operation} rejected derived contract: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).context("Ginux contract output is not UTF-8")
}

/// Validate the declaration-only contract of `options` ([`inspection_config`]).
pub(crate) fn inspect(launcher: &Path, options: &crate::ExecutionOptions) -> Result<Receipt> {
    validate(launcher, &inspection_config(options), options)
}

/// Have Ginux validate `config`, which must carry the contract of `options`.
pub(crate) fn validate(
    launcher: &Path,
    config: &str,
    options: &crate::ExecutionOptions,
) -> Result<Receipt> {
    let network = connects(options);
    let text = command(launcher, config, "instance-validate", None)?;
    ensure!(
        text.starts_with("Contract valid:"),
        "Ginux does not implement required contract validation"
    );
    let value = |field: &str| {
        text.split_whitespace()
            .find_map(|word| word.strip_prefix(field))
    };
    ensure!(
        value("configured-network-audit=") == Some("1"),
        "Ginux lacks required configured endpoint audit/usage v1; select the NEEDS-capable configured runtime"
    );
    ensure!(
        value("configured-denial-audit=") == Some("1"),
        "Ginux lacks required configured bounded denial audit v1; select the NEEDS-capable configured runtime"
    );
    let catalog: u32 = value("catalog=")
        .context("Ginux omitted selected catalog")?
        .parse()?;
    ensure!(
        catalog == CATALOG,
        "Ginux selected catalog {catalog}, expected {CATALOG}"
    );
    let digest = value("digest=")
        .context("Ginux omitted contract digest")?
        .to_owned();
    ensure!(
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid Ginux contract digest"
    );
    let expected = if network {
        BASE_DIGEST_NETWORK
    } else {
        BASE_DIGEST_OFFLINE
    };
    ensure!(
        digest == expected,
        "Ginux catalog-{CATALOG} build-base digest changed: expected {expected}, got {digest}; review the catalog and derived grants explicitly"
    );
    Ok(Receipt {
        catalog,
        digest,
        contract: Contract::derived(network),
    })
}
