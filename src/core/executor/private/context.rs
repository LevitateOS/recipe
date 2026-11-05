//! Internal execution context (F04 seam).
//!
//! Separates two host-owned responsibilities from the engine flow so a future
//! worker process (R02) can execute the same `install_once` flow without
//! taking a second lock or rewriting authoritative recipe sources:
//!
//! - Recipe locking: the host coordinator owns the advisory recipe lock.
//!   In-process host execution acquires it per operation; a worker runs under
//!   the coordinator's already-held lock and must not acquire a second one.
//! - Checkpoint persistence: host execution atomically rewrites the trusted
//!   main/base recipe source via [`super::state::persist_ctx`]. A worker
//!   instead reports each checkpoint through a caller-provided sink (the R02
//!   transport); the host coordinator performs the file write and the
//!   acknowledgement association.
//!
//! The host default ([`ExecutionContext::host`]) preserves existing behavior
//! exactly: acquire the lock per operation, file-persist gated on the
//! caller's persist flag.

use anyhow::Result;
use std::path::Path;

use crate::core::executor::CompiledRecipe;
use crate::core::lock::{RecipeLock, acquire_recipe_lock};

/// Worker-side checkpoint sink (R02 stub transport seam).
///
/// Called at exactly the points where host execution would file-persist:
/// after successful acquire/build/install phases, their success cleanups,
/// removal, and manual cleanup. Receives the in-memory compiled recipe
/// (main/base sources and paths), the updated `ctx`, a static label naming
/// the persistence point, and the caller's persist flag (`--no-persist-ctx`
/// negated).
///
/// The sink fires even when persistence is disabled so the worker still
/// reports phase completion; the host honors `--no-persist-ctx` by skipping
/// the file write while still acknowledging the generation (see
/// `docs/ginux-recipe-interfaces.md` §4). The sink must never write recipe
/// sources itself; that stays with the host coordinator.
pub(crate) type CheckpointSink<'a> =
    dyn FnMut(&CompiledRecipe, &rhai::Map, &'static str, bool) -> Result<()> + 'a;

/// Where checkpointed `ctx` content goes.
pub(crate) enum PersistSink<'a> {
    /// Host default: atomically rewrite the trusted main/base recipe source.
    File,
    /// Worker stub (R02): hand the checkpoint to the caller transport.
    // No in-tree caller yet; R02 constructs this via `ExecutionContext::worker_stub`.
    #[allow(dead_code)]
    Callback(&'a mut CheckpointSink<'a>),
}

/// Who owns the recipe lock for an operation.
pub(crate) enum LockOwnership {
    /// Host default: acquire the advisory recipe lock inside the operation.
    Acquire,
    /// Worker stub: the host coordinator already holds the lock.
    // No in-tree caller yet; R02 constructs this via `ExecutionContext::worker_stub`.
    #[allow(dead_code)]
    CallerHeld,
}

/// Internal execution context threading lock ownership and checkpoint
/// persistence through the engine flow.
pub(crate) struct ExecutionContext<'a> {
    lock_ownership: LockOwnership,
    persist_sink: PersistSink<'a>,
}

impl<'a> ExecutionContext<'a> {
    /// Host default: acquire the lock per operation, persist to files.
    pub(crate) fn host() -> Self {
        Self {
            lock_ownership: LockOwnership::Acquire,
            persist_sink: PersistSink::File,
        }
    }

    /// Worker stub for R02: the caller holds the lock and receives each
    /// checkpoint through `sink` instead of any source file being written.
    // No in-tree caller yet; R02 is the intended caller.
    #[allow(dead_code)]
    pub(crate) fn worker_stub(sink: &'a mut CheckpointSink<'a>) -> Self {
        Self {
            lock_ownership: LockOwnership::CallerHeld,
            persist_sink: PersistSink::Callback(sink),
        }
    }

    /// Acquire the recipe lock when host-owned; no-op when caller-held.
    ///
    /// Returns the guard to hold for the operation, or `None` when the
    /// caller (host coordinator) already holds the lock.
    pub(crate) fn acquire_lock(&self, recipe_path: &Path) -> Result<Option<RecipeLock>> {
        match self.lock_ownership {
            LockOwnership::Acquire => Ok(Some(acquire_recipe_lock(recipe_path)?)),
            LockOwnership::CallerHeld => Ok(None),
        }
    }

    /// Persist (or report) `ctx` at one persistence point.
    ///
    /// Host file mode keeps the existing gating: skipped entirely when
    /// `persist_enabled` is false. Callback mode always invokes the sink so
    /// phase completion is still reported; the host decides whether the
    /// acknowledged generation also updates a source file.
    pub(crate) fn persist_checkpoint(
        &mut self,
        compiled: &mut CompiledRecipe,
        ctx_map: &rhai::Map,
        label: &'static str,
        persist_enabled: bool,
    ) -> Result<()> {
        match &mut self.persist_sink {
            PersistSink::File => {
                if persist_enabled {
                    super::state::persist_ctx(compiled, ctx_map, label)
                } else {
                    Ok(())
                }
            }
            PersistSink::Callback(sink) => sink(compiled, ctx_map, label, persist_enabled),
        }
    }
}
