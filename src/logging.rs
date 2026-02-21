//! Shared logging contract for Recipe hook events.
//!
//! This module provides stable, crate-level hooks so other crates can emit the same
//! human + machine event contract used by the Recipe CLI.

use crate::core::events;

/// Machine event envelope name used by all Recipe hook events.
pub const RECIPE_HOOK_EVENT: &str = events::RECIPE_HOOK_EVENT;

/// Canonical machine-hook event payload.
pub type RecipeHookEvent = events::RecipeHookEvent;

pub use crate::core::output::{RecipeHookSink, set_event_sink, set_event_sink_handler};
pub use events::make_machine_hook_event;

/// Emit a hook event using Recipe's default formatter and hook contract.
pub fn emit_hook_event(recipe: &str, hook: &str, status: &str, msg: &str) {
    crate::core::output::hook_event(recipe, hook, status, msg);
}

/// Emit a typed hook event using Recipe's default formatter and hook contract.
pub fn emit_hook_event_struct(event: &RecipeHookEvent) {
    crate::core::output::hook_event_struct(event);
}

/// Toggle machine-events output globally for Recipe logging.
pub fn set_machine_events(enabled: bool) {
    crate::core::output::set_machine_events(enabled);
}
