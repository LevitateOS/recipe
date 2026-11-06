//! Logging helpers

/// Log a message to stderr (stdout is reserved for JSON output)
pub fn log(msg: &str) {
    crate::diagnostics::diag!("[recipe] {}", msg);
}

/// Log a debug message (only shown in verbose mode)
pub fn debug(msg: &str) {
    crate::diagnostics::diag!("[recipe:debug] {}", msg);
}

/// Log a warning message
pub fn warn(msg: &str) {
    crate::diagnostics::diag!("[recipe:warn] {}", msg);
}
