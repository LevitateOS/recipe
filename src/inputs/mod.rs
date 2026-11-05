//! Invocation input preparation (R05): stage everything a Ginux domain needs.
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
//! and renders the domain-facing artifacts R06 consumes: an invocation id,
//! the allowlisted environment (interfaces §5), and the domain config text
//! (interfaces §2). Nothing staged embeds an absolute host checkout path or
//! a secret: host files are selected explicitly, copied under relative
//! `/inputs/NAME` domain paths, and referenced only by content hash.
//!
//! What this does NOT do (owned by later tasks): domain launch and channel
//! transport (R06), retained storage backing and generations (G11/G12a),
//! export of results (G13/R08). Literal dependency and module closures preserve
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
mod root;

pub(crate) use root::prepare_root;

pub use config::{DomainConfig, config_text};
pub use discover::{
    ExtraInput, InputKind, MAIN_NAME_DOMAIN, StagedInput, discover_inputs, hash_file,
};
pub use env::{allowlisted_env, invocation_id, validate_invocation_id};

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Canonical tool destination shared by root preparation and the executor.
pub(crate) const TOOLS_DOMAIN_PATH: &str = "/work/tools";

/// Operations the worker entry point dispatches (interfaces §4, op 1).
const SUPPORTED_OPERATIONS: &[&str] = &[
    "install",
    "remove",
    "cleanup",
    "is_installed",
    "is_built",
    "is_acquired",
];

/// Staged invocation inputs; R06 verifies and materializes the selected image
/// and worker into a fresh root before launching Ginux.
///
/// The staging directory is a fresh per-invocation temp dir holding one file
/// per regular input under an internal indexed name, plus `manifest.txt` (G10-format
/// `INPUT:` lines: domain path, sha256, size, mode), `sources.txt`
/// (domain NAME `<-` referenced-as map), `config.toml`, and `env.txt`.
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
    /// Allowlisted domain environment (§5), `NAME=value` pairs.
    pub env: Vec<(String, String)>,
    /// Domain config text the `ginux-domain` parser accepts (≤ 4096 B).
    pub config_text: String,
    pub(crate) root_reference: String,
    staging: crate::temporary::TemporaryDirectory,
}

impl PreparedInvocation {
    pub(crate) fn lease_fd(&self) -> std::os::fd::RawFd {
        self.staging.lease_fd()
    }
    /// Fresh per-invocation staging directory holding the input files,
    /// `manifest.txt`, `config.toml`, and `env.txt`.
    pub fn staging_dir(&self) -> &Path {
        self.staging.path()
    }

    /// Domain path of the main recipe inside `/inputs`.
    pub fn recipe_domain_path(&self) -> &'static str {
        MAIN_NAME_DOMAIN
    }
}

