//! Instance config rendering within the §2 hard limits.
//!
//! The text is the `ginux-instance` schema the public lifecycle imports
//! (`ginux import NAME FILE`): `[instance]` (init, args, environment),
//! `[resources]`, `[execution]` (`balanced`/`ptrace-native`), `[access]` (the
//! workload's virtual uid and gid), `[contract]` (catalog, allow, deny) with
//! its filesystem and network lists, `[storage]`, `[devices]`, `[network]`,
//! `[audit]` and `[failure]` (`stop`). Every bound is enforced here with an
//! explicit error; nothing is truncated.
//!
//! What the public lifecycle does not offer is not rendered: no storage
//! limits (outputs are bounded when they are extracted, `crate::launch`), no
//! per-address network endpoints, connection or byte caps (the shared network
//! admits any address on a granted port; the worker holds Recipe's own
//! downloads to the pins, `crate::helpers::acquire::net`).

use anyhow::{Result, bail};

/// §2 hard limits (bytes/items); enforced, never truncated.
const MAX_ARGS: usize = 7;
const MAX_ARG: usize = 128;
const MAX_ENV: usize = 8;
const MAX_ENV_ENTRY: usize = 256;
const MAX_CONFIG: usize = 4096;

/// Instance config for one invocation; [`InstanceConfig::render`] produces the
/// text `ginux import` reads.
#[derive(Debug, Clone)]
pub struct InstanceConfig {
    id: String,
    root: String,
    /// The image `root` names: the one the recipe asked for, or the full one
    /// where it asked for a minimal image and none is named.
    image: crate::runtime::Image,
    init: String,
    args: Vec<String>,
    environment: Vec<String>,
    vcpus: u64,
    memory_mib: u64,
    /// The outbound grant (`[contract.network] outbound`, `[network]
    /// backend`); no ports means no network at all.
    outbound: crate::needs::contract::Outbound,
    contract: crate::needs::contract::Contract,
}

impl InstanceConfig {
    /// Build the invocation config: id/root/init/args/env plus fixed
    /// profile selections. `env` is the §5 allowlisted environment; the worker
    /// init and argv come from [`crate::domain`]; `image` is the runtime image
    /// the recipe asks for (`NEEDS.runtime`); `outbound` is the grant this
    /// run's config carries ([`crate::inputs::network::NetworkPolicy::outbound`]).
    pub fn for_invocation(
        invocation_id: &str,
        env: &[(String, String)],
        options: &crate::ExecutionOptions,
        image: crate::runtime::Image,
        outbound: crate::needs::contract::Outbound,
    ) -> Result<Self> {
        let (image, root) = crate::runtime::reference_for(image)?;
        if env.len() > MAX_ENV {
            bail!(
                "input preparation failure: {} env entries exceed the §2 limit of {MAX_ENV} × 256 B",
                env.len(),
            );
        }
        let mut environment = Vec::with_capacity(env.len());
        for (name, value) in env {
            let entry = format!("{name}={value}");
            if entry.len() > MAX_ENV_ENTRY {
                bail!(
                    "input preparation failure: env entry {name:?} is {} B, exceeding §2 256 B",
                    entry.len(),
                );
            }
            if !entry.bytes().all(|byte| byte >= 32 && byte != 127) {
                bail!("input preparation failure: env entry {name:?} has control characters");
            }
            environment.push(entry);
        }
        Ok(Self {
            id: invocation_id.to_string(),
            root,
            image,
            init: crate::domain::WORKER.to_string(),
            args: crate::domain::WORKER_ARGS.map(str::to_string).to_vec(),
            environment,
            vcpus: u64::from(options.jobs),
            memory_mib: options.memory_mib,
            contract: outbound.contract(),
            outbound,
        })
    }

    pub(crate) fn root_reference(&self) -> &str {
        &self.root
    }

    /// Which image the root reference names, so its layout is selected by
    /// that image's variable and shipped file.
    pub(crate) fn root_image(&self) -> crate::runtime::Image {
        self.image
    }

    /// Render the config text (≤ 4096 B). The `id` is the invocation id; the
    /// lifecycle names the instance itself and `ginux import` sets the id to
    /// that name.
    pub fn render(&self) -> Result<String> {
        let mut out = String::from("schema_version = 1\n\n[instance]\n");
        out.push_str(&format!("id = {}\n", quote(&self.id)?));
        out.push_str(&format!("root = {}\n", quote(&self.root)?));
        out.push_str(&format!("init = {}\n", quote(&self.init)?));
        out.push_str(&format!(
            "args = {}\n",
            quote_list(&self.args, MAX_ARGS, MAX_ARG, "args")?
        ));
        out.push_str(&format!(
            "environment = {}\n",
            quote_list(&self.environment, MAX_ENV, MAX_ENV_ENTRY, "environment")?
        ));
        out.push_str("\n[resources]\n");
        out.push_str(&format!("vcpus = {}\n", self.vcpus));
        out.push_str(&format!("memory_mib = {}\n", self.memory_mib));
        out.push_str("\n[execution]\nprofile = \"balanced\"\nbackend = \"ptrace-native\"\n");
        // The workload runs as the domain user, not as virtual root (the
        // native runtime's default).
        out.push_str(&format!(
            "\n[access]\nuid = {uid}\ngid = {uid}\n",
            uid = crate::domain::UID
        ));
        out.push_str(&self.contract.render());
        if self.outbound.ports().len() > crate::domain::MAX_NETWORK_ENDPOINTS {
            bail!(
                "input preparation failure: NEEDS.network names {} ports, more than {}",
                self.outbound.ports().len(),
                crate::domain::MAX_NETWORK_ENDPOINTS
            );
        }
        out.push_str(&crate::needs::contract::Contract::render_lists(
            self.outbound.ports(),
        ));
        out.push_str("\n[storage]\nassignments = []\n");
        out.push_str("\n[devices]\nassignments = [\"null\", \"random\", \"urandom\"]\n");
        out.push_str(&self.outbound.network_section());
        out.push_str("\n[audit]\nenabled = true\n");
        out.push_str("\n[failure]\npolicy = \"stop\"\n");
        if out.len() > MAX_CONFIG {
            bail!(
                "input preparation failure: rendered config is {} B, exceeding §2 4096 B \
                 (shorten env/args; config is never truncated)",
                out.len(),
            );
        }
        Ok(out)
    }
}

/// TOML basic-string quoting matching the instance parser's escapes.
fn quote(value: &str) -> Result<String> {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for byte in value.bytes() {
        match byte {
            b'"' | b'\\' => {
                out.push('\\');
                out.push(byte as char);
            }
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            0..=31 | 127 => {
                bail!("input preparation failure: config value has control characters");
            }
            _ => out.push(byte as char),
        }
    }
    out.push('"');
    Ok(out)
}

fn quote_list(values: &[String], max_items: usize, max_item: usize, field: &str) -> Result<String> {
    if values.len() > max_items {
        bail!(
            "input preparation failure: {field} has {} entries, exceeding §2 {max_items}",
            values.len(),
        );
    }
    for value in values {
        if value.len() > max_item {
            bail!(
                "input preparation failure: {field} entry is {} B, exceeding §2 {max_item} B",
                value.len(),
            );
        }
    }
    let mut out = String::from("[");
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            out.push_str(", ");
        }
        out.push_str(&quote(value)?);
    }
    out.push(']');
    Ok(out)
}
