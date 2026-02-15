//! I/O helpers for reading and writing files

use rhai::EvalAltResult;

/// Read a file's contents as a string
pub fn read_file(path: &str) -> Result<String, Box<EvalAltResult>> {
    std::fs::read_to_string(path).map_err(|e| format!("read failed: {}", e).into())
}

/// Read a file's contents as a string, returning empty string on error
///
/// This is useful for recipes that want to check if a file exists and read it
/// without having to handle errors.
pub fn read_file_or_empty(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Write content to a file
pub fn write_file(path: &str, content: &str) -> Result<(), Box<EvalAltResult>> {
    std::fs::write(path, content).map_err(|e| format!("write failed: {}", e).into())
}

/// Append content to a file
pub fn append_file(path: &str, content: &str) -> Result<(), Box<EvalAltResult>> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("append failed: {}", e))?;

    file.write_all(content.as_bytes())
        .map_err(|e| format!("write failed: {}", e).into())
}

/// List files matching a glob pattern
pub fn glob_list(pattern: &str) -> rhai::Array {
    // Deterministic ordering matters for callers that merge/overlay results
    // (e.g., kconfig fragments). The glob crate does not guarantee ordering.
    let mut out: Vec<String> = glob::glob(pattern)
        .map(|paths| {
            paths
                .filter_map(|r| r.ok())
                .map(|p| p.to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();

    out.sort();
    out.into_iter().map(rhai::Dynamic::from).collect()
}
