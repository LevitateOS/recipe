//! Machine-oriented logging and hook event plumbing.

use crate::core::events::RecipeHookEvent;
use crate::core::output::narration;
use std::sync::{
    Arc, OnceLock, RwLock,
    atomic::{AtomicBool, Ordering},
};

static MACHINE_EVENTS_ENABLED: AtomicBool = AtomicBool::new(false);
static EVENT_SINK: OnceLock<RwLock<Option<RecipeHookSink>>> = OnceLock::new();

/// Optional sink for structured hook events.
pub type RecipeHookSink = Arc<dyn Fn(&RecipeHookEvent) + Send + Sync>;

/// Enable or disable machine-friendly hook events.
pub fn set_machine_events(enabled: bool) {
    MACHINE_EVENTS_ENABLED.store(enabled, Ordering::Relaxed);
}

fn event_sink_store() -> &'static RwLock<Option<RecipeHookSink>> {
    EVENT_SINK.get_or_init(|| RwLock::new(None))
}

/// Set an optional sink for structured hook events.
pub fn set_event_sink(sink: Option<RecipeHookSink>) {
    let mut guard = event_sink_store()
        .write()
        .unwrap_or_else(|e| e.into_inner());
    *guard = sink;
}

/// Convenience wrapper for registering a hook sink without manually wrapping in `Arc`.
pub fn set_event_sink_handler<S>(sink: Option<S>)
where
    S: Fn(&RecipeHookEvent) + Send + Sync + 'static,
{
    set_event_sink(sink.map(|handler| -> RecipeHookSink { Arc::new(handler) as RecipeHookSink }));
}

fn emit_to_sink(event: &RecipeHookEvent) {
    let sink = event_sink_store()
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_else(|e| e.into_inner().clone());

    if let Some(sink) = sink {
        (sink)(event);
    }
}

fn machine_events() -> bool {
    MACHINE_EVENTS_ENABLED.load(Ordering::Relaxed)
}

/// Emit a hook event.
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

    emit_to_sink(event);

    if machine_events() {
        eprintln!("{}", event.as_json());
    }

    eprintln!("{}", human);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_set_event_sink_handler() {
        let events: Arc<Mutex<Vec<RecipeHookEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        set_event_sink_handler(Some(move |event: &RecipeHookEvent| {
            captured.lock().expect("mutex").push(event.clone());
        }));

        hook_event("pkg", "install", "running", "unit test");
        let got = {
            let guard = events.lock().expect("mutex");
            guard
                .iter()
                .any(|event| event.recipe == "pkg" && event.hook == "install")
        };

        assert!(got, "hook sink did not receive install event");
        set_event_sink(None);
    }
}
