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
pub fn hook_event(recipe: &str, hook: &str, status: crate::core::events::HookStatus, msg: &str) {
    let event = RecipeHookEvent::new(recipe, hook, status, msg);
    let human = narration::sentence_for_event(hook, status, msg.trim());

    if machine_events() {
        crate::diagnostics::diag!("{}", event.as_json());
    }

    crate::diagnostics::diag!("{}", human);
}
