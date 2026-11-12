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
//! The domain config's hard limits (interfaces §2) live in `config`; they are
//! enforced, never truncated.

mod config;
mod discover;
mod env;
mod image_cache;
mod network;
mod root;

pub(crate) use root::{VerifiedImage, prepare_root, seed_target};

pub(crate) use config::InstanceConfig;

/// Name of the staged domain config inside the staging directory.
const CONFIG_FILE: &str = "config.toml";
pub(crate) use discover::{ExtraInput, InputKind, StagedInput, discover_inputs, static_references};
pub(crate) use env::{allowlisted_env, invocation_id};

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

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
    /// Staged inputs sorted by domain name (recipe, base, imports, extras).
    pub inputs: Vec<StagedInput>,
    pub(crate) root_reference: String,
    pub(crate) network: network::NetworkPolicy,
    pub(crate) execution: crate::ExecutionOptions,
    pub(crate) declaration: crate::needs::Declaration,
    pub(crate) contract: crate::needs::contract::Receipt,
    staging: crate::temporary::TemporaryDirectory,
}

impl PreparedInvocation {
    pub(crate) fn lease_fd(&self) -> std::os::fd::RawFd {
        self.staging.lease_fd()
    }
    /// The staged domain config the launcher reads.
    pub(crate) fn config_path(&self) -> PathBuf {
        self.staging.path().join(CONFIG_FILE)
    }

    /// Fresh per-invocation staging directory holding the input files and `config.toml`.
    pub fn staging_dir(&self) -> &Path {
        self.staging.path()
    }
}

