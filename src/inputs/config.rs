//! Instance config rendering within the §2 hard limits.
//!
//! The text mirrors the `ginux-instance` normalized shape (`schema_version`,
//! `[instance]`, `[resources]`, `[execution]`, `[contract]`, `[storage]`,
//! `[devices]`, `[network]`, `[audit]`, `[failure]`) so the existing parser
//! accepts it with no Ginux-side changes: `balanced`/`ptrace-native`,
//! explicitly granted networking, `stderr` audit, `stop` failure policy. Every bound is
//! enforced here with an explicit error; nothing is truncated.

use anyhow::{Result, bail};

/// §2 hard limits (bytes/items); enforced, never truncated.
const MAX_ARGS: usize = 7;
const MAX_ARG: usize = 128;
const MAX_ENV: usize = 8;
const MAX_ENV_ENTRY: usize = 256;
const MAX_CONFIG: usize = 4096;

/// Instance config for one invocation; [`InstanceConfig::render`] produces the
/// parser-accepted text.
#[derive(Debug, Clone)]
pub struct InstanceConfig {
    id: String,
    root: String,
    init: String,
    args: Vec<String>,
    environment: Vec<String>,
    vcpus: u64,
    memory_mib: u64,
    storage_max_objects: u64,
    storage_max_bytes: Option<u64>,
    network: super::network::NetworkPolicy,
    contract: crate::needs::contract::Contract,
    /// Offer Ginux archive import streams on owned storage
    /// (`[storage] archive_import`); only for a launcher that accepts the field
    /// ([`crate::needs::contract::archive_import`]).
    archive_import: bool,
}

impl InstanceConfig {
    /// Build the invocation config: id/root/init/args/env plus fixed
    /// profile selections. `env` is the §5 allowlisted environment; the worker
    /// init and argv come from [`crate::domain`].
    pub fn for_invocation(
        invocation_id: &str,
        env: &[(String, String)],
        options: &crate::ExecutionOptions,
        network: super::network::NetworkPolicy,
        archive_import: bool,
    ) -> Result<Self> {
        let root = crate::runtime::root_reference()?;
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
        let vcpus = u64::from(options.jobs);
        Ok(Self {
            id: invocation_id.to_string(),
            root,
            init: crate::domain::WORKER.to_string(),
            args: crate::domain::WORKER_ARGS.map(str::to_string).to_vec(),
            environment,
            vcpus,
            memory_mib: options.memory_mib,
            storage_max_objects: options.storage_max_objects,
            storage_max_bytes: options.storage_max_bytes,
            network,
            contract: crate::needs::contract::Contract::of(options),
            archive_import,
        })
    }

    pub(crate) fn root_reference(&self) -> &str {
        &self.root
    }

    /// Render the parser-accepted config text (≤ 4096 B). Archive import is
    /// an optimization: a config that fits only without its field is rendered
    /// without it, and the workload extracts member by member.
    pub fn render(&self) -> Result<String> {
        match self.render_with(self.archive_import) {
            Err(_) if self.archive_import => self.render_with(false),
            result => result,
        }
    }

    fn render_with(&self, archive_import: bool) -> Result<String> {
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
        out.push_str(&self.contract.render());
        // Only domain paths cross this interface. The launcher's memory backend
        // owns these bytes; no writable host directory is assigned.
        out.push_str(&format!(
            "\n[storage]\nassignments = [{}]\n",
            crate::domain::WRITABLE_DIRS
                .map(|dir| format!("\"memory:{dir}\""))
                .join(", ")
        ));
        out.push_str(&format!("max_objects = {}\n", self.storage_max_objects));
        if let Some(bytes) = self.storage_max_bytes {
            out.push_str(&format!("max_bytes = {bytes}\n"));
        }
        if archive_import {
            out.push_str("archive_import = true\n");
        }
        out.push_str("\n[devices]\nassignments = [\"null\"]\n");
        if self.network.endpoints.is_empty() {
            out.push_str("\n[network]\nbackend = \"none\"\n");
        } else {
            out.push_str("\n[network]\nbackend = \"mediated\"\n");
            out.push_str(&format!(
                "endpoints = {}\nmax_connections = {}\nmax_bytes = {}\n",
                quote_list(
                    &self.network.endpoints,
                    crate::domain::MAX_NETWORK_ENDPOINTS,
                    64,
                    "network endpoints"
                )?,
                crate::domain::MAX_NETWORK_CONNECTIONS,
                self.network.max_bytes
            ));
        }
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
