//! Canonical hook logging contract for recipe lifecycle events.
//!
//! String fields are intentionally used instead of enum variants to preserve
//! forward compatibility with existing hook names.

use serde::Serialize;

/// Event envelope used by machine-readable hook logging.
pub const RECIPE_HOOK_EVENT: &str = "recipe-hook";

/// Canonical machine event payload for recipe lifecycle hooks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecipeHookEvent {
    pub event: &'static str,
    pub recipe: String,
    pub hook: String,
    pub status: String,
    pub msg: String,
}

impl RecipeHookEvent {
    /// Construct a standard recipe hook event.
    pub fn new(recipe: &str, hook: &str, status: &str, msg: &str) -> Self {
        Self {
            event: RECIPE_HOOK_EVENT,
            recipe: recipe.to_string(),
            hook: hook.to_string(),
            status: status.to_string(),
            msg: msg.to_string(),
        }
    }

    /// Serialize as one JSON line for machine streams.
    pub fn as_json(&self) -> String {
        serde_json::to_string(self).expect("hook event fields always serialize")
    }
}