/// Assemble the invocation input set for `recipe`.
///
/// `search_path` mirrors the executor's module search path (recipe directory
/// is always tried first). `extra_inputs` are caller-declared assets, source
/// closure, and caches as `NAME=host-path` selections.
/// Preparation fails explicitly on unsupported discovery (absolute host
/// paths in `extends:`/`import`, missing/unreadable inputs, secret-bearing
/// or non-regular files) and on any §2 limit overflow.
pub fn prepare_invocation(
    recipe: &Path,
    search_path: Option<&Path>,
    extra_inputs: &[ExtraInput],
    options: &crate::ExecutionOptions,
    launcher: &Path,
    dependency_inputs: &[ExtraInput],
) -> Result<PreparedInvocation> {
    let graph = crate::needs::declarations(recipe, search_path)?;
    let declaration = graph.last().context("empty recipe graph")?.clone();
    declaration.needs.validate_inputs(
        &extra_inputs
            .iter()
            .map(|input| (input.name.clone(), input.path.clone()))
            .collect::<Vec<_>>(),
    )?;
    let execution = declaration.needs.resolve(&options.narrowing, options)?;
    let options = &execution;
    let mut staged_inputs = extra_inputs.to_vec();
    staged_inputs.extend_from_slice(dependency_inputs);
    let discovered = discover_inputs(recipe, search_path, &staged_inputs)?;

    for node in &graph {
        for (path, digest) in &node.source_digests {
            anyhow::ensure!(
                discovered
                    .inputs
                    .iter()
                    .any(
                        |input| input.host_path.canonicalize().ok().as_ref() == Some(path)
                            && hex::encode(input.sha256) == *digest
                    ),
                "recipe authority changed during input discovery: {}",
                path.display()
            );
        }
    }
    let invocation_id = invocation_id(&discovered.recipe_stem)?;
    let env = allowlisted_env(&invocation_id)?;

    let network = network::NetworkPolicy::resolve(options)?;
    let archive_import = crate::needs::contract::archive_import(launcher, options)?;
    let random_devices = crate::needs::contract::random_devices(launcher, options)?;
    let config = InstanceConfig::for_invocation(
        &invocation_id,
        &env,
        options,
        network.clone(),
        archive_import,
        random_devices,
    )?;
    let config_text = config.render()?;
    let contract = crate::needs::contract::validate(launcher, &config_text, options)?;

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
            .read(true)
            .write(true)
            .create_new(true)
            .open(&dest)?;
        // A copy-on-write clone where the filesystem can; either way the
        // digest checked is that of the staged bytes.
        let (size, digest) = crate::fsx::snapshot_hashed(&mut source, &mut target, input.size)
            .with_context(|| format!("input changed size: {}", input.name))?;
        if size != input.size || digest != hex::encode(input.sha256) {
            bail!("input changed after discovery: {}", input.name);
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(input.mode))?;
    }
    std::fs::write(staging.path().join(CONFIG_FILE), &config_text)?;

    reject_host_leak(&config_text, &env, staging.path(), &discovered.host_dirs)?;

    Ok(PreparedInvocation {
        invocation_id,
        inputs: discovered.inputs,
        root_reference: config.root_reference().to_owned(),
        network,
        execution,
        declaration,
        contract,
        staging,
    })
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
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        markers.push(home);
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
            if value_has_host_path(&format!("{name}={value}"), Path::new(&marker)) {
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
    // An imported tool's host parent can also name a fixed image directory.
    // Only the complete PATH generated by the environment owner is exempt;
    // other values and altered PATH entries still undergo the prefix check.
    if value
        .strip_prefix("PATH=")
        .is_some_and(|path| path == env::executable_path())
    {
        return false;
    }
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

/// Bind main inputs as NAME=PATH and dependency inputs as DEP::NAME=PATH.
/// A dependency alias must identify one recipe across the complete graph.
/// No input is implicitly lent to another recipe, even when names match.
pub(crate) fn bind_inputs(
    graph: &[crate::needs::Declaration],
    supplied: &[ExtraInput],
    search: Option<&Path>,
) -> Result<std::collections::BTreeMap<PathBuf, Vec<ExtraInput>>> {
    let main = graph.last().context("empty recipe graph")?;
    let mut aliases =
        std::collections::BTreeMap::<String, std::collections::BTreeSet<PathBuf>>::new();
    for node in graph {
        for dependency in &node.dependencies {
            aliases
                .entry(dependency.name.clone())
                .or_default()
                .insert(dependency.recipe.clone());
        }
    }
    let mut bindings: std::collections::BTreeMap<PathBuf, Vec<ExtraInput>> = graph
        .iter()
        .map(|node| (node.recipe.clone(), Vec::new()))
        .collect();
    for input in supplied {
        let (recipe, name) = if let Some((alias, name)) = input.name.split_once("::") {
            let recipes = aliases
                .get(alias)
                .with_context(|| format!("unknown dependency input scope {alias}"))?;
            ensure!(
                recipes.len() == 1,
                "ambiguous dependency alias {alias}; use distinct dependency names"
            );
            (recipes.first().unwrap(), name)
        } else {
            (&main.recipe, input.name.as_str())
        };
        bindings.get_mut(recipe).unwrap().push(ExtraInput {
            name: name.into(),
            path: input.path.clone(),
        });
    }
    for node in graph {
        node.needs
            .validate_inputs(
                &bindings[&node.recipe]
                    .iter()
                    .map(|input| (input.name.clone(), input.path.clone()))
                    .collect::<Vec<_>>(),
            )
            .with_context(|| format!("inputs for {}", node.recipe.display()))?;
        let discovered = discover_inputs(&node.recipe, search, &bindings[&node.recipe])
            .with_context(|| format!("input/source closure for {}", node.recipe.display()))?;
        for (source, digest) in &node.source_digests {
            ensure!(
                discovered
                    .inputs
                    .iter()
                    .any(
                        |input| input.host_path.canonicalize().ok().as_ref() == Some(source)
                            && hex::encode(input.sha256) == *digest
                    ),
                "recipe authority changed during graph preflight: {}",
                source.display()
            );
        }
    }
    Ok(bindings)
}
