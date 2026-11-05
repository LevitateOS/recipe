//! Shared subprocess execution for recipe-facing shell and argv helpers.

use rhai::EvalAltResult;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};

const CAPTURE_TAIL_BYTES: usize = 256 * 1024;

fn description(command: &Command) -> String {
    // Environment overrides can contain secrets; do not use Command's Debug.
    let mut text = command.get_program().to_string_lossy().into_owned();
    for arg in command.get_args() {
        text.push(' ');
        text.push_str(&arg.to_string_lossy());
    }
    if let Some(dir) = command.get_current_dir() {
        text.push_str(&format!("\n  in: {}", dir.display()));
    }
    text
}

fn failure(label: &str, status: ExitStatus, tail: &[u8]) -> Box<EvalAltResult> {
    let tail = &tail[tail.len().saturating_sub(CAPTURE_TAIL_BYTES)..];
    format!(
        "command failed: {}\n  command: {}\n--- output tail ---\n{}",
        status,
        label,
        String::from_utf8_lossy(tail).trim_end()
    )
    .into()
}

fn drain<R: Read>(mut reader: R, tail: Arc<Mutex<VecDeque<u8>>>) -> std::io::Result<()> {
    let mut buffer = [0u8; 8192];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        // Diagnostics are best effort; pipe read errors are returned to the caller.
        let _ = std::io::stderr().write_all(&buffer[..count]);
        let _ = std::io::stderr().flush();
        let mut captured = tail
            .lock()
            .map_err(|_| std::io::Error::other("command output lock poisoned"))?;
        for byte in &buffer[..count] {
            if captured.len() == CAPTURE_TAIL_BYTES {
                captured.pop_front();
            }
            captured.push_back(*byte);
        }
    }
}

fn stream(mut command: Command) -> Result<(ExitStatus, Vec<u8>), Box<EvalAltResult>> {
    let label = description(&command);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("command failed to start: {label}: {error}"))?;
    let stdout = child.stdout.take().ok_or("failed to open child stdout")?;
    let stderr = child.stderr.take().ok_or("failed to open child stderr")?;
    let tail = Arc::new(Mutex::new(VecDeque::with_capacity(8192)));
    let out_tail = Arc::clone(&tail);
    let out = match std::thread::Builder::new().spawn(move || drain(stdout, out_tail)) {
        Ok(thread) => thread,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("command stdout reader failed to start: {label}: {error}").into());
        }
    };
    let err_tail = Arc::clone(&tail);
    let err = match std::thread::Builder::new().spawn(move || drain(stderr, err_tail)) {
        Ok(thread) => thread,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            // The child is stopped, so the already-running reader reaches EOF.
            let _ = out.join();
            return Err(format!("command stderr reader failed to start: {label}: {error}").into());
        }
    };
    let status = child.wait();
    if status.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    // Join both drains before propagating either error.
    let out = out.join();
    let err = err.join();
    let status = status.map_err(|error| format!("command failed to run: {label}: {error}"))?;
    for result in [out, err] {
        result
            .map_err(|_| format!("command output drain panicked: {label}"))?
            .map_err(|error| format!("command output read failed: {label}: {error}"))?;
    }
    let captured = tail
        .lock()
        .map_err(|_| "command output lock poisoned")?
        .iter()
        .copied()
        .collect();
    Ok((status, captured))
}

pub(crate) fn run_checked(command: Command) -> Result<(), Box<EvalAltResult>> {
    let label = description(&command);
    let (status, tail) = stream(command)?;
    if status.success() {
        Ok(())
    } else {
        Err(failure(&label, status, &tail))
    }
}

pub(crate) fn run_status(command: Command) -> Result<i64, Box<EvalAltResult>> {
    stream(command).map(|(status, _)| status.code().unwrap_or(-1) as i64)
}

pub(crate) fn capture(mut command: Command) -> Result<Output, Box<EvalAltResult>> {
    let label = description(&command);
    command
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("command failed to start: {label}: {error}").into())
}

pub(crate) fn checked_capture(command: Command) -> Result<Output, Box<EvalAltResult>> {
    let label = description(&command);
    let output = capture(command)?;
    let _ = std::io::stderr().write_all(&output.stderr);
    if output.status.success() {
        Ok(output)
    } else {
        let mut tail = output.stdout;
        tail.extend_from_slice(&output.stderr);
        Err(failure(&label, output.status, &tail))
    }
}
