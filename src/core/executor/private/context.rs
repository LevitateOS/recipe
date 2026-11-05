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

use anyhow::{Result, anyhow, bail};
use std::path::{Path, PathBuf};

use crate::core::executor::CompiledRecipe;
use crate::core::lock::{RecipeLock, acquire_recipe_lock};

/// One persistence point in the lifecycle (R03 structured checkpoint).
///
/// Names the lifecycle phase that just completed (`acquire`, `build`,
/// `install`, `remove`, `cleanup`) and the machine-readable reason for this
/// checkpoint: `phase:<phase>` for phase completions, the actual cleanup
/// reason (`auto.<phase>.success`, `manual`) for cleanup completions. The
/// worker reports these verbatim in its `checkpoint` frames, so the host
/// observes the actual checkpoint stream instead of matching on error-context
/// label strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CheckpointPoint {
    /// Lifecycle phase that just completed.
    pub phase: &'static str,
    /// Machine-readable persistence-point reason.
    pub reason: &'static str,
}

impl CheckpointPoint {
    /// Legacy error-context label for host file persistence
    /// (`"Failed to persist ctx after <phase>"`). Host error text stays
    /// byte-identical to before; only the worker transport uses the
    /// structured `phase`/`reason` directly.
    pub(crate) fn label(self) -> &'static str {
        match self.phase {
            "acquire" => "Failed to persist ctx after acquire",
            "build" => "Failed to persist ctx after build",
            "install" => "Failed to persist ctx after install",
            "remove" => "Failed to persist ctx after remove",
            "cleanup" => "Failed to persist ctx after cleanup",
            _ => "Failed to persist ctx",
        }
    }
}

/// Worker-side checkpoint sink (R03 structured transport seam).
///
/// Called at exactly the points where host execution would file-persist:
/// after successful acquire/build/install phases, their success cleanups,
/// removal, and manual cleanup. Receives the in-memory compiled recipe
/// (main/base sources and paths), the updated `ctx`, the structured
/// [`CheckpointPoint`] naming the persistence point, and the caller's persist
/// flag (`--no-persist-ctx` negated).
///
/// The sink fires even when persistence is disabled so the worker still
/// reports phase completion; the host honors `--no-persist-ctx` by skipping
/// the file write while still acknowledging the generation (see
/// `docs/ginux-recipe-interfaces.md` §4). The sink must never write recipe
/// sources itself; that stays with the host coordinator.
pub(crate) type CheckpointSink<'a> =
    dyn FnMut(&CompiledRecipe, &rhai::Map, CheckpointPoint, bool) -> Result<()> + 'a;

/// Where checkpointed `ctx` content goes.
pub(crate) enum PersistSink<'a> {
    /// Host default: atomically rewrite the trusted main/base recipe source.
    File,
    /// Worker: hand the checkpoint to the caller transport.
    Callback(&'a mut CheckpointSink<'a>),
}

/// Who owns the recipe lock for an operation.
pub(crate) enum LockOwnership {
    /// Host default: acquire the advisory recipe lock inside the operation.
    Acquire,
    /// Worker: the host coordinator already holds the lock.
    CallerHeld,
}

/// Internal execution context threading lock ownership and checkpoint
/// persistence through the engine flow.
pub(crate) struct ExecutionContext<'a> {
    lock_ownership: LockOwnership,
    persist_sink: PersistSink<'a>,
    work_root: Option<PathBuf>,
    active_recipes: Vec<PathBuf>,
}

impl<'a> ExecutionContext<'a> {
    /// Host default: acquire the lock per operation, persist to files.
    pub(crate) fn host() -> Self {
        Self {
            lock_ownership: LockOwnership::Acquire,
            persist_sink: PersistSink::File,
            work_root: None,
            active_recipes: Vec::new(),
        }
    }

    /// Worker entry point: the caller holds the lock and receives each
    /// checkpoint through `sink` instead of any source file being written.
    pub(crate) fn worker_stub(sink: &'a mut CheckpointSink<'a>) -> Self {
        Self {
            lock_ownership: LockOwnership::CallerHeld,
            persist_sink: PersistSink::Callback(sink),
            work_root: None,
            active_recipes: Vec::new(),
        }
    }

    pub(crate) fn prepare_work_root(&mut self, build_dir: &Path) {
        if self.work_root.is_none() {
            self.work_root = Some(build_dir.to_path_buf());
        }
    }

    pub(crate) fn tools_prefix(&self) -> Result<PathBuf> {
        Ok(self.work_root()?.join(".tools"))
    }

    pub(crate) fn work_root(&self) -> Result<&Path> {
        self.work_root
            .as_deref()
            .ok_or_else(|| anyhow!("missing recipe invocation work root"))
    }

    pub(crate) fn current_recipe(&self) -> Option<&Path> {
        self.active_recipes.last().map(PathBuf::as_path)
    }

    /// Detect cycles before locking or evaluating the next recipe. Every nested
    /// install shares this invocation's work root and tools destination.
    pub(crate) fn enter_recipe(&mut self, recipe: &Path, build_dir: &Path) -> Result<()> {
        if self.active_recipes.iter().any(|active| active == recipe) {
            let chain = self
                .active_recipes
                .iter()
                .map(|p| p.display().to_string())
                .chain(std::iter::once(recipe.display().to_string()))
                .collect::<Vec<_>>();
            bail!(
                "Circular recipe dependency detected: {}",
                chain.join(" -> ")
            );
        }
        if self.active_recipes.len() >= 64 {
            bail!("Recipe dependency depth exceeds 64 at {}", recipe.display());
        }
        self.prepare_work_root(build_dir);
        self.active_recipes.push(recipe.to_path_buf());
        Ok(())
    }

    pub(crate) fn leave_recipe(&mut self) {
        self.active_recipes.pop();
        if self.active_recipes.is_empty() {
            self.work_root = None;
        }
    }

    /// Transport v1 has only a main-recipe snapshot, not per-dependency identity.
    /// R07 must replace this seam before resumable dependency execution is allowed.
    pub(crate) fn require_dependency_policy(&self, persist_enabled: bool) -> Result<()> {
        if persist_enabled && matches!(self.persist_sink, PersistSink::Callback(_)) {
            bail!(
                "Worker transport v1 cannot checkpoint dependency state; resumable dependency execution is unavailable. Use --no-persist-ctx for explicitly non-resumable execution."
            );
        }
        Ok(())
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
        point: CheckpointPoint,
        persist_enabled: bool,
    ) -> Result<()> {
        if self.active_recipes.len() > 1 {
            // Never serialize a dependency's ctx as the parent's v1 snapshot or
            // rewrite an immutable dependency source. Dependency state transport
            // is separate from this non-resumable lifecycle implementation.
            self.require_dependency_policy(persist_enabled)?;
            return Ok(());
        }
        match &mut self.persist_sink {
            PersistSink::File => {
                if persist_enabled {
                    super::state::persist_ctx(compiled, ctx_map, point)
                } else {
                    Ok(())
                }
            }
            PersistSink::Callback(sink) => sink(compiled, ctx_map, point, persist_enabled),
        }
    }
}