/// Assemble the invocation input set for `recipe` + `operation`.
///
/// `search_path` mirrors the executor's module search path (recipe directory
/// is always tried first). `extra_inputs` are caller-declared assets, source
/// closure, and caches as `NAME=host-path` selections; `extra_env` are
/// caller-declared toolchain variables filtered against the allowlist.
/// Preparation fails explicitly on unsupported discovery (absolute host
/// paths in `extends:`/`import`, missing/unreadable inputs, secret-bearing
/// or non-regular files) and on any §2 limit overflow.
pub fn prepare_invocation(
    recipe: &Path,
    operation: &str,
    search_path: Option<&Path>,
    extra_inputs: &[ExtraInput],
    extra_env: &[(String, String)],
    options: &crate::ExecutionOptions,
) -> Result<PreparedInvocation> {
    options.validate()?;
    let operation = normalize_operation(operation)?;
    let discovered = discover_inputs(recipe, search_path, extra_inputs)?;

    let invocation_id = invocation_id(&discovered.recipe_stem)?;
    let mut env = allowlisted_env(&invocation_id, extra_env)?;
    for (name, value) in &mut env {
        match name.as_str() {
            "ARCH" => *value = options.target_arch.clone(),
            "NPROC" => *value = options.jobs.to_string(),
            _ => {}
        }
    }
    let config = DomainConfig::for_invocation(&invocation_id, MAIN_NAME_DOMAIN, &env, options)?;
    let config_text = config.render()?;

    let staging = crate::temporary::TemporaryDirectory::new()
        .context("input preparation failure: cannot create staging dir")?;
    // Indexed private files cannot collide with source names or metadata.
    for (index, input) in discovered.inputs.iter().enumerate() {
        if input.kind != InputKind::File {
            continue;
        }
        let dest = staging.path().join(format!("input-{index}"));
        use std::io::Read;
        use std::os::unix::fs::OpenOptionsExt;
        let mut source = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&input.host_path)?;
        if !source.metadata()?.is_file() {
            bail!("input changed kind: {}", input.name);
        }
        let mut target = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)?;
        if std::io::copy(&mut source.by_ref().take(input.size + 1), &mut target)? != input.size {
            bail!("input changed size: {}", input.name);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(input.mode))?;
        }
    }
    let manifest = render_manifest(&discovered.inputs);
    std::fs::write(staging.path().join("manifest.txt"), &manifest)?;
    let mut sources =
        String::from("# domain NAME <- referenced-as (R06/I02 layout reconstruction)\n");
    for input in &discovered.inputs {
        sources.push_str(&format!("{} <- {}\n", input.name, input.referenced_as));
    }
    std::fs::write(staging.path().join("sources.txt"), &sources)?;
    std::fs::write(staging.path().join("config.toml"), &config_text)?;
    std::fs::write(
        staging.path().join("env.txt"),
        env.iter()
            .map(|(name, value)| format!("{name}={value}\n"))
            .collect::<String>(),
    )?;

    reject_host_leak(&config_text, &env, staging.path(), &discovered.host_dirs)?;

    Ok(PreparedInvocation {
        invocation_id,
        operation: operation.to_string(),
        recipe_digest: discovered.recipe_digest,
        inputs: discovered.inputs,
        env,
        config_text,
        root_reference: config.root_reference().to_owned(),
        staging,
    })
}

/// Accept the worker's operation spellings, normalizing CLI aliases.
fn normalize_operation(operation: &str) -> Result<&'static str> {
    match operation {
        "install" | "remove" | "cleanup" | "is_installed" | "is_built" | "is_acquired" => {
            Ok(SUPPORTED_OPERATIONS
                .iter()
                .find(|op| **op == operation)
                .copied()
                .unwrap_or("install"))
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

/// G10-format manifest: domain paths plus content hashes, no host paths.
fn render_manifest(inputs: &[StagedInput]) -> String {
    let mut out = String::new();
    let mut total = 0u64;
    for input in inputs {
        total += input.size;
        out.push_str(&format!(
            "INPUT: path=/inputs/{} sha256={} size={} mode={:04o} kind={:?}\n",
            input.name,
            hex::encode(input.sha256),
            input.size,
            input.mode,
            input.kind,
        ));
    }
    out.push_str(&format!(
        "INPUTS: packaged={} total={total}\n",
        inputs.len()
    ));
    out
}

/// Last-line defense: the rendered config and env must never name a host
/// staging directory or a host recipe directory. They only carry domain
/// paths by construction; a match here is an explicit failure, not a scrub.
fn reject_host_leak(
    config_text: &str,
    env: &[(String, String)],
    staging: &Path,
    host_dirs: &[PathBuf],
) -> Result<()> {
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
        if config_text.contains(&marker as &str) {
            bail!(
                "input preparation failure: rendered config leaks host path {marker:?} \
                 (host paths must never enter domain config)"
            );
        }
        for (name, value) in env {
            if value.contains(&marker as &str) {
                bail!(
                    "input preparation failure: env {name:?} leaks host path {marker:?} \
                     (only allowlisted domain values cross into the worker)"
                );
            }
        }
    }
    Ok(())
}
