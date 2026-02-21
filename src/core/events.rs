//! Canonical hook logging contract for recipe lifecycle events.
//!
//! This module defines the machine event model and helper constructors used by both
//! Rust APIs and CLI output. String fields are intentionally used instead of enum
//! variants to preserve forward compatibility with existing hook names.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Event envelope used by machine-readable hook logging.
pub const RECIPE_HOOK_EVENT: &str = "recipe-hook";

/// Canonical machine event payload for recipe lifecycle hooks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipeHookEvent {
    pub event: String,
    pub recipe: String,
    pub hook: String,
    pub status: String,
    pub msg: String,
}

impl RecipeHookEvent {
    /// Construct a standard recipe hook event.
    pub fn new(recipe: &str, hook: &str, status: &str, msg: &str) -> Self {
        Self {
            event: RECIPE_HOOK_EVENT.to_string(),
            recipe: recipe.to_string(),
            hook: hook.to_string(),
            status: status.to_string(),
            msg: msg.to_string(),
        }
    }

    /// Serialize as JSON string for stderr/stdout machine streams.
    pub fn as_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            let fallback = json!({
                "event": self.event,
                "recipe": self.recipe,
                "hook": self.hook,
                "status": self.status,
                "msg": self.msg,
            });
            fallback.to_string()
        })
    }

    /// Serialize as JSON value.
    pub fn as_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| {
            json!({
                "event": self.event,
                "recipe": self.recipe,
                "hook": self.hook,
                "status": self.status,
                "msg": self.msg,
            })
        })
    }
}

/// Build a machine JSON event line for quick callers.
pub fn make_machine_hook_event(recipe: &str, hook: &str, status: &str, msg: &str) -> String {
    RecipeHookEvent::new(recipe, hook, status, msg).as_json()
}
