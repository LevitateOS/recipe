//! Pure shell execution helpers
//!
//! These functions run commands in the current working directory
//! without depending on execution context.
//!
//! IMPORTANT: `shell()`, `shell_in()`, `shell_status()`, and `shell_status_in()`
//! stream child output to stderr so that shell output does not corrupt the JSON
//! context emitted on stdout by the recipe binary.

use rhai::EvalAltResult;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const CAPTURE_TAIL_BYTES: usize = 256 * 1024;

fn push_tail(buf: &mut VecDeque<u8>, bytes: &[u8]) {
    for &b in bytes {
        if buf.len() == CAPTURE_TAIL_BYTES {
            buf.pop_front();
        }
        buf.push_back(b);
    }
}

fn run_streaming(dir: Option<&str>, cmd: &str) -> Result<(), Box<EvalAltResult>> {
    let mut c = Command::new("sh");
    c.args(["-c", cmd])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(d) = dir {
        c.current_dir(d);
    }

    let mut child = c
        .spawn()
        .map_err(|e| format!("command failed to start: {}", e))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to open child stdout".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to open child stderr".to_owned())?;

    let tail: Arc<Mutex<VecDeque<u8>>> = Arc::new(Mutex::new(VecDeque::with_capacity(
        CAPTURE_TAIL_BYTES.min(8192),
    )));

    let tail_out = Arc::clone(&tail);
    let t1 = std::thread::spawn(move || stream_to_stderr_and_capture(stdout, tail_out));
    let tail_err = Arc::clone(&tail);
    let t2 = std::thread::spawn(move || stream_to_stderr_and_capture(stderr, tail_err));

    let status = child
        .wait()
        .map_err(|e| format!("command failed to run: {}", e))?;

    let _ = t1.join();
    let _ = t2.join();

    if status.success() {
        return Ok(());
    }

    let code = status.code();
    let tail_str = {
        let locked = tail.lock().unwrap();
        let bytes: Vec<u8> = locked.iter().copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    };

    Err(format!(
        "command failed with exit code: {:?}\n  command: {}\n--- output tail ---\n{}",
        code,
        cmd,
        tail_str.trim_end()
    )
    .into())
}

fn stream_to_stderr_and_capture<R: Read>(mut r: R, tail: Arc<Mutex<VecDeque<u8>>>) {
    let mut buf = [0u8; 8192];
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };

        // Best-effort streaming to terminal.
        let _ = std::io::stderr().write_all(&buf[..n]);
        let _ = std::io::stderr().flush();

        if let Ok(mut locked) = tail.lock() {
            push_tail(&mut locked, &buf[..n]);
        }
    }
}

fn stream_to_stderr<R: Read>(mut r: R) {
    let mut buf = [0u8; 8192];
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };

        // Best-effort streaming to terminal.
        let _ = std::io::stderr().write_all(&buf[..n]);
        let _ = std::io::stderr().flush();
    }
}

fn run_streaming_status(dir: Option<&str>, cmd: &str) -> i64 {
    let mut c = Command::new("sh");
    c.args(["-c", cmd])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(d) = dir {
        c.current_dir(d);
    }

    let mut child = match c.spawn() {
        Ok(c) => c,
        Err(_) => return -1,
    };

    let Some(stdout) = child.stdout.take() else {
        return -1;
    };
    let Some(stderr) = child.stderr.take() else {
        return -1;
    };

    let t1 = std::thread::spawn(move || stream_to_stderr(stdout));
    let t2 = std::thread::spawn(move || stream_to_stderr(stderr));

    let status = child.wait().ok();

    let _ = t1.join();
    let _ = t2.join();

    status
        .and_then(|s| s.code())
        .unwrap_or(-1)
        .clamp(i32::MIN, i32::MAX) as i64
}

/// Run a shell command in the current directory.
///
/// Throws an error if the command fails.
/// Child stdout and stderr are streamed to stderr so build output is visible without corrupting
/// machine-readable JSON on stdout.
///
/// # Example
/// ```rhai
/// shell("make -j4");
/// ```
pub fn shell(cmd: &str) -> Result<(), Box<EvalAltResult>> {
    run_streaming(None, cmd)
}

/// Run a shell command in a specific directory.
///
/// Throws an error if the command fails.
/// Child stdout and stderr are streamed to stderr to protect the JSON output pipe.
///
/// # Example
/// ```rhai
/// shell_in("/tmp/build", "make -j4");
/// ```
pub fn shell_in(dir: &str, cmd: &str) -> Result<(), Box<EvalAltResult>> {
    run_streaming(Some(dir), cmd)
        .map_err(|e| format!("{}\n  in: {}", e.to_string().trim_end_matches('\n'), dir).into())
}

/// Run a shell command and return its exit status code.
///
/// Returns the exit code (0 for success), or -1 if the command couldn't run.
/// Child stdout is redirected to stderr to protect the JSON output pipe.
///
/// # Example
/// ```rhai
/// let code = shell_status("test -f /etc/passwd");
/// if code == 0 {
///     log("file exists");
/// }
/// ```
pub fn shell_status(cmd: &str) -> i64 {
    run_streaming_status(None, cmd)
}

/// Run a shell command in a specific directory and return its exit status code.
/// Child stdout is redirected to stderr to protect the JSON output pipe.
pub fn shell_status_in(dir: &str, cmd: &str) -> i64 {
    run_streaming_status(Some(dir), cmd)
}

/// Run a shell command and return its stdout output.
///
/// Throws an error if the command fails.
/// NOTE: This captures stdout for the caller — does NOT redirect to stderr.
///
/// # Example
/// ```rhai
/// let output = shell_output("uname -r");
/// log("kernel: " + trim(output));
/// ```
pub fn shell_output(cmd: &str) -> Result<String, Box<EvalAltResult>> {
    let output = Command::new("sh")
        .args(["-c", cmd])
        .output()
        .map_err(|e| format!("command failed to start: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "command failed with exit code: {:?}\n  command: {}",
            output.status.code(),
            cmd
        )
        .into());
    }

    String::from_utf8(output.stdout).map_err(|e| format!("invalid utf8 output: {}", e).into())
}

/// Run a shell command in a specific directory and return its stdout output.
/// NOTE: This captures stdout for the caller — does NOT redirect to stderr.
pub fn shell_output_in(dir: &str, cmd: &str) -> Result<String, Box<EvalAltResult>> {
    let output = Command::new("sh")
        .args(["-c", cmd])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("command failed to start: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "command failed with exit code: {:?}\n  command: {}\n  in: {}",
            output.status.code(),
            cmd,
            dir
        )
        .into());
    }

    String::from_utf8(output.stdout).map_err(|e| format!("invalid utf8 output: {}", e).into())
}
