//! Rhai-based package recipe executor for LevitateOS
//!
//! Recipes are Rhai scripts that define how to acquire, build, and install packages.
//! The engine provides helper functions and executes the `acquire()`, `build()`, and
//! `install()` functions defined in each recipe.
//!
//! # Example Recipe
//!
//! ```rhai
//! let name = "bash";
//! let version = "5.2.26";
//! let deps = ["readline", "ncurses"];  // Optional dependencies
//!
//! fn acquire() {
//!     download("https://ftp.gnu.org/gnu/bash/bash-5.2.26.tar.gz");
//!     verify_sha256("abc123...");
//! }
//!
//! fn build() {
//!     extract("tar.gz");
//!     cd("bash-5.2.26");
//!     run(`./configure --prefix=${PREFIX}`);
//!     run(`make -j${NPROC}`);
//! }
//!
//! fn install() {
//!     run("make install");
//! }
//! ```
//!
//! # Dependencies
//!
//! Recipes can declare dependencies using `let deps = ["pkg1", "pkg2"]`.
//! Use `recipe install --deps <package>` to install dependencies automatically.
//! The `recipe deps <package>` command shows dependency information.
//!
//! # Engine-Provided Functions
//!
//! ## Acquire Phase
//! - `download(url)` - Download file from URL
//! - `copy(pattern)` - Copy files matching glob pattern
//! - `verify_sha256(hash)` - Verify last downloaded/copied file
//!
//! ## Build Phase
//! - `extract(format)` - Extract archive (tar.gz, tar.xz, tar.bz2, zip)
//! - `cd(dir)` - Change working directory
//! - `run(cmd)` - Execute shell command
//!
//! ## Install Phase
//! - `install_bin(pattern)` - Install to PREFIX/bin (0o755)
//! - `install_lib(pattern)` - Install to PREFIX/lib (0o644)
//! - `install_man(pattern)` - Install to PREFIX/share/man/man{N}
//!
//! For anything more complex (RPM extraction, custom paths), use `run()` directly.
//!
//! # Variables Available in Scripts
//!
//! - `PREFIX` - Installation prefix
//! - `BUILD_DIR` - Temporary build directory
//! - `ARCH` - Target architecture (x86_64, aarch64)
//! - `NPROC` - Number of CPUs
//! - `RPM_PATH` - Path to RPM repository (from environment)

mod core;
pub mod helpers;

pub use core::deps;
pub use core::lockfile;
pub use core::output;
pub use core::recipe_state;

use anyhow::Result;
use rhai::{module_resolvers::FileModuleResolver, Engine};
use std::path::{Path, PathBuf};

/// Recipe execution engine
pub struct RecipeEngine {
    engine: Engine,
    prefix: PathBuf,
    build_dir: PathBuf,
    recipes_path: Option<PathBuf>,
}

impl RecipeEngine {
    /// Create a new recipe engine
    pub fn new(prefix: PathBuf, build_dir: PathBuf) -> Self {
        let mut engine = Engine::new();

        // Register all helper functions
        helpers::register_all(&mut engine);

        Self {
            engine,
            prefix,
            build_dir,
            recipes_path: None,
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

    /// Execute a recipe script (install a package)
    ///
    /// Follows the package lifecycle:
    /// 1. is_installed() - Check if already done (skip if true)
    /// 2. acquire() - Get source materials
    /// 3. build() - Compile/transform (optional)
    /// 4. install() - Copy to PREFIX
    pub fn execute(&self, recipe_path: &Path) -> Result<()> {
        core::lifecycle::execute(&self.engine, &self.prefix, &self.build_dir, recipe_path)
    }

    /// Remove an installed package
    pub fn remove(&self, recipe_path: &Path) -> Result<()> {
        core::lifecycle::remove(&self.engine, &self.prefix, recipe_path)
    }

    /// Check for updates to a package
    /// Returns Some(new_version) if update available
    pub fn update(&self, recipe_path: &Path) -> Result<Option<String>> {
        core::lifecycle::update(&self.engine, recipe_path)
    }

    /// Upgrade a package (reinstall if newer version in recipe)
    /// Returns true if upgrade was performed
    pub fn upgrade(&self, recipe_path: &Path) -> Result<bool> {
        core::lifecycle::upgrade(&self.engine, &self.prefix, &self.build_dir, recipe_path)
    }

    /// Get the prefix path
    pub fn prefix(&self) -> &Path {
        &self.prefix
    }

    /// Get the recipes path
    pub fn recipes_path(&self) -> Option<&Path> {
        self.recipes_path.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cheat_test::{cheat_aware, cheat_reviewed};
    use tempfile::TempDir;

    #[cheat_reviewed("API test - engine creation with paths")]
    #[test]
    fn test_engine_creation() {
        let prefix = TempDir::new().unwrap();
        let build_dir = TempDir::new().unwrap();
        let engine = RecipeEngine::new(prefix.path().to_path_buf(), build_dir.path().to_path_buf());
        assert!(engine.recipes_path.is_none());
    }

    #[cheat_aware(
        protects = "User can execute minimal valid recipe",
        severity = "HIGH",
        ease = "MEDIUM",
        cheats = [
            "Skip recipe execution entirely",
            "Return success without running functions",
            "Ignore validation errors"
        ],
        consequence = "User's recipe appears to succeed but nothing is installed"
    )]
    #[test]
    fn test_empty_recipe() {
        let prefix = TempDir::new().unwrap();
        let build_dir = TempDir::new().unwrap();
        let engine = RecipeEngine::new(prefix.path().to_path_buf(), build_dir.path().to_path_buf());

        let recipe_dir = TempDir::new().unwrap();
        let recipe_path = recipe_dir.path().join("test.rhai");
        std::fs::write(
            &recipe_path,
            r#"
            let name = "test";
            let version = "1.0.0";
            let installed = false;

            fn acquire() {}
            fn build() {}
            fn install() {}
        "#,
        )
        .unwrap();

        let result = engine.execute(&recipe_path);
        assert!(result.is_ok(), "Failed: {:?}", result);
    }
}
