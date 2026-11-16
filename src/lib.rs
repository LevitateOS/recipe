//! Managed Rhai package recipes executed through the configured Ginux launcher.
//!
//! Every executable recipe declares `const RECIPE_API = 2;` (or `3`). Setup reads
//! immutable `RUNTIME`; hooks receive `ctx.runtime`, resumable `ctx.state` and
//! named `ctx.outputs`. Package facts are initialized from current recipe bytes.
//!
//! Checks return `ready(ctx)` or `needs_work(ctx, reason)`. Check errors fail
//! execution. Acquire/build/install/remove hooks return the updated context.
//! Custom readiness is rechecked after successful work; persisted booleans do
//! not replace filesystem or product validation.
//!
//! Explicit `RecipeEngine::add_input` selections are imported read-only under
//! `/inputs`. Owned build work uses `/work/build`, dependency tools `/work/tools`,
//! and the installed-target/product tree `/out`. A named `target` input seeds
//! `/out` on fresh state. Hooks never inspect coordinator host paths.
//!
//! Only `ctx.state` and its matching filesystem snapshot are checkpointed.
//! Publication validates selected files/directories before returning their host
//! paths in `ctx.outputs`; failed execution cannot publish incomplete work.
//!
//! Acquisition uses `download(url, exact_dest, sha256[, resume])` or pinned
//! `git_checkout(url, exact_dest, full_commit)`. Explicit invocation network
//! grants are required. Commands and helpers execute inside the domain.
//!
//! See README.md, WRITING_RECIPES.md and HELPERS_AUDIT.md for the current CLI,
//! helper signatures, input/output contracts, resource limits and recovery.

/// What a front end may show about a recipe, read without running it.
pub mod accounts;
mod backend;
mod cancellation;
pub mod catalog;
pub mod clibrary;
mod core;
mod diagnostics;
mod domain;
/// Where a recipe runs: in a Ginux domain or on the host (`--execution`).
pub mod execution;
mod failure;
#[doc(hidden)]
pub mod fsx;
mod ginux;
mod helpers;
mod host;
/// Declared behaviour versions that name Recipe and Ginux in cache identities.
pub mod identity;
#[doc(hidden)]
pub mod image;
mod inputs;
mod invocation_log;
mod launch;
mod lifecycle;
pub mod needs;
pub mod offline_cache;
mod options;
pub mod packages;
mod process;
mod progress_file;
mod publication;
mod recipes;
pub mod release;
mod retained;
/// Installed runtime defaults shared with OS packaging.
pub mod runtime;
pub mod sources;
pub mod system;
/// Target filesystem metadata carried independently of build-host privileges.
pub mod target;
mod temporary;
pub mod updates;
mod worker;

pub use backend::GinuxLauncher;
pub use core::executor::{DeclaredMetadata, declared_metadata};
pub use execution::ExecutionMode;
pub use options::ExecutionOptions;

/// The default destination (lock and saved-state scope) for `recipe` under
/// `state_dir` (or the per-user application state).
pub fn default_destination(
    state_dir: Option<&std::path::Path>,
    recipe: &std::path::Path,
) -> anyhow::Result<std::path::PathBuf> {
    retained::default_destination(&retained::state_root(state_dir)?, recipe)
}

/// Entry points used only by this crate's own binaries and integration tests.
#[doc(hidden)]
pub mod bin_support {
    pub use crate::cancellation::{install_cli_handlers, signal};
    pub use crate::core::output::{info, warning};
    pub use crate::domain::WORKER_ENTRY_ARG;
    pub use crate::helpers::internal::hash::{FileHashes, compute_all_hashes};
    pub use crate::invocation_log::retained as retained_invocation;
    pub use crate::worker::protocol::{MAX_DIAGNOSTIC_BYTES, MAX_DIAGNOSTIC_FRAMES};
    /// The cleanup reason when the user gives none.
    pub const DEFAULT_CLEANUP_REASON: &str = crate::lifecycle::CleanupReason::Manual.as_str();
    pub use crate::worker::run_worker;

    /// The last event of a change, for a front end (`--machine-events`):
    /// `done`, or `failed` with the step that failed where the worker named
    /// one, and Recipe's own message as the reason.
    pub fn final_event(recipe: &str, result: &anyhow::Result<()>) -> serde_json::Value {
        match result {
            Ok(()) => serde_json::json!({"event": "done", "recipe": recipe}),
            Err(error) => {
                let hook = error
                    .downcast_ref::<crate::backend::WorkloadFailure>()
                    .and_then(|failure| failure.0.phase)
                    .map(crate::lifecycle::Phase::name);
                let mut event = serde_json::json!({
                    "event": "failed",
                    "recipe": recipe,
                    "hook": hook,
                    "reason": format!("{error:#}"),
                });
                // A connection Ginux refused during the run, if its log holds one.
                let refused = error
                    .downcast_ref::<crate::backend::WorkloadFailure>()
                    .and_then(|failure| failure.1.as_deref())
                    .and_then(crate::needs::report::refused_connection);
                if let Some(refused) = refused {
                    event["refused"] = refused;
                }
                event
            }
        }
    }
}

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Recipe execution engine
pub struct RecipeEngine {
    build_dir: PathBuf,
    recipes_path: Option<PathBuf>,
    /// User-defined scope constants (injected via --define KEY=VALUE)
    defines: Vec<(String, String)>,
    inputs: Vec<inputs::ExtraInput>,
    persist_state: bool,
    execution: ExecutionOptions,
    state_dir: Option<PathBuf>,
    /// Explicit launcher selection; None resolves Ginux from the environment
    /// when a recipe of the operation runs in Ginux (`crate::execution`).
    launcher: Option<GinuxLauncher>,
    /// Owns `build_dir` when it is a throwaway destination; removed on drop.
    _temporary: Option<tempfile::TempDir>,
}

