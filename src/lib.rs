//! Managed Rhai package recipes executed through the configured Ginux launcher.
//!
//! Every recipe starts with `const RECIPE_API = 1;` and states its facts in
//! literal declarations (`NEEDS`, `PACKAGE`, `SOURCES`, `FILES`, `PACKAGES`,
//! ...), read without running it ([`declarations`], [`needs`]); the format is
//! docs/recipe-format.md.
//!
//! A run: the coordinator ([`RecipeEngine`]) runs each dependency first, in
//! its own run, then starts a worker for the recipe, in a Ginux domain or on
//! the host. The worker builds the engine's constants (`PREFIX`, `JOBS`,
//! `HOST_LIBC`, `PACKAGE_FORMAT`, `INPUTS`, `DEFINES`, `DEPS`, `WORK`), runs
//! the download step, places the outputs (`/out/NAME`, chosen by the engine),
//! calls `build(src, out)` or `configure(tree)`, then `verify(out)` and
//! `cleanup(reason)`, and returns where it placed each output. The
//! coordinator validates the outputs, publishes them and returns their host
//! paths in the result's `outputs`; a failed run publishes nothing.
//!
//! Explicit `RecipeEngine::add_input` selections are imported read-only under
//! `/inputs` (`INPUTS.NAME`). Build material goes to `WORK`, dependency tools
//! to `/work/tools`. A named `target` input seeds the one output on fresh
//! state. Recipe code never sees coordinator host paths.
//!
//! Only the saved values (`save`, `checkpoint`) and their matching filesystem
//! snapshot are kept for an interrupted run; a saved value never proves that
//! work is done.
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
/// The literal declarations of a recipe, read without running it.
pub mod declarations;
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
/// The pins file `NAME.pins` beside a recipe with `PACKAGES`.
pub mod pins;
mod process;
mod progress_file;
mod publication;
mod recipes;
pub mod release;
mod retained;
/// Installed runtime defaults shared with OS packaging.
pub mod runtime;
pub mod sources;
/// The state of a program and the material Recipe keeps for it.
pub mod state;
pub mod system;
/// Target filesystem metadata carried independently of build-host privileges.
pub mod target;
mod temporary;
pub mod updates;
mod worker;

