//! Domain config rendering within the §2 hard limits.
//!
//! The text mirrors the `ginux-domain` normalized shape (`schema_version`,
//! `[domain]`, `[resources]`, `[execution]`, `[contract]`, `[storage]`,
//! `[devices]`, `[network]`, `[audit]`, `[failure]`) so the existing parser
//! accepts it with no Ginux-side changes: `balanced`/`ptrace-native`,
//! explicitly granted networking, `stderr` audit, `stop` failure policy. Every bound is
//! enforced here with an explicit error; nothing is truncated.

use anyhow::{Result, bail};

use super::env;

/// §2 hard limits (bytes/items); enforced, never truncated.
const MAX_ID: usize = 64;
const MAX_PATH: usize = 128;
const MAX_ARGS: usize = 7;
const MAX_ARG: usize = 128;
const MAX_ENV: usize = 8;
const MAX_ENV_ENTRY: usize = 256;
const MAX_CONFIG: usize = 4096;

/// Default immutable image reference: the I01-pinned Rocky rootfs content,
/// named by digest rather than by host path (host `.artifacts` paths never
/// enter domain config). Override with `GINUX_ROOT`; the override must pass
/// the same reference validation. How the launcher resolves this reference
/// to retained image storage is owned by G-launch/G11.
const DEFAULT_ROOT_REF: &str = "oci:rocky-10.2.20260525.0-amd64-rootfs@sha256:1026f8d3b36eaea1bcab8e609045991f08c70eb7645cb37f58f48faa8b40be9c";

/// Domain init: the worker binary shipped inside `/inputs` (I01/R02).
const WORKER_INIT: &str = "/inputs/recipe-worker";

/// Domain config for one invocation; [`DomainConfig::render`] produces the
/// parser-accepted text.
#[derive(Debug, Clone)]
pub struct DomainConfig {
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
}

impl DomainConfig {
    /// Build the invocation config: id/root/init/args/env plus fixed
    /// profile selections. `main_domain_path` is the staged main recipe
    /// (`/inputs/main.rhai`); `env` is the §5 allowlisted environment.
    ///
    /// `args` carries argv *after* argv[0]: the launcher supplies argv[0]
    /// from `init` (standard execve convention); R06/G-launch confirm the
    /// split when wiring launch.
    pub fn for_invocation(
        invocation_id: &str,
        main_domain_path: &str,
        env: &[(String, String)],
        options: &crate::ExecutionOptions,
    ) -> Result<Self> {
        options.validate()?;
        validate_id(invocation_id)?;
        let root = root_reference()?;
        validate_init(WORKER_INIT)?;
        validate_domain_path_value("args[0]", main_domain_path)?;
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
            init: WORKER_INIT.to_string(),
            args: vec![
                crate::worker::WORKER_ENTRY_ARG.to_string(),
                main_domain_path.to_string(),
            ],
            environment,
            vcpus,
            memory_mib: options.memory_mib,
            storage_max_objects: options.storage_max_objects,
            storage_max_bytes: options.storage_max_bytes,
            network: super::network::NetworkPolicy::default(),
        })
    }

    pub(crate) fn with_network(mut self, network: super::network::NetworkPolicy) -> Self {
        self.network = network;
        self
    }

    pub(crate) fn root_reference(&self) -> &str {
        &self.root
    }

    /// Render the parser-accepted config text (≤ 4096 B).
    pub fn render(&self) -> Result<String> {
        let mut out = String::from("schema_version = 1\n\n[domain]\n");
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
        out.push_str("\n[contract]\nprofile = \"balanced\"\n");
        // Only domain paths cross this interface. The launcher's memory backend
        // owns these bytes; no writable host directory is assigned.
        out.push_str("\n[storage]\nassignments = [\"memory:/work\", \"memory:/work/build\", \"memory:/work/tools\", \"memory:/work/tmp\", \"memory:/out\"]\n");
        out.push_str(&format!("max_objects = {}\n", self.storage_max_objects));
        if let Some(bytes) = self.storage_max_bytes {
            out.push_str(&format!("max_bytes = {bytes}\n"));
        }
        out.push_str("\n[devices]\nassignments = [\"null\"]\n");
        if self.network.endpoints.is_empty() {
            out.push_str("\n[network]\nbackend = \"none\"\n");
        } else {
            out.push_str("\n[network]\nbackend = \"mediated\"\n");
            out.push_str(&format!(
                "endpoints = {}\nmax_connections = 16\nmax_bytes = {}\n",
                quote_list(&self.network.endpoints, 8, 64, "network endpoints")?,
                self.network.max_bytes
            ));
        }
        out.push_str("\n[audit]\nmode = \"stderr\"\n");
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

/// Render the config text for `invocation_id` + `env` in one step.
pub fn config_text(
    invocation_id: &str,
    main_domain_path: &str,
    env: &[(String, String)],
    options: &crate::ExecutionOptions,
) -> Result<String> {
    DomainConfig::for_invocation(invocation_id, main_domain_path, env, options)?
        .with_network(super::network::NetworkPolicy::resolve(options)?)
        .render()
}

/// Image reference: I01 pin by default, `GINUX_ROOT` override when set.
/// Mirrors the domain parser's reference rule (no leading `/`, no `..`,
/// charset `[A-Za-z0-9_.:/@-]`), plus the §2 ≤ 128 B bound.
fn root_reference() -> Result<String> {
    let root = std::env::var("GINUX_ROOT").unwrap_or_else(|_| DEFAULT_ROOT_REF.to_string());
    if root.is_empty() || root.len() > MAX_PATH {
        bail!(
            "input preparation failure: domain root {root:?} has {} B, want 1..={MAX_PATH} B (§2)",
            root.len(),
        );
    }
    validate_reference(&root)?;
    Ok(root)
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > MAX_ID {
        bail!(
            "input preparation failure: domain id {id:?} has {} B, want 1..={MAX_ID} B (§2)",
            id.len(),
        );
    }
    env::validate_invocation_id(id)
}

/// Domain init must be a domain path (§2 ≤ 128 B; parser: starts with `/`,
/// no `..`, no NUL).
fn validate_init(init: &str) -> Result<()> {
    if init.len() < 2 || init.len() > MAX_PATH {
        bail!(
            "input preparation failure: domain init {init:?} has {} B, want 2..={MAX_PATH} B (§2)",
            init.len(),
        );
    }
    validate_domain_path_value("init", init)
}

fn validate_domain_path_value(field: &str, value: &str) -> Result<()> {
    if value.len() > MAX_ARG {
        bail!(
            "input preparation failure: {field} {value:?} has {} B, exceeding §2 128 B",
            value.len(),
        );
    }
    if !value.starts_with('/') || value.contains("..") || value.contains('\0') {
        bail!(
            "input preparation failure: {field} {value:?} is not a domain path \
             (want an absolute `/...` path with no `..`)"
        );
    }
    Ok(())
}

/// Mirror of the domain parser's reference rule for `root`/`storage`/`devices`.
fn validate_reference(value: &str) -> Result<()> {
    if value.starts_with('/')
        || value.contains("..")
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/' | b'@')
        })
    {
        bail!(
            "input preparation failure: domain reference {value:?} is not a reference \
             (no leading `/`, no `..`, charset `[A-Za-z0-9_.:/@-]`; host paths never enter config)"
        );
    }
    Ok(())
}

/// TOML basic-string quoting matching the domain parser's escapes.
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
