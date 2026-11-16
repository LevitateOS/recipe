//! Output pipeline for Recipe: human narration + machine step events.
//!
//! - `logging` contains machine event emission, sinks and the machine-mode toggle.
//! - `narration` contains human-readable message rendering and CLI helpers.

mod logging;
mod narration;

pub use logging::{
    event_recipe, fetch_event, flush_progress, install_progress_sink, machine_event,
    progress_report, set_event_recipe, set_machine_events, step_event,
};
pub use narration::{action, detail, error, info, skip, sub_action, success, warning};
