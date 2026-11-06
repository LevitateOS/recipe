//! Read the environment seen by commands in the current invocation.

/// Return an effective child environment value, or empty if unset.
pub fn get_env(name: &str) -> String {
    crate::process::environment(name)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default()
}
