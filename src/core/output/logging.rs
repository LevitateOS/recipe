//! Machine-oriented logging and hook event plumbing.

use crate::core::events::RecipeHookEvent;
use crate::core::output::narration;
use std::sync::atomic::{AtomicBool, Ordering};

static MACHINE_EVENTS_ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable or disable machine-friendly hook events.
pub fn set_machine_events(enabled: bool) {
    MACHINE_EVENTS_ENABLED.store(enabled, Ordering::Relaxed);
}

fn machine_events() -> bool {
    MACHINE_EVENTS_ENABLED.load(Ordering::Relaxed)
}

/// Emit one machine event payload (when enabled) and one human-readable line.
///
/// Machine payload example:
///   {"event":"recipe-hook","recipe":"name","hook":"install","status":"running","msg":"..."}
/// Human line example:
///   [install] Working on install
pub fn hook_event(recipe: &str, hook: &str, status: &str, msg: &str) {
    hook_event_struct(&RecipeHookEvent::new(recipe, hook, status, msg))
}

/// Emit a typed recipe hook event.
/// This is the canonical output path used by both machine and human renderers.
pub fn hook_event_struct(event: &RecipeHookEvent) {
    let human = narration::format_hook_line(event);

    if machine_events() {
        crate::logging::diag!("{}", event.as_json());
    }

    crate::logging::diag!("{}", human);
}
