//! Environment variable helpers

/// Get an environment variable, returning empty string if not set
pub fn get_env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// Set an environment variable
pub fn set_env(name: &str, value: &str) {
    // SAFETY: We are setting env vars in a single-threaded recipe context.
    // The caller must ensure no other threads are reading env vars concurrently.
    unsafe { std::env::set_var(name, value) };
}
