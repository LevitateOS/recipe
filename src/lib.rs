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
//! - `http_get(url)` - Fetch URL content as string
//!
//! ## Verification
//! - `verify_sha256(path, hash)` - Verify checksum
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

mod core;
pub mod helpers;

pub use core::output;

use anyhow::Result;
use rhai::{Engine, module_resolvers::FileModuleResolver};
use std::path::{Path, PathBuf};

/// Recipe execution engine
pub struct RecipeEngine {
    engine: Engine,
    build_dir: PathBuf,
    recipes_path: Option<PathBuf>,
    /// User-defined scope constants (injected via --define KEY=VALUE)
    defines: Vec<(String, String)>,
}

impl RecipeEngine {
    /// Create a new recipe engine
    pub fn new(build_dir: PathBuf) -> Self {
        let mut engine = Engine::new();
        helpers::register_all(&mut engine);

        Self {
            engine,
            build_dir,
            recipes_path: None,
            defines: Vec::new(),
        }
    }

    /// Set the recipes path for module resolution
    pub fn with_recipes_path(mut self, path: PathBuf) -> Self {
        let mut resolver = FileModuleResolver::new();
        resolver.set_base_path(&path);
        self.engine.set_module_resolver(resolver);
        self.recipes_path = Some(path);
        self
    }

    /// Add a user-defined scope constant.
    pub fn add_define(&mut self, key: String, value: String) {
        self.defines.push((key, value));
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
        core::executor::install(
            &self.engine,
            &self.build_dir,
            recipe_path,
            &self.defines,
            self.recipes_path.as_deref(),
        )
    }

    /// Remove an installed package
    ///
    /// Returns the final ctx map after removal.
    pub fn remove(&self, recipe_path: &Path) -> Result<rhai::Map> {
        core::executor::remove(&self.engine, recipe_path, self.recipes_path.as_deref())
    }

    /// Clean up build artifacts
    ///
    /// Returns the final ctx map after cleanup.
    pub fn cleanup(&self, recipe_path: &Path) -> Result<rhai::Map> {
        core::executor::cleanup(
            &self.engine,
            &self.build_dir,
            recipe_path,
            self.recipes_path.as_deref(),
        )
    }

    /// Get the recipes path
    pub fn recipes_path(&self) -> Option<&Path> {
        self.recipes_path.as_deref()
    }

    /// Get the underlying Rhai engine (for advanced use)
    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_engine_creation() {
        let build_dir = TempDir::new().unwrap();
        let engine = RecipeEngine::new(build_dir.path().to_path_buf());
        assert!(engine.recipes_path.is_none());
    }

    #[test]
    fn test_minimal_recipe() {
        let build_dir = TempDir::new().unwrap();
        let engine = RecipeEngine::new(build_dir.path().to_path_buf());

        let recipe_dir = TempDir::new().unwrap();
        let recipe_path = recipe_dir.path().join("test.rhai");
        std::fs::write(
            &recipe_path,
            r#"
let ctx = #{
    name: "test",
    installed: false,
};

fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}

fn acquire(ctx) { ctx }
fn install(ctx) {
    ctx.installed = true;
    ctx
}
"#,
        )
        .unwrap();

        let result = engine.execute(&recipe_path);
        assert!(result.is_ok(), "Failed: {:?}", result);

        // Verify ctx was persisted
        let content = std::fs::read_to_string(&recipe_path).unwrap();
        assert!(content.contains("installed: true"));
    }
}
