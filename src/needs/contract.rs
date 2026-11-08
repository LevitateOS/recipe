//! Compile Recipe declarations through the selected Ginux catalog, without
//! starting a workload or resolving a network hostname.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

pub const CATALOG: u32 = 2;

/// Finite, version-pinned build base. Syscall selectors select only the
/// corresponding access verb. Assigned filesystem/device/endpoint owners
/// continue to constrain every admitted operation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Contract {
    pub catalog: u32,
    pub read: Vec<String>,
    pub write: Vec<String>,
    pub execute: Vec<String>,
    pub connect: Vec<String>,
    pub listen: Vec<String>,
    pub process: Vec<String>,
    pub system: Vec<String>,
    pub deny: Vec<String>,
    pub exact_allow: Vec<String>,
}

impl Contract {
    pub fn derived(network: bool) -> Self {
        let names = |values: &[&str]| values.iter().map(|value| (*value).to_owned()).collect();
        Self {
            catalog: CATALOG,
            read: names(&[
                "filesystem.path.read",
                "device.console.read",
                "time.clock.read",
                "syscall.**",
            ]),
            write: names(&[
                "filesystem.path.write",
                "device.console.write",
                "syscall.**",
            ]),
            execute: names(&["syscall.**"]),
            connect: if network {
                names(&["network.inet.tcp.connect", "syscall.**"])
            } else {
                Vec::new()
            },
            listen: Vec::new(),
            process: names(&[
                "signal.virtual.deliver",
                "credential.virtual.set",
                "syscall.**",
            ]),
            system: names(&["memory.virtual.manage", "syscall.**"]),
            deny: names(&["dev.mmio.direct", "dev.dma.direct", "network.packet.raw"]),
            // Owned local packet endpoints use these numbers too. TCP calls
            // additionally require the network semantic grant and endpoint.
            exact_allow: names(&["syscall.sendto", "syscall.recvfrom"]),
        }
    }

    pub fn render(&self) -> String {
        let mut text = format!(
            "\n[userspace.contract]\nversion = 1\ncatalog = {}\ndefault = \"deny\"\n",
            self.catalog
        );
        for (name, values) in [
            ("read", &self.read),
            ("write", &self.write),
            ("execute", &self.execute),
            ("connect", &self.connect),
            ("listen", &self.listen),
            ("process", &self.process),
            ("system", &self.system),
            ("deny", &self.deny),
            ("exact_allow", &self.exact_allow),
        ] {
            text.push_str(&format!(
                "{name} = {}\n",
                serde_json::to_string(values).expect("string array")
            ));
        }
        text
    }

    /// The declaration/base owner corresponding to Ginux's recipe verb/index.
    pub fn provenance(&self, source: &str, index: usize, operation: &str) -> String {
        if source == "connect" {
            return "NEEDS.network (restricted by caller subset and pinned numeric endpoints)"
                .into();
        }
        if source == "default-deny" {
            return if operation == "network.inet.tcp.connect" {
                "NEEDS.network has no effective outbound TCP grant (including caller narrowing)"
                    .into()
            } else if operation.starts_with("network.")
                && (operation.ends_with("bind") || operation.ends_with("listen"))
            {
                "Recipe fixed exclusion: listening/binding is never granted by NEEDS".into()
            } else {
                "Ginux capability/build-base question: no NEEDS field grants this operation".into()
            };
        }
        if source == "deny" {
            return "Recipe fixed hardware/raw-packet exclusion".into();
        }
        format!(
            "Recipe catalog-{} build base: {source}[{index}]; assignments and runtime bounds still apply",
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
        "schema_version = 1\n[instance]\nid = \"recipe-contract\"\nroot = \"oci:recipe-contract@sha256:{}\"\ninit = \"/inputs/recipe-worker\"\nargs = []\nenvironment = []\n[resources]\nvcpus = {}\nmemory_mib = {}\n[execution]\nprofile = \"balanced\"\nbackend = \"ptrace-native\"\n{}\n[storage]\nassignments = []\n[devices]\nassignments = []\n[network]\nbackend = \"none\"\n[audit]\nmode = \"stderr\"\n[failure]\npolicy = \"stop\"\n",
        "0".repeat(64),
        options.jobs,
        options.memory_mib,
        Contract::derived(!options.network_hosts.is_empty()).render()
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
        "Ginux {operation} rejected derived contract: {}",
        format!(
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    );
    String::from_utf8(output.stdout).context("Ginux contract output is not UTF-8")
}

pub(crate) fn validate(launcher: &Path, config: &str, network: bool) -> Result<Receipt> {
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
        "4106370023c93de07ccc93b48b8883dc2e09186b1fa0ed1835e6d97ff7ccf314"
    } else {
        "121cb495da506cad611b9d81dfb38eb15ff23e9c965d8d758f3a1c5076f04f79"
    };
    ensure!(
        digest == expected,
        "Ginux catalog-2 build-base digest changed: expected {expected}, got {digest}; review the catalog and derived grants explicitly"
    );
    Ok(Receipt {
        catalog,
        digest,
        contract: Contract::derived(network),
    })
}
