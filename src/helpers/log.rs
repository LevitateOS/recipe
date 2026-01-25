//! Logging helpers

/// Log a message to stdout
pub fn log(msg: &str) {
    println!("[recipe] {}", msg);
}

/// Log a debug message (only shown in verbose mode)
pub fn debug(msg: &str) {
    eprintln!("[recipe:debug] {}", msg);
}

/// Log a warning message
pub fn warn(msg: &str) {
    eprintln!("[recipe:warn] {}", msg);
}