impl RecipeEngine {
    /// Create a new recipe engine
    pub fn new(build_dir: PathBuf) -> Self {
        Self {
            build_dir,
            recipes_path: None,
            defines: Vec::new(),
            inputs: Vec::new(),
            persist_state: true,
            execution: ExecutionOptions::default(),
            state_dir: None,
            launcher: None,
            _temporary: None,
        }
    }

    /// An engine whose destination is a fresh directory removed with the engine,
    /// for invocations that keep no state.
    pub fn with_temporary_destination() -> Result<Self> {
        let temporary = tempfile::tempdir().context("cannot create a temporary destination")?;
        let mut engine = Self::new(temporary.path().to_owned());
        engine._temporary = Some(temporary);
        Ok(engine)
    }

    /// Configure the required Ginux executable.
    pub fn with_launcher(mut self, launcher: GinuxLauncher) -> Self {
        self.launcher = Some(launcher);
        self
    }

    /// Resolve the Ginux executable before executing recipe code.
    pub fn launcher(&self) -> Result<GinuxLauncher> {
        self.launcher
            .clone()
            .map(Ok)
            .unwrap_or_else(GinuxLauncher::from_env)
    }

    /// Set the recipes path for module resolution
    pub fn with_recipes_path(mut self, path: PathBuf) -> Self {
        self.recipes_path = Some(path);
        self
    }

    /// Configure whether runtime state and matching files are retained.
    ///
    /// Default is `true`.
    pub fn with_state_persistence(mut self, persist: bool) -> Self {
        self.persist_state = persist;
        self
    }

    /// Select target, job count and main-recipe force policy.
    pub fn with_execution_options(mut self, options: ExecutionOptions) -> Self {
        self.execution = options;
        self
    }

    /// Select coordinator-owned runtime state storage.
    pub fn with_state_dir(mut self, path: Option<PathBuf>) -> Self {
        self.state_dir = path;
        self
    }

    /// Add a user-defined scope constant.
    pub fn add_define(&mut self, key: String, value: String) {
        self.defines.push((key, value));
    }

    /// Import an explicitly selected file or source tree read-only under /inputs.
    /// Source imports exclude root .git/target metadata; contained links are preserved.
    /// Named rootfs/target trees preserve filesystem links and permissions.
    pub fn add_input(&mut self, name: String, path: PathBuf) {
        self.inputs.push(inputs::ExtraInput { name, path });
    }

    /// Execute a recipe script (install a package)
    ///
    /// Follows the package lifecycle:
    /// 1. is_installed() - Skip only on explicit ready(ctx).
    /// 2. is_built() - Check if build needed
    /// 3. is_acquired() - Check if acquire needed
    /// 4. Execute needed phases
    /// 5. Checkpoint ctx.state together with matching owned files.
    ///
    /// Returns the final ctx map containing all recipe state.
    pub fn execute(&self, recipe_path: &Path) -> Result<serde_json::Value> {
        self.invoke(worker::protocol::Operation::Install, None, recipe_path)
    }

    /// Remove an installed package
    ///
    /// Returns the final ctx map after removal.
    pub fn remove(&self, recipe_path: &Path) -> Result<serde_json::Value> {
        self.invoke(worker::protocol::Operation::Remove, None, recipe_path)
    }

    /// Clean up build artifacts with an explicit reason.
    ///
    /// Returns the final ctx map after cleanup.
    pub fn cleanup_with_reason(
        &self,
        recipe_path: &Path,
        reason: &str,
    ) -> Result<serde_json::Value> {
        self.invoke(
            worker::protocol::Operation::Cleanup,
            Some(reason),
            recipe_path,
        )
    }

    /// Execute `is_installed(ctx)` manually.
    pub fn is_installed(&self, recipe_path: &Path) -> Result<serde_json::Value> {
        self.invoke(worker::protocol::Operation::IsInstalled, None, recipe_path)
    }

    /// Execute `is_built(ctx)` manually.
    pub fn is_built(&self, recipe_path: &Path) -> Result<serde_json::Value> {
        self.invoke(worker::protocol::Operation::IsBuilt, None, recipe_path)
    }

    /// Execute `is_acquired(ctx)` manually.
    pub fn is_acquired(&self, recipe_path: &Path) -> Result<serde_json::Value> {
        self.invoke(worker::protocol::Operation::IsAcquired, None, recipe_path)
    }

    fn invoke(
        &self,
        operation: worker::protocol::Operation,
        cleanup_reason: Option<&str>,
        recipe: &Path,
    ) -> Result<serde_json::Value> {
        backend::run(
            self.launcher.as_ref(),
            &launch::Invocation {
                operation,
                cleanup_reason,
                recipe,
                search_path: self.recipes_path.as_deref(),
                persist_state: self.persist_state,
                options: worker::protocol::InvocationOptions {
                    defines: self.defines.clone(),
                    execution: self.execution.clone(),
                },
                inputs: &self.inputs,
                destination: &self.build_dir,
                state_dir: self.state_dir.as_deref(),
            },
        )
    }
}
