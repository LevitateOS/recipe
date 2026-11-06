//! Rhai-based package recipe executor for LevitateOS
//!
//! Recipes are Rhai scripts that use a `ctx` map for state. The engine provides
//! helper functions and executes phase functions defined in each recipe.
//!
//! # Recipe Pattern
//!
//! ```rhai
//! let ctx = #{
//!     name: "mypackage",
//!     version: "1.0",
//!     installed: false,
//! };
//!
//! fn is_acquired(ctx) { if ctx.source == "" { throw "not acquired"; } ctx }
//! fn is_built(ctx) { if !is_file(ctx.artifact) { throw "not built"; } ctx }
//! fn is_installed(ctx) { if !ctx.installed { throw "not installed"; } ctx }
//!
//! fn acquire(ctx) { ctx.source = download(...); ctx }
//! fn build(ctx) { run(...); ctx }
//! fn install(ctx) { ctx.installed = true; ctx }
//!
//! # Cleanup Lifecycle
//!
//! Recipes may optionally define:
//! - `fn cleanup(ctx, reason) { ...; ctx }`
//!
//! `reason` is an enum-like string:
//! - `manual`
//! - `auto.<phase>.<outcome>` (examples: `auto.acquire.success`, `auto.build.failure`)
//!
//! The intent is that cleanup is an engine-invoked hygiene hook (not an ad-hoc
//! manual step), and `recipe cleanup` is just a manual trigger with `reason=manual`.
//! ```
//!
//! # Phase Lifecycle
//!
//! 1. `is_installed()` - Check if already done (skip if doesn't throw)
//! 2. `is_built()` - Check if build needed
//! 3. `is_acquired()` - Check if acquire needed
//! 4. Execute needed phases (acquire, build, install)
//! 5. Persist ctx after each phase
//!
//! # Engine-Provided Functions
//!
//! ## Filesystem
//! - `is_file(path)` - Check if path is a file
//! - `is_dir(path)` - Check if path is a directory
//! - `mkdir(path)` - Create directory recursively
//! - `rm(path)` - Remove file or directory
//! - `mv(src, dst)` - Move/rename
//! - `chmod(path, mode)` - Change permissions
//!
//! ## Paths
//! - `join_path(a, b)` - Join path components
//! - `basename(path)` - Get filename
//!
//! ## Commands
//! - `shell(cmd)` - Run shell command, throw on failure
//! - `shell_status(cmd)` - Run command, return exit code
//!
//! ## Network
//! - `download(url, dest)` - HTTP download
//! - `download_with_resume(url, dest)` - HTTP download with Range resume (throws if resume can't be honored)
//! - `http_get(url)` - Fetch URL content as string
//! - `torrent(url, dest_dir)` - BitTorrent download into `dest_dir`
//!
//! ## Verification
//! - `verify_sha256(path, hash)` - Verify checksum
//! - `fetch_sha256(url, filename)` - Fetch a SHA256 from a remote checksum file
//! - `check_disk_space(path, bytes)` - Verify free space
//!
//! ## Archive
//! - `extract(archive, dest)` - Auto-detect format and extract
//!
//! ## File I/O
//! - `read_file(path)` - Read file as string
//! - `write_file(path, content)` - Write string to file
//!
//! ## String
//! - `trim(str)` - Remove whitespace
//!
//! ## Logging
//! - `log(msg)` - Print info message
//!
//! # Variables Available in Scripts
//!
//! - `RECIPE_DIR` - Directory containing the recipe file
//! - `BUILD_DIR` - Temporary build directory
//! - `ARCH` - Target architecture (x86_64, aarch64)
//! - `NPROC` - Number of CPUs
//! - `RPM_PATH` - Path to RPM repository (from environment)

pub mod backend;
#[doc(hidden)]
pub mod cancellation;
mod core;
mod options;
pub use options::ExecutionOptions;
pub mod helpers;
pub mod inputs;
pub mod launch;
mod publication;
mod retained;
mod temporary;

/// Default coordinator-owned runtime state directory.
pub fn default_state_directory() -> anyhow::Result<std::path::PathBuf> {
    retained::default_directory()
}
mod llm;
pub mod logging;
pub mod worker;

pub use backend::GinuxLauncher;
pub use core::executor::{DeclaredMetadata, declared_metadata};

pub use core::output;
pub use logging::{
    RECIPE_HOOK_EVENT, RecipeHookEvent, RecipeHookSink, emit_hook_event, emit_hook_event_struct,
    make_machine_hook_event, set_event_sink, set_event_sink_handler, set_machine_events,
};

use anyhow::Result;
use std::path::{Path, PathBuf};

/// Configure Recipe's automatic LLM-based repair loop for failed installs.
///
/// When enabled, Recipe will (on selected failures) ask the configured LLM provider to return a
/// unified diff, apply it, and retry the install.
#[derive(Debug, Clone)]
pub struct AutoFixConfig {
    /// Maximum number of patch attempts per failing `recipe install`.
    pub attempts: u8,
    /// Working directory used for patch application and LLM invocation. If unset, Recipe will try
    /// to detect the git repo root and fall back to the recipe's directory.
    pub cwd: Option<PathBuf>,
    /// Optional extra instructions appended to the built-in autofix prompt.
    pub prompt_file: Option<PathBuf>,
    /// Allowed roots for patched file paths. If empty, defaults to the detected repo root (or
    /// `cwd`/recipe dir fallback).
    pub allow_paths: Vec<PathBuf>,
}

