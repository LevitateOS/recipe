//! Rhai-based recipe engine
//!
//! Provides the execution environment for recipe scripts.

mod context;
mod lifecycle;
mod phases;
mod util;

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

        // Register acquire phase helpers
        engine.register_fn("download", phases::download);
        engine.register_fn("copy", phases::copy_files);
        engine.register_fn("verify_sha256", phases::verify_sha256);

        // Register build phase helpers
        engine.register_fn("extract", phases::extract);
        engine.register_fn("cd", phases::change_dir);
        engine.register_fn("run", phases::run_cmd);

        // Register install phase helpers
        engine.register_fn("install_bin", phases::install_bin);
        engine.register_fn("install_lib", phases::install_lib);
        engine.register_fn("install_man", phases::install_man);
        engine.register_fn("rpm_install", phases::rpm_install);

        // Register filesystem utilities
        engine.register_fn("exists", util::exists);
        engine.register_fn("file_exists", util::file_exists);
        engine.register_fn("dir_exists", util::dir_exists);
        engine.register_fn("mkdir", util::mkdir);
        engine.register_fn("rm", util::rm_files);
        engine.register_fn("mv", util::move_file);
        engine.register_fn("ln", util::symlink);
        engine.register_fn("chmod", util::chmod_file);

        // Register I/O utilities
        engine.register_fn("read_file", util::read_file);
        engine.register_fn("glob_list", util::glob_list);

        // Register environment utilities
        engine.register_fn("env", util::get_env);
        engine.register_fn("set_env", util::set_env);

        // Register command utilities
        engine.register_fn("run_output", util::run_output);
        engine.register_fn("run_status", util::run_status);

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

    /// Execute a recipe script
    ///
    /// Follows the package lifecycle:
    /// 1. is_installed() - Check if already done (skip if true)
    /// 2. acquire() - Get source materials
    /// 3. build() - Compile/transform (optional)
    /// 4. install() - Copy to PREFIX
    pub fn execute(&self, recipe_path: &Path) -> Result<()> {
        lifecycle::execute(&self.engine, &self.prefix, &self.build_dir, recipe_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_engine_creation() {
        let prefix = TempDir::new().unwrap();
        let build_dir = TempDir::new().unwrap();
        let engine = RecipeEngine::new(prefix.path().to_path_buf(), build_dir.path().to_path_buf());
        assert!(engine.recipes_path.is_none());
    }

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
