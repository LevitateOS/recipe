//! Worker-local invocation state of the one recipe this domain runs.
//! The coordinator owns the destination lock and all durable
//! state; this module never rewrites sources.
use anyhow::{Result, anyhow};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Reports one recovery point (a recipe identity and its ctx) to the
/// coordinator and returns once it is acknowledged.
pub(crate) type CheckpointSink<'a> = dyn FnMut(&Path, &rhai::Map) -> Result<()> + 'a;

pub(crate) struct ExecutionContext<'a> {
    sink: &'a mut CheckpointSink<'a>,
    options: crate::ExecutionOptions,
    /// Saved state was offered and checkpoints are kept for recovery.
    resumable: bool,
    states: BTreeMap<PathBuf, rhai::Map>,
}

impl<'a> ExecutionContext<'a> {
    pub(crate) fn new(
        sink: &'a mut CheckpointSink<'a>,
        options: crate::ExecutionOptions,
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
            resumable,
            states,
        })
    }

    pub(crate) fn options(&self) -> &crate::ExecutionOptions {
        &self.options
    }

    pub(crate) fn resumable(&self) -> bool {
        self.resumable
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
    /// without persistence there is nothing to save. When its state equals the
    /// state already saved for it (restored, or checkpointed earlier in this
    /// invocation), the newest saved generation already pairs that state with
    /// a filesystem a resume can continue from, so there is nothing new to
    /// save: no snapshot of the whole domain storage is taken again.
    pub(crate) fn checkpoint_reused(&mut self, recipe: &Path, ctx: &rhai::Map) -> Result<()> {
        if !self.resumable {
            return Ok(());
        }
        // A state that does not convert is left to `checkpoint`, which
        // reports exactly what cannot be saved.
        let json =
            |value: &rhai::Dynamic| rhai::serde::from_dynamic::<serde_json::Value>(value).ok();
        if let (Some(saved), Some(state)) = (self.states.get(recipe), ctx.get("state"))
            && let (Some(saved), Some(state)) =
                (json(&rhai::Dynamic::from_map(saved.clone())), json(state))
            && saved == state
        {
            return Ok(());
        }
        self.checkpoint(recipe, ctx)
    }
}
