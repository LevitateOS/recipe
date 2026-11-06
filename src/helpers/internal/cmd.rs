//! Recipe-facing subprocess policy on top of [`crate::process`].
//!
//! Helpers stream child output into the diagnostic stream and keep a tail for
//! failure messages; capturing helpers keep everything and forward stderr
//! after exit.

use crate::process::{Error, Exit, Finished, Keep, Route, Run};
use rhai::EvalAltResult;
use std::process::Command;

const CAPTURE_TAIL_BYTES: usize = 256 * 1024;

fn failure(label: &str, exit: Exit, tail: &[u8]) -> Box<EvalAltResult> {
    let tail = &tail[tail.len().saturating_sub(CAPTURE_TAIL_BYTES)..];
    format!(
        "command failed: {exit}\n  command: {label}\n--- output tail ---\n{}",
        String::from_utf8_lossy(tail).trim_end()
    )
    .into()
}

fn streamed(command: Command) -> (String, Result<Finished, Error>) {
    let run = Run::new(command)
        .route(Route::Diagnostics, Route::Diagnostics)
        .keep(Keep::Tail(CAPTURE_TAIL_BYTES));
    (run.label(), run.run())
}

/// Run with live output; fail unless the child exits 0.
pub(crate) fn run_checked(command: Command) -> Result<(), Box<EvalAltResult>> {
    let (label, finished) = streamed(command);
    let finished = finished.map_err(|error| error.to_string())?;
    if finished.exit.success() {
        Ok(())
    } else {
        Err(failure(&label, finished.exit, &finished.tail))
    }
}

/// Run with live output and return the exit code. Recipes see `-1` when the
/// program could not start or was killed by a signal (the historical shell
/// helper contract); any other failure is an error.
pub(crate) fn run_status(command: Command) -> Result<i64, Box<EvalAltResult>> {
    match streamed(command).1 {
        Ok(Finished {
            exit: Exit::Code(code),
            ..
        }) => Ok(i64::from(code)),
        Ok(Finished {
            exit: Exit::Signal(_),
            ..
        })
        | Err(Error::Spawn(_)) => Ok(-1),
        Err(error) => Err(error.to_string().into()),
    }
}

/// Run silently and return everything the child wrote.
pub(crate) fn capture(command: Command) -> Result<Finished, Box<EvalAltResult>> {
    Run::new(command)
        .run()
        .map_err(|error| error.to_string().into())
}

/// [`capture`], forward stderr to the diagnostic stream, and fail unless the
/// child exits 0.
pub(crate) fn checked_capture(command: Command) -> Result<Finished, Box<EvalAltResult>> {
    let run = Run::new(command);
    let label = run.label();
    let finished = run.run().map_err(|error| error.to_string())?;
    let _ = crate::diagnostics::write(crate::diagnostics::Stream::Stderr, &finished.stderr);
    if finished.exit.success() {
        Ok(finished)
    } else {
        let mut tail = finished.stdout;
        tail.extend_from_slice(&finished.stderr);
        Err(failure(&label, finished.exit, &tail))
    }
}
