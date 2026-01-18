//! I/O helpers for reading files and listing globs

use rhai::EvalAltResult;

/// Read a file's contents as a string
pub fn read_file(path: &str) -> Result<String, Box<EvalAltResult>> {
    std::fs::read_to_string(path).map_err(|e| format!("read failed: {}", e).into())
}

/// List files matching a glob pattern
pub fn glob_list(pattern: &str) -> rhai::Array {
    glob::glob(pattern)
        .map(|paths| {
            paths
                .filter_map(|r| r.ok())
                .map(|p| rhai::Dynamic::from(p.to_string_lossy().to_string()))
                .collect()
        })
        .unwrap_or_default()
}
