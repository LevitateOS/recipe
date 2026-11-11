//! Logging helpers

/// Log a message to stderr (stdout is reserved for JSON output)
pub fn log(msg: &str) {
    crate::diagnostics::diag!("[recipe] {}", msg);
}

/// Log a debug message
pub fn debug(msg: &str) {
    crate::diagnostics::diag!("[recipe:debug] {}", msg);
}

/// Log a warning message
pub fn warn(msg: &str) {
    crate::diagnostics::diag!("[recipe:warn] {}", msg);
}

/// Report progress: `done` of `total` (0 when unknown) `what` is counted.
pub fn progress(done: i64, total: i64, what: &str) {
    crate::core::output::progress_report(done.max(0) as u64, total.max(0) as u64, what);
}
