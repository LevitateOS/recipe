//! Invocation input preparation: stage everything a Ginux domain needs.
//!
//! Given a recipe path and an operation, [`prepare_invocation`] assembles the
//! supported invocation input set the worker resolves inside the domain:
//!
//! - the main recipe file,
//! - its `//! extends:` base recipe, resolved like the executor does,
//! - transitive Rhai `import "..."` modules, resolved against the recipe's
//!   directory first and the executor search path second,
//! - caller-supplied extra inputs (assets, declared source closure, caches),
//!
//! and renders the domain-facing artifacts launch consumes: an invocation id,
//! the allowlisted environment (interfaces §5), and the domain config text
//! (interfaces §2). Nothing staged embeds an absolute host checkout path or
//! a secret: host files are selected explicitly, copied under relative
//! `/inputs/NAME` domain paths, and referenced only by content hash.
//!
//! Launch, channel transport, retained storage and result export live in
//! `launch`, `retained` and `publication`. Literal dependency and module closures preserve
//! their relative directory layout. Unsupported input discovery fails with an explicit error
//! instead of silently staging the wrong bytes.
//!
//! Hard limits enforced (interfaces §2, no silent truncation):
//!
//! - invocation id ≤ 64 B, `[A-Za-z0-9_-]{1,64}`,
//! - `root` / `init` ≤ 128 B, `args` ≤ 7 × 128 B,
//! - `environment` ≤ 8 × 256 B, `storage` / `devices` ≤ 8 × 64 B,
//! - normalized config text ≤ 4096 B.

mod config;
mod discover;
mod env;
mod network;
mod root;

pub(crate) use root::{prepare_root, seed_target};

pub(crate) use config::DomainConfig;
pub(crate) use discover::{ExtraInput, InputKind, StagedInput, discover_inputs};
pub(crate) use env::{allowlisted_env, invocation_id, validate_invocation_id};

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};


/// Operations the worker entry point dispatches (interfaces §4, op 1).
const SUPPORTED_OPERATIONS: &[&str] = &[
    "install",
    "remove",
    "cleanup",
    "is_installed",
    "is_built",
    "is_acquired",
];

/// Staged invocation inputs; launch verifies and materializes the selected image
/// and worker into a fresh root before launching Ginux.
///
/// The staging directory is a fresh per-invocation temp dir holding one file
/// per regular input under an internal indexed name plus `config.toml`.
/// The temp dir lives as long as this value does.
#[derive(Debug)]
pub struct PreparedInvocation {
    /// `<recipe-name>-<unix-millis>-<pid>`, `[A-Za-z0-9_-]{1,64}` (§5).
    pub invocation_id: String,
    /// Canonical operation (`install`, `remove`, `cleanup`, `is_*`).
    pub operation: String,
    /// sha256 hex of the main recipe source (protocol `recipe_digest`).
    pub recipe_digest: String,
    /// Staged inputs sorted by domain name (recipe, base, imports, extras).
    pub inputs: Vec<StagedInput>,
    pub(crate) root_reference: String,
    pub(crate) network: network::NetworkPolicy,
    staging: crate::temporary::TemporaryDirectory,
}

impl PreparedInvocation {
    pub(crate) fn lease_fd(&self) -> std::os::fd::RawFd {
        self.staging.lease_fd()
    }
    /// Fresh per-invocation staging directory holding the input files and `config.toml`.
    pub fn staging_dir(&self) -> &Path {
        self.staging.path()
    }
}

