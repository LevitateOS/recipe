//! Worker-local invocation state shared by the main recipe and its
//! dependencies. The coordinator owns the destination lock and all durable
//! state; this module never rewrites sources.
use anyhow::{Result, anyhow, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Reports one recovery point (a recipe identity and its ctx) to the
/// coordinator and returns once it is acknowledged.
pub(crate) type CheckpointSink<'a> = dyn FnMut(&Path, &rhai::Map) -> Result<()> + 'a;

pub(crate) struct ExecutionContext<'a> {
    sink: &'a mut CheckpointSink<'a>,
    options: crate::ExecutionOptions,
    /// The main recipe's work directory; dependency work lives under it.
    work_root: PathBuf,
    /// Saved state was offered and checkpoints are kept for recovery.
    resumable: bool,
    /// Recipes being evaluated, main recipe first.
    active: Vec<PathBuf>,
    states: BTreeMap<PathBuf, rhai::Map>,
}

impl<'a> ExecutionContext<'a> {
    pub(crate) fn new(
        sink: &'a mut CheckpointSink<'a>,
        options: crate::ExecutionOptions,
        work_root: &Path,
        saved: &BTreeMap<String, serde_json::Value>,
        resumable: bool,
    ) -> Result<Self> {
        let mut states = BTreeMap::new();
        for (path, value) in saved {
            let state = rhai::serde::to_dynamic(value)?
                .try_cast::<rhai::Map>()
                .ok_or_else(|| anyhow!("saved ctx.state must be a map"))?;
            states.insert(PathBuf::from(path), state);
        }
        Ok(Self {
            sink,
            options,
            work_root: work_root.to_path_buf(),
            resumable,
            active: Vec::new(),
            states,
        })
    }

    /// Options for the recipe being evaluated: force flags apply to the main
    /// recipe only, never to its dependencies.
    pub(crate) fn options(&self) -> crate::ExecutionOptions {
        if self.active.len() > 1 {
            self.options.for_dependency()
        } else {
            self.options.clone()
        }
    }

    pub(crate) fn resumable(&self) -> bool {
        self.resumable
    }

    pub(crate) fn work_root(&self) -> &Path {
        &self.work_root
    }

    pub(crate) fn current_recipe(&self) -> Option<&Path> {
        self.active.last().map(PathBuf::as_path)
    }

    /// Start evaluating `recipe`, rejecting cycles and runaway nesting.
    pub(crate) fn enter(&mut self, recipe: &Path) -> Result<()> {
        if self.active.iter().any(|active| active == recipe) {
            let chain = self
                .active
                .iter()
                .chain(std::iter::once(&recipe.to_path_buf()))
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>();
            bail!("Circular recipe dependency detected: {}", chain.join(" -> "));
        }
        if self.active.len() >= crate::recipes::MAX_DEPTH {
            bail!(
                "Recipe dependency depth exceeds {} at {}",
                crate::recipes::MAX_DEPTH,
                recipe.display()
            );
        }
        self.active.push(recipe.to_path_buf());
        Ok(())
    }

    pub(crate) fn leave(&mut self) {
        self.active.pop();
    }

    pub(crate) fn saved_state(&self, recipe: &Path) -> Option<&rhai::Map> {
        self.states.get(recipe)
    }

    /// Report a recovery point. Every completed phase and cleanup reports one,
    /// also without persistence, so the coordinator sees each step.
    pub(crate) fn checkpoint(&mut self, recipe: &Path, ctx: &rhai::Map) -> Result<()> {
        (self.sink)(recipe, ctx)?;
        if self.resumable
            && let Some(state) = ctx
                .get("state")
                .and_then(|value| value.clone().try_cast::<rhai::Map>())
        {
            self.states.insert(recipe.to_path_buf(), state);
        }
        Ok(())
    }

    /// A recipe that is already installed can still discover state in its
    /// check. Save it, with the matching filesystem, before the parent goes on;
    /// without persistence there is nothing to save.
    pub(crate) fn checkpoint_reused(&mut self, recipe: &Path, ctx: &rhai::Map) -> Result<()> {
        if self.resumable {
            self.checkpoint(recipe, ctx)?;
        }
        Ok(())
    }
}
