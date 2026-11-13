//! Declared behaviour versions: how a saved-state identity names the tools
//! that execute a recipe, instead of naming them by their bytes.
//!
//! A saved state (`crate::retained`) is reused only when nothing that can
//! change what a recipe produces has changed. Its identity names by content
//! the recipe closure, the inputs, the defines, the execution options and the
//! root image. The two tools that execute the recipe, Recipe itself (as the
//! coordinator and as the worker inside the root) and the Ginux launcher,
//! change daily during development without changing what any recipe
//! produces. By default the identity therefore names them by the declared
//! versions below. Exact identity (`--exact-identity`, [`EXACT_ENV`],
//! `ExecutionOptions::exact_identity`) also names the worker's and the
//! launcher's bytes, so only byte-identical tools reuse a state; release
//! builds use it.
//!
//! The OS builder puts [`declared`] into its part keys for the same reason.
//!
//! A declared version that is not bumped when it should be makes a cache
//! reuse work that a changed tool would have done differently: the result is
//! a silently wrong output. When in doubt, bump.

use anyhow::{Result, bail};
use serde::Serialize;

/// What Recipe does to what a recipe produces.
///
/// Bump it in the same commit as any Recipe change that can alter the files,
/// metadata or `ctx.state` any recipe produces from the same inputs:
/// - a helper's output or semantics (what it writes, where, with which
///   modes, owners, links or timestamps; what it returns or accepts);
/// - the target metadata format, staging, publication or export handling;
/// - how inputs, dependency products, retained snapshots or the seeded target
///   are materialized for the worker, or how outputs are taken from it;
/// - the rendered Ginux instance configuration, the worker's environment or
///   the paths the worker sees;
/// - the Rhai engine setup, or a dependency upgrade that can change any of the
///   above (Rhai, the archive and compression crates, hashing).
///
/// A change that produces the same files, metadata and state, only faster or
/// with different diagnostics (such as a faster extractor with the same
/// result), does not bump it. The coordinator/worker transport version is
/// named separately (`PROTOCOL_VERSION`).
pub const ENGINE_BEHAVIOUR: u32 = 1;

/// What of Ginux a workload can observe, as Recipe, the consumer, judges it.
///
/// The catalog and the build-base contract digests Ginux reports are already
/// part of every identity ([`declared`] and each state's contract), and Recipe
/// refuses a launcher whose catalog or digest differs from its pins. This
/// number covers what they do not. Bump it when a Ginux change (including
/// moving the `tools/ginux` pin) can alter what a workload observes or what
/// Recipe receives back:
/// - syscall results, errno values, or which calls succeed;
/// - VFS behaviour: permissions, ownership, virtual credentials, timestamps,
///   links, extended attributes, directory listing order, path resolution;
/// - process, signal, clock, environment or resource-limit behaviour a
///   workload can see;
/// - the filesystem snapshot, export or restore format Recipe consumes.
///
/// A change that only alters speed, diagnostics, audit text or the launcher's
/// own robustness, with identical workload-visible results, does not bump it.
/// Kept in Recipe, not in Ginux: Ginux is upstream-owned, and what counts as
/// observable for cache reuse is this consumer's policy.
pub const GINUX_BEHAVIOUR: u32 = 1;

/// What a host run does differently from a Ginux run (`crate::execution`),
/// beyond [`ENGINE_BEHAVIOUR`]: the host directories that stand in for the
/// domain, the snapshot and export archives made from them, path confinement,
/// and the network client's own grant. It enters only the saved-state
/// identity of host runs, so Ginux runs and the OS builder's part keys
/// ([`declared`]) are unaffected. Bump it with any change to those.
pub const HOST_BEHAVIOUR: u32 = 1;

/// The environment variable that selects exact identity in the CLI: `1` on,
/// `0` or empty off; anything else is an error.
pub const EXACT_ENV: &str = "RECIPE_EXACT_IDENTITY";

/// Everything the default identity names about the executing tools.
#[derive(Debug, Clone, Serialize)]
pub struct Declared {
    pub engine: u32,
    pub ginux: u32,
    /// The coordinator/worker transport version.
    pub transport: u32,
    /// The Ginux catalog and the build-base contract digests Recipe pins and
    /// checks against what the launcher reports.
    pub catalog: u32,
    pub base_digest_offline: &'static str,
    pub base_digest_network: &'static str,
}

impl Declared {
    pub fn current() -> Self {
        Self {
            engine: ENGINE_BEHAVIOUR,
            ginux: GINUX_BEHAVIOUR,
            transport: crate::worker::protocol::PROTOCOL_VERSION,
            catalog: crate::needs::contract::CATALOG,
            base_digest_offline: crate::needs::contract::BASE_DIGEST_OFFLINE,
            base_digest_network: crate::needs::contract::BASE_DIGEST_NETWORK,
        }
    }
}

/// [`Declared::current`] as one line, for keys outside Recipe (the OS
/// builder's parts): every field, so a bump of any of them changes it.
pub fn declared() -> String {
    let d = Declared::current();
    format!(
        "recipe-engine-{} ginux-{} transport-{} catalog-{} base-{}-{}",
        d.engine, d.ginux, d.transport, d.catalog, d.base_digest_offline, d.base_digest_network
    )
}

/// Whether [`EXACT_ENV`] selects exact identity.
pub fn exact_from_env() -> Result<bool> {
    match std::env::var_os(EXACT_ENV) {
        None => Ok(false),
        Some(value) => match value.to_str() {
            Some("1") => Ok(true),
            Some("0" | "") => Ok(false),
            _ => bail!("{EXACT_ENV} must be 1, 0 or empty"),
        },
    }
}
