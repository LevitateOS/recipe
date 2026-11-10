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

/// Emit one event that only a front end reads: a JSON object with an `event`
/// field, on the diagnostic stream like the hook events. Nothing is written
/// without `--machine-events`, and no human line goes with it.
pub fn machine_event(event: serde_json::Value) {
    if machine_events() {
        crate::diagnostics::diag!("{event}");
    }
}

/// The recipe this process runs, named in events raised below the engine
/// (a download does not know whose it is).
static EVENT_RECIPE: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

pub fn set_event_recipe(name: &str) {
    if let Ok(mut recipe) = EVENT_RECIPE.lock() {
        *recipe = name.to_owned();
    }
}

/// How far a download is: `{"event":"fetch","recipe":..,"source":..,"bytes":..,"total":..}`,
/// `total` null when the server did not say.
pub fn fetch_event(source: &str, bytes: u64, total: Option<u64>) {
    if !machine_events() {
        return;
    }
    let recipe = EVENT_RECIPE.lock().map(|recipe| recipe.clone()).unwrap_or_default();
    machine_event(serde_json::json!({
        "event": "fetch", "recipe": recipe, "source": source, "bytes": bytes, "total": total,
    }));
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
