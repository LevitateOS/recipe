//! Output pipeline for Recipe: human narration + machine hook logging.
//!
//! - `logging` contains machine event emission, sink hooks, and machine-mode output toggles.
//! - `narration` contains human-readable message rendering and CLI helpers.

mod logging;
mod narration;

pub use logging::{
    RecipeHookSink, hook_event, hook_event_struct, set_event_sink, set_event_sink_handler,
    set_machine_events,
};
pub use narration::{
    action, action_numbered, build_spinner, detail, download_progress, error, info, list_item,
    progress_done, progress_fail, progress_success, skip, spinner, sub_action, success, warning,
};
