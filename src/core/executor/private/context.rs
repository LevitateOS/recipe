//! Worker-local invocation and dependency state. The coordinator owns the
//! destination lock and all durable state; this module never rewrites sources.
use crate::core::executor::CompiledRecipe;
use anyhow::{Result, anyhow, bail};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CheckpointPoint {
    pub phase: &'static str,
    pub reason: &'static str,
}

/// Report a checkpoint even when recovery is disabled (acknowledgement 0).
pub(crate) type CheckpointSink<'a> =
    dyn FnMut(&CompiledRecipe, &rhai::Map, CheckpointPoint, bool) -> Result<()> + 'a;

pub(crate) struct ExecutionContext<'a> {
    sink: &'a mut CheckpointSink<'a>,
    work_root: Option<PathBuf>,
    active_recipes: Vec<PathBuf>,
    restored_states: std::collections::BTreeMap<PathBuf, rhai::Map>,
    resumable: bool,
}
impl<'a> ExecutionContext<'a> {
    pub(crate) fn worker(sink: &'a mut CheckpointSink<'a>) -> Self {
        Self {
            sink,
            work_root: None,
            active_recipes: Vec::new(),
            restored_states: Default::default(),
            resumable: false,
        }
    }
    pub(crate) fn resumable(&self) -> bool {
        self.resumable
    }

    pub(crate) fn prepare_work_root(&mut self, build_dir: &Path) {
        if self.work_root.is_none() {
            self.work_root = Some(build_dir.to_path_buf());
        }
    }

    pub(crate) fn tools_prefix_override(&self) -> Option<&Path> {
        Some(Path::new(crate::inputs::TOOLS_DOMAIN_PATH))
    }

    pub(crate) fn tools_prefix(&self) -> Result<PathBuf> {
        Ok(PathBuf::from(crate::inputs::TOOLS_DOMAIN_PATH))
    }

    pub(crate) fn work_root(&self) -> Result<&Path> {
        self.work_root
            .as_deref()
            .ok_or_else(|| anyhow!("missing recipe invocation work root"))
    }

    pub(crate) fn current_recipe(&self) -> Option<&Path> {
        self.active_recipes.last().map(PathBuf::as_path)
    }

    /// Detect cycles before evaluating the next recipe. Every nested
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

    pub(crate) fn restore_states(
        &mut self,
        states: &std::collections::BTreeMap<String, serde_json::Value>,
        enabled: bool,
    ) -> Result<()> {
        self.resumable = enabled;
        for (path, value) in states {
            let dynamic = rhai::serde::to_dynamic(value)?;
            let state = dynamic
                .try_cast::<rhai::Map>()
                .ok_or_else(|| anyhow!("saved ctx.state must be a map"))?;
            self.restored_states.insert(PathBuf::from(path), state);
        }
        Ok(())
    }

    pub(crate) fn restore_runtime(
        &self,
        recipe: &Path,
        runtime: &mut crate::core::invocation::Runtime<'_>,
    ) -> Result<()> {
        if self.resumable && !runtime.is_api2() {
            bail!(
                "resumable execution requires RECIPE_API = 2 and explicit ctx.state; use --no-persist-state while migrating"
            );
        }
        if let Some(state) = self.restored_states.get(recipe) {
            runtime.restore_state(state)?;
        }
        Ok(())
    }

    /// A reused dependency can discover new state in its readiness check.
    /// Save that state with the same filesystem before the parent continues.
    pub(crate) fn checkpoint_reused(
        &mut self,
        compiled: &mut CompiledRecipe,
        ctx: &rhai::Map,
    ) -> Result<()> {
        if self.resumable {
            self.persist_checkpoint(
                compiled,
                ctx,
                CheckpointPoint {
                    phase: "install",
                    reason: "check:is_installed",
                },
                true,
            )?;
        }
        Ok(())
    }

    pub(crate) fn persist_checkpoint(
        &mut self,
        compiled: &mut CompiledRecipe,
        ctx_map: &rhai::Map,
        point: CheckpointPoint,
        persist_enabled: bool,
    ) -> Result<()> {
        let result = (self.sink)(compiled, ctx_map, point, persist_enabled);
        if result.is_ok() && self.resumable {
            if let Some(state) = ctx_map
                .get("state")
                .and_then(|value| value.clone().try_cast::<rhai::Map>())
            {
                self.restored_states
                    .insert(compiled.recipe_path.clone(), state);
            }
        }
        result
    }
}
