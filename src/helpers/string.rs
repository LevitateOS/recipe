//! String manipulation helpers

/// Trim whitespace from both ends of a string
pub fn trim(s: &str) -> String {
    s.trim().to_string()
}

/// Check if a string starts with a prefix
pub fn starts_with(s: &str, prefix: &str) -> bool {
    s.starts_with(prefix)
}

/// Check if a string ends with a suffix
pub fn ends_with(s: &str, suffix: &str) -> bool {
    s.ends_with(suffix)
}

/// Check if a string contains a substring
pub fn contains(s: &str, pattern: &str) -> bool {
    s.contains(pattern)
}

/// Replace all occurrences of a pattern with a replacement
pub fn replace(s: &str, from: &str, to: &str) -> String {
    s.replace(from, to)
}

/// Split a string by a delimiter and return an array
pub fn split(s: &str, delimiter: &str) -> rhai::Array {
    s.split(delimiter)
        .map(|part| rhai::Dynamic::from(part.to_string()))
        .collect()
}
