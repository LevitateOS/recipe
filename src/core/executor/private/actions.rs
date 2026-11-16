//! The operations besides install: `recipe cleanup` (`cleanup("manual")`,
//! docs/recipe-format.md section 3.7) and the upstream check's `latest()`
//! (sections 2.9 and 3.5).

use super::loaded::{EngineError, Load, with_recipe};
use super::reporting::{report_phase_failure, report_phase_success};
use crate::core::events::HookStatus;
use crate::core::output;
use crate::lifecycle::CleanupReason;
use anyhow::anyhow;
use std::path::Path;

// C4: what `recipe cleanup` deletes itself (downloads, build material,
// saved state, section 6.5) is C4's; it runs before this function. Here
// only the recipe's own `cleanup("manual")` runs. Its dependencies are not
// run for it, so their entries in `DEPS` are `()`.
pub(crate) fn cleanup(recipe: &Path, load: &Load<'_>) -> Result<(), EngineError> {
    with_recipe(recipe, load, |recipe| {
        let name = recipe.name.clone();
        output::set_event_recipe(&name);
        if !recipe.runtime.has_fn("cleanup", 1) {
            output::hook_event(
                &name,
                "cleanup",
                HookStatus::Skipped,
                "the recipe defines no cleanup(reason)",
            );
            output::skip(&format!(
                "{name} defines no cleanup(reason); nothing to run"
            ));
            return Ok(());
        }
        output::action(&format!("Cleaning up {name}"));
        super::flow::run_cleanup(recipe, CleanupReason::Manual)
            .inspect_err(|e| report_phase_failure(&name, "cleanup", e))?;
        report_phase_success(&name, "cleanup");
        Ok(())
    })
}

/// Call `latest()` and return the version it gives.
pub(crate) fn latest(recipe: &Path, load: &Load<'_>) -> Result<String, EngineError> {
    with_recipe(recipe, load, |recipe| {
        let name = recipe.name.clone();
        if !recipe.runtime.has_fn("latest", 0) {
            return Err(anyhow!("{name} defines no latest()").into());
        }
        let value = recipe
            .runtime
            .call("latest", Vec::new())
            .map_err(|error| anyhow!("latest() failed: {error}"))?;
        let version = value.into_string().map_err(|kind| {
            anyhow!("latest() returns the newest version as a string, not {kind}")
        })?;
        let version = version.trim().to_owned();
        if version.is_empty() || version.len() > 256 || version.chars().any(char::is_control) {
            return Err(anyhow!("latest() returned {version:?}, not a version").into());
        }
        Ok(version)
    })
}
