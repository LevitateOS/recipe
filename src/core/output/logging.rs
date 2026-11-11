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

/// One progress report on its way from the recipe to the coordinator.
pub struct ProgressReport {
    pub recipe: String,
    pub phase: String,
    pub what: String,
    pub done: u64,
    pub total: u64,
}

type ProgressSink = dyn Fn(&ProgressReport) + Send + Sync;

static PROGRESS_SINK: std::sync::OnceLock<Box<ProgressSink>> = std::sync::OnceLock::new();

/// The worker routes progress reports to the coordinator through `sink`.
pub fn install_progress_sink(sink: Box<ProgressSink>) {
    let _ = PROGRESS_SINK.set(sink);
}

/// Least time between two reports leaving the worker.
const PROGRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

struct ProgressState {
    recipe: String,
    phase: String,
    last_sent: Option<std::time::Instant>,
    /// The newest report held back by the rate limit: (done, total, what).
    pending: Option<(u64, u64, String)>,
}

static PROGRESS: std::sync::Mutex<ProgressState> = std::sync::Mutex::new(ProgressState {
    recipe: String::new(),
    phase: String::new(),
    last_sent: None,
    pending: None,
});

fn send_progress(state: &mut ProgressState, done: u64, total: u64, what: String) {
    state.last_sent = Some(std::time::Instant::now());
    state.pending = None;
    let report = ProgressReport {
        recipe: state.recipe.clone(),
        phase: state.phase.clone(),
        what,
        done,
        total,
    };
    machine_event(serde_json::json!({
        "event": "progress", "recipe": report.recipe, "phase": report.phase,
        "what": report.what, "done": done, "total": total,
    }));
    if let Some(sink) = PROGRESS_SINK.get() {
        sink(&report);
    }
}

/// The recipe's `progress(done, total, what)`: sent now when it is the first,
/// completes the count, or the last one left at least 250 ms ago; otherwise
/// held, and sent by the next report or by [`flush_progress`].
pub fn progress_report(done: u64, total: u64, what: &str) {
    let Ok(mut state) = PROGRESS.lock() else {
        return;
    };
    let due = state
        .last_sent
        .is_none_or(|last| last.elapsed() >= PROGRESS_INTERVAL)
        || (total > 0 && done >= total);
    if due {
        send_progress(&mut state, done, total, what.to_owned());
    } else {
        state.pending = Some((done, total, what.to_owned()));
    }
}

/// Send the report the rate limit is holding, if any (end of a step, end of the run).
pub fn flush_progress() {
    if let Ok(mut state) = PROGRESS.lock()
        && let Some((done, total, what)) = state.pending.take()
    {
        send_progress(&mut state, done, total, what);
    }
}

/// A step starts: flush what the last one held, then tell the coordinator the
/// new phase with an empty count.
fn progress_phase(recipe: &str, hook: &str) {
    let Ok(mut state) = PROGRESS.lock() else {
        return;
    };
    if let Some((done, total, what)) = state.pending.take() {
        send_progress(&mut state, done, total, what);
    }
    state.recipe = recipe.to_owned();
    state.phase = hook.to_owned();
    send_progress(&mut state, 0, 0, String::new());
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
    let recipe = EVENT_RECIPE
        .lock()
        .map(|recipe| recipe.clone())
        .unwrap_or_default();
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
    match status {
        crate::core::events::HookStatus::Running => progress_phase(recipe, hook),
        crate::core::events::HookStatus::Success | crate::core::events::HookStatus::Failed => {
            flush_progress();
        }
        _ => {}
    }
    let event = RecipeHookEvent::new(recipe, hook, status, msg);
    let human = narration::sentence_for_event(hook, status, msg.trim());

    if machine_events() {
        crate::diagnostics::diag!("{}", event.as_json());
    }

    crate::diagnostics::diag!("{}", human);
}