impl Default for AutoFixConfig {
    fn default() -> Self {
        Self {
            attempts: 2,
            cwd: None,
            prompt_file: None,
            allow_paths: Vec::new(),
        }
    }
}

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
    llm_profile: Option<String>,
    autofix: Option<AutoFixConfig>,
    /// Explicit launcher selection; None resolves mandatory Ginux from the
    /// environment when an operation starts.
    launcher: Option<GinuxLauncher>,
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
            llm_profile: None,
            autofix: None,
            launcher: None,
        }
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

    /// Select an LLM profile (from XDG `recipe/llm.toml`) for this engine execution.
    pub fn with_llm_profile(mut self, profile: Option<String>) -> Self {
        self.llm_profile = profile;
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

    /// Grant explicit outbound TCP hosts; the coordinator resolves and pins them before launch.
    pub fn with_network_hosts(mut self, hosts: Vec<String>) -> Self {
        self.execution.network_hosts = hosts;
        self
    }

    /// Select coordinator-owned runtime state storage.
    pub fn with_state_dir(mut self, path: Option<PathBuf>) -> Self {
        self.state_dir = path;
        self
    }

    /// Enable Recipe's automatic LLM-based repair loop for this engine execution.
    pub fn with_autofix(mut self, cfg: Option<AutoFixConfig>) -> Self {
        self.autofix = cfg;
        self
    }

    /// Add a user-defined scope constant.
    pub fn add_define(&mut self, key: String, value: String) {
        self.defines.push((key, value));
    }

    /// Import an explicitly selected file or source tree read-only under /inputs.
    /// Tree imports exclude root .git and target metadata; symlinks are rejected.
    pub fn add_input(&mut self, name: String, path: PathBuf) {
        self.inputs.push(inputs::ExtraInput { name, path });
    }

    /// Execute a recipe script (install a package)
    ///
    /// Follows the package lifecycle:
    /// 1. is_installed() - Check if already done (skip if doesn't throw)
    /// 2. is_built() - Check if build needed
    /// 3. is_acquired() - Check if acquire needed
    /// 4. Execute needed phases
    /// 5. Persist ctx after each phase
    ///
    /// Returns the final ctx map containing all recipe state.
    pub fn execute(&self, recipe_path: &Path) -> Result<rhai::Map> {
        self.launcher()?.run(
            "install",
            recipe_path,
            self.recipes_path.as_deref(),
            self.persist_state,
            self.invocation_options("manual")?,
            &self.inputs,
            &self.build_dir,
            self.state_dir.as_deref(),
        )
    }

    /// Remove an installed package
    ///
    /// Returns the final ctx map after removal.
    pub fn remove(&self, recipe_path: &Path) -> Result<rhai::Map> {
        self.launcher()?.run(
            "remove",
            recipe_path,
            self.recipes_path.as_deref(),
            self.persist_state,
            self.invocation_options("manual")?,
            &self.inputs,
            &self.build_dir,
            self.state_dir.as_deref(),
        )
    }

    /// Clean up build artifacts
    ///
    /// Returns the final ctx map after cleanup.
    pub fn cleanup(&self, recipe_path: &Path) -> Result<rhai::Map> {
        self.cleanup_with_reason(recipe_path, "manual")
    }

    /// Clean up build artifacts with an explicit reason.
    ///
    /// Returns the final ctx map after cleanup.
    pub fn cleanup_with_reason(&self, recipe_path: &Path, reason: &str) -> Result<rhai::Map> {
        self.launcher()?.run(
            "cleanup",
            recipe_path,
            self.recipes_path.as_deref(),
            self.persist_state,
            self.invocation_options(reason)?,
            &self.inputs,
            &self.build_dir,
            self.state_dir.as_deref(),
        )
    }

    /// Execute `is_installed(ctx)` manually.
    pub fn is_installed(&self, recipe_path: &Path) -> Result<rhai::Map> {
        self.launcher()?.run(
            "is_installed",
            recipe_path,
            self.recipes_path.as_deref(),
            self.persist_state,
            self.invocation_options("manual")?,
            &self.inputs,
            &self.build_dir,
            self.state_dir.as_deref(),
        )
    }

    /// Execute `is_built(ctx)` manually.
    pub fn is_built(&self, recipe_path: &Path) -> Result<rhai::Map> {
        self.launcher()?.run(
            "is_built",
            recipe_path,
            self.recipes_path.as_deref(),
            self.persist_state,
            self.invocation_options("manual")?,
            &self.inputs,
            &self.build_dir,
            self.state_dir.as_deref(),
        )
    }

    /// Execute `is_acquired(ctx)` manually.
    pub fn is_acquired(&self, recipe_path: &Path) -> Result<rhai::Map> {
        self.launcher()?.run(
            "is_acquired",
            recipe_path,
            self.recipes_path.as_deref(),
            self.persist_state,
            self.invocation_options("manual")?,
            &self.inputs,
            &self.build_dir,
            self.state_dir.as_deref(),
        )
    }

    fn invocation_options(
        &self,
        cleanup_reason: &str,
    ) -> Result<worker::protocol::InvocationOptions> {
        if self.autofix.is_some() || self.llm_profile.is_some() {
            anyhow::bail!(
                "managed execution does not yet support autofix or LLM profiles; no recipe code was run"
            );
        }
        Ok(worker::protocol::InvocationOptions {
            defines: self.defines.clone(),
            cleanup_reason: cleanup_reason.to_owned(),
            execution: self.execution.clone(),
        })
    }

    /// Get the recipes path
    pub fn recipes_path(&self) -> Option<&Path> {
        self.recipes_path.as_deref()
    }
}
