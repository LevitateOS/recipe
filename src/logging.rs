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

// Worker installation is explicit and never selected through environment flags.
// The shared callback serializes diagnostics with lifecycle protocol frames.
type DiagnosticSink =
    dyn Fn(crate::worker::protocol::DiagnosticStream, &[u8]) -> std::io::Result<()> + Send + Sync;
static DIAGNOSTIC_SINK: std::sync::OnceLock<Box<DiagnosticSink>> = std::sync::OnceLock::new();

pub(crate) fn install_diagnostic_sink(sink: Box<DiagnosticSink>) -> anyhow::Result<()> {
    DIAGNOSTIC_SINK
        .set(sink)
        .map_err(|_| anyhow::anyhow!("worker diagnostic sink already installed"))
}

pub(crate) fn diagnostic(
    stream: crate::worker::protocol::DiagnosticStream,
    bytes: &[u8],
) -> std::io::Result<()> {
    use std::io::Write;
    match DIAGNOSTIC_SINK.get() {
        Some(sink) => sink(stream, bytes),
        None => std::io::stderr().write_all(bytes),
    }
}

pub(crate) fn diagnostic_line(message: &str) {
    let mut bytes = message.as_bytes().to_vec();
    bytes.push(b'\n');
    let _ = diagnostic(crate::worker::protocol::DiagnosticStream::Recipe, &bytes);
}

macro_rules! diag {
    ($($args:tt)*) => { $crate::logging::diagnostic_line(&format!($($args)*)) };
}
pub(crate) use diag;
