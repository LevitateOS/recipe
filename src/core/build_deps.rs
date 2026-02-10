//! Build dependency resolution for recipes
//!
//! When a recipe declares `let build_deps = ["linux-deps"];`, the executor
//! resolves each dep by finding `{name}.rhai` in the recipes search path,
//! executing it to install tools into `BUILD_DIR/.tools/`, then prepending
//! `.tools/` bin dirs to PATH before the build phase.

use super::output;
use anyhow::{Context, Result, anyhow};
use rhai::{Engine, Scope};
use std::fs;
use std::path::{Path, PathBuf};

/// Resolves and installs build dependencies into a `.tools/` prefix.
pub struct BuildDepsResolver<'a> {
    engine: &'a Engine,
    build_dir: &'a Path,
    recipes_path: Option<&'a Path>,
    defines: &'a [(String, String)],
    execution_stack: Vec<String>,
}

impl<'a> BuildDepsResolver<'a> {
    pub fn new(
        engine: &'a Engine,
        build_dir: &'a Path,
        recipes_path: Option<&'a Path>,
        defines: &'a [(String, String)],
    ) -> Self {
        Self {
            engine,
            build_dir,
            recipes_path,
            defines,
            execution_stack: Vec::new(),
        }
    }

    /// Resolve and install all build deps, returning the `.tools/` prefix path.
    pub fn resolve_and_install(&mut self, deps: &[String]) -> Result<PathBuf> {
        let tools_prefix = self.build_dir.join(".tools");

        for dep in deps {
            if self.execution_stack.contains(dep) {
                anyhow::bail!(
                    "Circular build dependency detected: {} -> {}",
                    self.execution_stack.join(" -> "),
                    dep
                );
            }
            self.execution_stack.push(dep.clone());
            self.install_dep(dep, &tools_prefix)?;
            self.execution_stack.pop();
        }

        Ok(tools_prefix)
    }

    /// Find and execute a single dep recipe.
    fn install_dep(&self, name: &str, tools_prefix: &Path) -> Result<()> {
        let recipe_path = self.find_recipe(name)?;

        let (ast, _source, base_dir) =
            super::executor::compile_recipe(self.engine, &recipe_path, self.recipes_path)?;

        let recipe_dir = recipe_path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string());

        let dep_build_dir = self.build_dir.join(format!(".deps/{}", name));
        fs::create_dir_all(&dep_build_dir)
            .with_context(|| format!("Failed to create dep build dir for {}", name))?;

        let mut scope = Scope::new();
        scope.push_constant("RECIPE_DIR", recipe_dir);
        if let Some(ref bd) = base_dir {
            scope.push_constant("BASE_RECIPE_DIR", bd.to_string_lossy().to_string());
        }
        scope.push_constant("BUILD_DIR", dep_build_dir.to_string_lossy().to_string());
        scope.push_constant("TOOLS_PREFIX", tools_prefix.to_string_lossy().to_string());
        scope.push_constant("ARCH", std::env::consts::ARCH);
        scope.push_constant("NPROC", num_cpus::get() as i64);

        for (key, value) in self.defines {
            scope.push_constant(key.as_str(), value.clone());
        }

        // Run top-level to populate ctx
        self.engine
            .run_ast_with_scope(&mut scope, &ast)
            .map_err(|e| anyhow!("Failed to run build dep {}: {}", name, e))?;

        let ctx_map: rhai::Map = scope
            .get_value("ctx")
            .ok_or_else(|| anyhow!("Build dep {} missing ctx", name))?;

        // Check if already installed
        let needs_install = Self::check_throws(self.engine, &ast, &scope, "is_installed", &ctx_map);
        if !needs_install {
            output::skip(&format!("build-dep {} already satisfied", name));
            return Ok(());
        }

        output::action(&format!("Installing build-dep: {}", name));

        // Run acquire → install (simplified, no locking or persistence)
        let needs_acquire = Self::check_throws(self.engine, &ast, &scope, "is_acquired", &ctx_map);

        let mut ctx = ctx_map;
        if needs_acquire && Self::has_fn(&ast, "acquire") {
            output::sub_action("acquire");
            ctx = self
                .engine
                .call_fn::<rhai::Map>(&mut scope, &ast, "acquire", (ctx,))
                .map_err(|e| anyhow!("build-dep {} acquire failed: {}", name, e))?;
        }

        if Self::has_fn(&ast, "install") {
            output::sub_action("install");
            self.engine
                .call_fn::<rhai::Map>(&mut scope, &ast, "install", (ctx,))
                .map_err(|e| anyhow!("build-dep {} install failed: {}", name, e))?;
        }

        output::success(&format!("build-dep {} installed", name));
        Ok(())
    }

    /// Find a recipe file by name in the search path.
    fn find_recipe(&self, name: &str) -> Result<PathBuf> {
        let filename = format!("{}.rhai", name);

        if let Some(sp) = self.recipes_path {
            let candidate = sp.join(&filename);
            if candidate.exists() {
                return Ok(candidate);
            }
        }

        anyhow::bail!(
            "Build dep recipe '{}' not found (search_path: {:?})",
            filename,
            self.recipes_path
        )
    }

    fn check_throws(
        engine: &Engine,
        ast: &rhai::AST,
        scope: &Scope,
        fn_name: &str,
        ctx: &rhai::Map,
    ) -> bool {
        if !Self::has_fn(ast, fn_name) {
            return true;
        }
        engine
            .call_fn::<rhai::Map>(&mut scope.clone(), ast, fn_name, (ctx.clone(),))
            .is_err()
    }

    fn has_fn(ast: &rhai::AST, name: &str) -> bool {
        ast.iter_functions().any(|f| f.name == name)
    }
}