pub use backend::GinuxLauncher;
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
    pub use crate::worker::run_worker;

    /// The last event of a change, for a front end (`--machine-events`):
    /// `done`, or `failed` with the step that failed where the worker named
    /// one, and Recipe's own message as the reason.
    pub fn final_event(recipe: &str, result: &anyhow::Result<()>) -> serde_json::Value {
        match result {
            Ok(()) => serde_json::json!({"event": "done", "recipe": recipe}),
            Err(error) => {
                let step = error
                    .downcast_ref::<crate::backend::WorkloadFailure>()
                    .and_then(|failure| failure.0.phase)
                    .map(crate::lifecycle::Phase::name);
                let mut event = serde_json::json!({
                    "event": "failed",
                    "recipe": recipe,
                    "step": step,
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

/// What [`RecipeEngine::install_activating`] calls with the run's result once
/// the outputs are published, before the material is deleted: it activates
/// the package (and may change the result, adding `activation`). An error
/// fails the install and leaves the kept result for the next one.
pub type Activate<'a> = dyn Fn(&mut serde_json::Value) -> Result<()> + 'a;

/// Recipe execution engine
pub struct RecipeEngine {
    build_dir: PathBuf,
    recipes_path: Option<PathBuf>,
    /// User-defined scope constants (injected via --define KEY=VALUE)
    defines: Vec<(String, String)>,
    inputs: Vec<inputs::ExtraInput>,
    persist_state: bool,
    /// The keep option (docs/recipe-format.md, section 6.4): an install
    /// leaves the downloads and build material.
    keep: bool,
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
            keep: false,
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

    /// Keep the downloads and build material after a successful install
    /// (`--keep`). Default is `false`: the install ends with the cleanup step.
    pub fn with_keep(mut self, keep: bool) -> Self {
        self.keep = keep;
        self
    }

    /// Select target, job count and the other execution options.
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

    /// Install a recipe (docs/recipe-format.md, section 6.3): run its
    /// dependencies, then every step still missing by Recipe's records, and
    /// publish the outputs. A kept result with the current identity is
    /// published without running anything. Publishing the outputs is the
    /// install of a producer (decision 23); then, unless kept
    /// ([`Self::with_keep`]), the cleanup step deletes the downloads, the
    /// build material and the saved state of the recipe and of the
    /// dependency runs made for it.
    ///
    /// Returns the run's result: `operation`, `name`, `version`, `steps`,
    /// `outputs` (each output's published path), `prefix`, `dependencies`,
    /// `kept`, `freed_bytes`, `contract`, `network_usage`.
    pub fn install(&self, recipe_path: &Path) -> Result<serde_json::Value> {
        self.install_activating(recipe_path, None)
    }

    /// [`Self::install`], calling `activate` with the result once the
    /// outputs are published and before the cleanup step, so the material
    /// is deleted only after the program is activated.
    pub fn install_activating(
        &self,
        recipe_path: &Path,
        activate: Option<&Activate<'_>>,
    ) -> Result<serde_json::Value> {
        let finish = launch::Finish {
            keep: self.keep,
            activate,
        };
        self.invoke_with(
            worker::protocol::Operation::Install,
            recipe_path,
            Some(&finish),
        )
    }

    /// Build a recipe: its dependencies, the download step where it is
    /// missing and the build step, unless a kept result with the current
    /// identity exists. The result is kept, not published (`outputs` is
    /// empty).
    pub fn build(&self, recipe_path: &Path) -> Result<serde_json::Value> {
        self.invoke(worker::protocol::Operation::Build, recipe_path)
    }

    /// The download step only, unless Recipe keeps the downloads already.
    pub fn download(&self, recipe_path: &Path) -> Result<serde_json::Value> {
        self.invoke(worker::protocol::Operation::Download, recipe_path)
    }

    /// Run the recipe's own `cleanup("manual")` on fresh work folders. The
    /// material `recipe cleanup` deletes is deleted by the caller first
    /// ([`state::cleanup`]).
    pub fn cleanup(&self, recipe_path: &Path) -> Result<serde_json::Value> {
        self.invoke(worker::protocol::Operation::Cleanup, recipe_path)
    }

    /// The destination: where this engine keeps its lock and saved-state scope.
    pub fn destination(&self) -> &Path {
        &self.build_dir
    }

    /// Call the recipe's `latest()` on the host and return the version it
    /// gives (docs/recipe-format.md, section 2.9).
    /// The inputs the caller added are not staged and no input is required:
    /// latest() sees none. `recipe updates --upstream` calls this for a
    /// recipe that defines latest().
    pub fn latest(&self, recipe_path: &Path) -> Result<String> {
        let result = self.invoke(worker::protocol::Operation::Latest, recipe_path)?;
        result["latest"]
            .as_str()
            .map(str::to_owned)
            .context("latest() returned no version")
    }

    fn invoke(
        &self,
        operation: worker::protocol::Operation,
        recipe: &Path,
    ) -> Result<serde_json::Value> {
        self.invoke_with(operation, recipe, None)
    }

    fn invoke_with(
        &self,
        operation: worker::protocol::Operation,
        recipe: &Path,
        finish: Option<&launch::Finish<'_>>,
    ) -> Result<serde_json::Value> {
        let no_dependencies = Default::default();
        let no_inputs = Vec::new();
        backend::run(
            self.launcher.as_ref(),
            &launch::Invocation {
                operation,
                recipe,
                search_path: self.recipes_path.as_deref(),
                persist_state: self.persist_state,
                options: worker::protocol::InvocationOptions {
                    defines: self.defines.clone(),
                    execution: self.execution.clone(),
                },
                inputs: if operation == worker::protocol::Operation::Latest {
                    &no_inputs
                } else {
                    &self.inputs
                },
                destination: &self.build_dir,
                state_dir: self.state_dir.as_deref(),
                // Each run of the graph names its own (`backend`).
                dependencies: &no_dependencies,
                key: "",
                finish,
            },
        )
    }
}