/// Assemble the invocation input set for `recipe` + `operation`.
///
/// `search_path` mirrors the executor's module search path (recipe directory
/// is always tried first). `extra_inputs` are caller-declared assets, source
/// closure, and caches as `NAME=host-path` selections.
/// Preparation fails explicitly on unsupported discovery (absolute host
/// paths in `extends:`/`import`, missing/unreadable inputs, secret-bearing
/// or non-regular files) and on any §2 limit overflow.
pub fn prepare_invocation(
    recipe: &Path,
    operation: &str,
    search_path: Option<&Path>,
    extra_inputs: &[ExtraInput],
    options: &crate::ExecutionOptions,
) -> Result<PreparedInvocation> {
    options.validate()?;
    let operation = normalize_operation(operation)?;
    let discovered = discover_inputs(recipe, search_path, extra_inputs)?;

    let invocation_id = invocation_id(&discovered.recipe_stem)?;
    let env = allowlisted_env(&invocation_id)?;

    let network = network::NetworkPolicy::resolve(options)?;
    let config = DomainConfig::for_invocation(&invocation_id, &env, options)?
        .with_network(network.clone());
    let config_text = config.render()?;

    let staging = crate::temporary::TemporaryDirectory::new()
        .context("input preparation failure: cannot create staging dir")?;
    // Indexed private files cannot collide with source names or metadata.
    for (index, input) in discovered.inputs.iter().enumerate() {
        if input.kind != InputKind::File {
            continue;
        }
        let dest = staging.path().join(format!("input-{index}"));
        let mut source = crate::fsx::open_regular(&input.host_path)
            .with_context(|| format!("input changed kind: {}", input.name))?;
        let mut target = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)?;
        let (size, digest) = crate::fsx::copy_hashed(&mut source, &mut target, input.size)
            .with_context(|| format!("input changed size: {}", input.name))?;
        if size != input.size || digest != hex::encode(input.sha256) {
            bail!("input changed after discovery: {}", input.name);
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(input.mode))?;
    }
    std::fs::write(staging.path().join("config.toml"), &config_text)?;

    reject_host_leak(&config_text, &env, staging.path(), &discovered.host_dirs)?;

    Ok(PreparedInvocation {
        invocation_id,
        operation: operation.to_string(),
        recipe_digest: discovered.recipe_digest,
        inputs: discovered.inputs,
        root_reference: config.root_reference().to_owned(),
        network,
        staging,
    })
}

/// Accept the worker's operation spellings, normalizing CLI aliases.
fn normalize_operation(operation: &str) -> Result<&'static str> {
    match operation {
        "install" | "remove" | "cleanup" | "is_installed" | "is_built" | "is_acquired" => {
            Ok(SUPPORTED_OPERATIONS[SUPPORTED_OPERATIONS
                .iter()
                .position(|op| *op == operation)
                .expect("matched arm lists supported operations")])
        }
        "isinstalled" => Ok("is_installed"),
        "isbuilt" => Ok("is_built"),
        "isacquired" => Ok("is_acquired"),
        other => bail!(
            "input preparation failure: unknown operation {other:?}: \
             want install|remove|cleanup|is_installed|is_built|is_acquired"
        ),
    }
}


/// Last-line defense over actual path values, after the field owners validate
/// domain roots and allowlisted environment. Component prefixes distinguish
/// a host /tmp path from the legal domain path /work/tmp.
fn reject_host_leak(
    config_text: &str,
    env: &[(String, String)],
    staging: &Path,
    host_dirs: &[PathBuf],
) -> Result<()> {
    let config: toml::Value =
        toml::from_str(config_text).context("invalid rendered domain configuration")?;
    let staging = staging.to_string_lossy();
    let mut markers: Vec<String> = vec![staging.into_owned()];
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            markers.push(home);
        }
    }
    for dir in host_dirs {
        markers.push(dir.to_string_lossy().into_owned());
    }
    for marker in markers {
        if marker.len() < 2 {
            continue;
        }
        if config_has_host_path(&config, Path::new(&marker)) {
            bail!(
                "input preparation failure: rendered config leaks host path {marker:?} \
                 (host paths must never enter domain config)"
            );
        }
        for (name, value) in env {
            if value_has_host_path(value, Path::new(&marker)) {
                bail!(
                    "input preparation failure: env {name:?} leaks host path {marker:?} \
                     (only allowlisted domain values cross into the worker)"
                );
            }
        }
    }
    Ok(())
}

fn value_has_host_path(value: &str, marker: &Path) -> bool {
    let value = value
        .split_once('=')
        .map(|(_, value)| value)
        .unwrap_or(value);
    std::iter::once(value)
        .chain(value.split(':'))
        .any(|value| value.starts_with('/') && Path::new(value).starts_with(marker))
}
fn config_has_host_path(value: &toml::Value, marker: &Path) -> bool {
    match value {
        toml::Value::String(value) => value_has_host_path(value, marker),
        toml::Value::Array(values) => values
            .iter()
            .any(|value| config_has_host_path(value, marker)),
        toml::Value::Table(values) => values
            .values()
            .any(|value| config_has_host_path(value, marker)),
        _ => false,
    }
}
