//! I/O helpers for reading and writing files

use rhai::EvalAltResult;
use std::path::Path;

/// Read a file's contents as a string
pub fn read_file(path: &str) -> Result<String, Box<EvalAltResult>> {
    read_text(path).map_err(|e| format!("read failed: {}", e).into())
}

/// Read through [`confine::open`]: below the run's root in a host run.
fn read_text(path: &str) -> std::io::Result<String> {
    use std::io::Read;
    let mut text = String::new();
    crate::helpers::confine::open(
        Path::new(path),
        std::fs::OpenOptions::new().read(true),
        libc::O_RDONLY,
        0,
    )?
    .read_to_string(&mut text)?;
    Ok(text)
}

/// Read a file's contents as a string, returning empty string on error
///
/// This is useful for recipes that want to check if a file exists and read it
/// without having to handle errors.
pub fn read_file_or_empty(path: &str) -> String {
    read_text(path).unwrap_or_default()
}

/// Create or truncate `path` and write `bytes`, through [`confine::open`].
fn write_bytes(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    crate::helpers::confine::open(
        Path::new(path),
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true),
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
        0o666,
    )
    .and_then(|mut file| file.write_all(bytes))
}

/// Write content to a file
pub fn write_file(path: &str, content: &str) -> Result<(), Box<EvalAltResult>> {
    write_bytes(path, content.as_bytes()).map_err(|e| format!("write failed: {}", e).into())
}

/// Append content to a file
pub fn append_file(path: &str, content: &str) -> Result<(), Box<EvalAltResult>> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let mut file = crate::helpers::confine::open(
        Path::new(path),
        OpenOptions::new().create(true).append(true),
        libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
        0o666,
    )
    .map_err(|e| format!("append failed: {}", e))?;

    file.write_all(content.as_bytes())
        .map_err(|e| format!("write failed: {}", e).into())
}

/// Append a line only if it does not already exist in the file
pub fn append_line_if_missing(path: &str, line: &str) -> Result<bool, Box<EvalAltResult>> {
    if line.contains('\n') {
        return Err("append_line_if_missing requires a single line without newline".into());
    }

    // Only a missing file counts as empty; an unreadable one must not get a
    // duplicate line.
    let existing = match read_text(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("read failed: {error}").into()),
    };
    if existing.lines().any(|candidate| candidate == line) {
        return Ok(false);
    }

    let mut to_append = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        to_append.push('\n');
    }
    to_append.push_str(line);
    to_append.push('\n');
    append_file(path, &to_append)?;
    Ok(true)
}

/// Replace all occurrences of a string inside a file
pub fn replace_in_file(path: &str, from: &str, to: &str) -> Result<(), Box<EvalAltResult>> {
    if from.is_empty() {
        return Err("replace_in_file requires a non-empty search string".into());
    }

    let file_path = Path::new(path);
    let content = read_text(path).map_err(|e| {
        format!(
            "replace_in_file read failed for {}: {}",
            file_path.display(),
            e
        )
    })?;
    let updated = content.replace(from, to);
    write_bytes(path, updated.as_bytes()).map_err(|e| {
        format!(
            "replace_in_file write failed for {}: {}",
            file_path.display(),
            e
        )
        .into()
    })
}

/// List the paths a glob pattern matches, sorted (callers that overlay
/// results, such as kconfig fragments, rely on the order).
pub fn glob_list(pattern: &str) -> Result<rhai::Array, Box<EvalAltResult>> {
    Ok(super::filesystem::glob_matches(pattern)?
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned().into())
        .collect())
}
