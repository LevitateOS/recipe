//! Output pipeline for Recipe: human narration + machine hook logging.
//!
//! - `logging` contains machine event emission, sink hooks, and machine-mode output toggles.
//! - `narration` contains human-readable message rendering and CLI helpers.

mod logging;
mod narration;

pub use logging::{
    fetch_event, flush_progress, hook_event, install_progress_sink, machine_event, progress_report,
    set_event_recipe, set_machine_events,
};
pub use narration::{action, detail, error, info, skip, sub_action, success, warning};
