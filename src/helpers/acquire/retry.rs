//! The one place a failed acquisition is tried again.
//!
//! Every download (the `download` helpers and the built-in `SOURCES` and pin
//! fetch, which calls them) and the metadata fetches run their attempt through
//! [`retrying`]. A failure that is transient by nature is tried again after a
//! short, growing pause; an answer (a 404, a wrong checksum, a refused host, a
//! size limit, an interruption) ends the run at once.
//!
//! Transient: a name lookup that failed, a connection refused, reset or timed
//! out, a TLS handshake or a body cut off, HTTP 408, 425, 429, 500, 502, 503
//! and 504. Each attempt builds its own HTTP client, so it looks the name up
//! again (inside a Ginux domain the worker resolves names itself; in a host
//! run the resolver answers from the coordinator's pinned addresses, which
//! cannot fail transiently, and a refusal there is a permission error that is
//! never retried).

use crate::core::output;
use rhai::EvalAltResult;
use std::io::ErrorKind;
use std::time::{Duration, Instant};

/// Attempts per acquisition, the first included.
const ATTEMPTS: usize = 4;
/// The pause before attempt 2, 3 and 4, in seconds.
const PAUSES: [u64; ATTEMPTS - 1] = [1, 3, 9];

/// Why one attempt failed.
pub(super) enum Fail {
    /// Worth another attempt; the message is the reason.
    Transient(String),
    /// An answer: the run fails now with this error.
    Fatal(Box<EvalAltResult>),
}

impl From<Box<EvalAltResult>> for Fail {
    fn from(error: Box<EvalAltResult>) -> Self {
        Self::Fatal(error)
    }
}

impl From<String> for Fail {
    fn from(message: String) -> Self {
        Self::Fatal(message.into())
    }
}

impl From<&str> for Fail {
    fn from(message: &str) -> Self {
        Self::Fatal(message.into())
    }
}

/// Classify the failure of sending a request; `prefix` starts the message.
pub(super) fn call_failure(prefix: &str, error: ureq::Error) -> Fail {
    let message = format!("{prefix}: {error}");
    let transient = match &error {
        ureq::Error::Status(code, _) => matches!(code, 408 | 425 | 429 | 500 | 502 | 503 | 504),
        ureq::Error::Transport(transport) => match transport.kind() {
            // A lookup or connect failure is retried unless the resolver
            // refused the host (not in the grant).
            ureq::ErrorKind::Dns | ureq::ErrorKind::ConnectionFailed => {
                source_io_kind(&error) != Some(ErrorKind::PermissionDenied)
            }
            // A failure on an open connection: only the interrupted kinds
            // (a TLS certificate error is `InvalidData`, an answer).
            ureq::ErrorKind::Io => source_io_kind(&error).is_none_or(transient_io),
            ureq::ErrorKind::BadStatus => true,
            _ => false,
        },
    };
    if transient {
        Fail::Transient(message)
    } else {
        Fail::Fatal(message.into())
    }
}

/// Classify a failed read of a response body; the byte limit, a full disk
/// and the like (kinds that are not about the connection) are answers.
pub(super) fn read_failure(prefix: &str, error: std::io::Error) -> Fail {
    let message = format!("{prefix}: {error}");
    if transient_io(error.kind()) {
        Fail::Transient(message)
    } else {
        Fail::Fatal(message.into())
    }
}

fn transient_io(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::TimedOut
            | ErrorKind::WouldBlock
            | ErrorKind::ConnectionRefused
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::NotConnected
            | ErrorKind::BrokenPipe
            | ErrorKind::UnexpectedEof
            | ErrorKind::Interrupted
    )
}

/// The kind of the first `io::Error` in the error's chain of causes. A
/// resolver refusal (`PermissionDenied`) is found here and is not transient;
/// a lookup failure from the system resolver has no useful kind of its own
/// and is retried.
fn source_io_kind(error: &ureq::Error) -> Option<ErrorKind> {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(error);
    while let Some(current) = cause {
        if let Some(io) = current.downcast_ref::<std::io::Error>() {
            return Some(io.kind());
        }
        cause = current.source();
    }
    None
}

/// Run `attempt` up to [`ATTEMPTS`] times while it fails transiently. `url`
/// names the transfer in the line printed before each new attempt (host and
/// path only).
pub(super) fn retrying<T>(
    url: &str,
    mut attempt: impl FnMut() -> Result<T, Fail>,
) -> Result<T, Box<EvalAltResult>> {
    let target = describe(url);
    let mut number = 1;
    loop {
        interrupted()?;
        match attempt() {
            Ok(value) => return Ok(value),
            Err(Fail::Fatal(error)) => return Err(error),
            Err(Fail::Transient(reason)) if number == ATTEMPTS => {
                return Err(format!("{reason} (gave up after {ATTEMPTS} attempts)").into());
            }
            Err(Fail::Transient(reason)) => {
                let pause = PAUSES[number - 1];
                number += 1;
                output::warning(&format!(
                    "{target}: {reason}; attempt {number} of {ATTEMPTS} in {pause} s"
                ));
                pause_for(Duration::from_secs(pause))?;
            }
        }
    }
}

/// `host/path` of a URL, without credentials or query.
fn describe(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => format!("{}{}", parsed.host_str().unwrap_or("?"), parsed.path()),
        Err(_) => "download".to_owned(),
    }
}

fn interrupted() -> Result<(), Box<EvalAltResult>> {
    crate::cancellation::check().map_err(|error| error.to_string().into())
}

/// Sleep, noticing an interruption within a tenth of a second.
fn pause_for(total: Duration) -> Result<(), Box<EvalAltResult>> {
    let started = Instant::now();
    while started.elapsed() < total {
        interrupted()?;
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}
