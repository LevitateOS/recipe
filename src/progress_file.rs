//! The coordinator's `--progress-file`: one JSON object, kept current while an
//! invocation runs so a reader outside can see how far it is.
//!
//! The worker reports through `Progress` frames
//! ([`crate::worker::protocol::ProgressPayload`]); only the coordinator writes
//! host files. The file is replaced atomically (temp file in the same
//! directory, then rename), at most once a second, except for the first
//! report, a change of phase and the end of the invocation, which are written
//! at once. A report held back by the limit is written by the next frame or at
//! the end. A failed write is reported once and never fails the invocation.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::worker::protocol::ProgressPayload;

const WRITE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

pub(crate) struct ProgressFile {
    path: PathBuf,
    invocation: String,
    recipe: String,
    phase: String,
    what: String,
    done: u64,
    total: u64,
    started: String,
    reported: bool,
    last_write: Option<Instant>,
    warned: bool,
}

impl ProgressFile {
    /// Write the first record (`done` 0, `total` 0, no `what`) so a reader sees
    /// the invocation exists.
    pub(crate) fn start(path: &Path, invocation: &str, recipe: &str, phase: &str) -> Self {
        let mut file = Self {
            path: std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()),
            invocation: invocation.to_owned(),
            recipe: recipe.to_owned(),
            phase: phase.to_owned(),
            what: String::new(),
            done: 0,
            total: 0,
            started: rfc3339(SystemTime::now()),
            reported: false,
            last_write: None,
            warned: false,
        };
        file.write("running");
        file
    }

    /// Take one report from the worker.
    pub(crate) fn report(&mut self, report: &ProgressPayload) {
        let first = !self.reported;
        let phase_changed = report.phase != self.phase;
        self.reported = true;
        self.recipe.clone_from(&report.recipe);
        self.phase.clone_from(&report.phase);
        self.what.clone_from(&report.what);
        self.done = report.done;
        self.total = report.total;
        if first
            || phase_changed
            || self
                .last_write
                .is_none_or(|last| last.elapsed() >= WRITE_INTERVAL)
        {
            self.write("running");
        }
    }

    /// The invocation ended: write the final state.
    pub(crate) fn finish(&mut self, ok: bool) {
        self.write(if ok { "finished" } else { "failed" });
    }

    fn write(&mut self, state: &str) {
        self.last_write = Some(Instant::now());
        let record = serde_json::json!({
            "version": 1,
            "invocation": self.invocation,
            "recipe": self.recipe,
            "phase": self.phase,
            "what": self.what,
            "done": self.done,
            "total": self.total,
            "started": self.started,
            "updated": rfc3339(SystemTime::now()),
            "state": state,
        });
        if let Err(error) = replace_atomically(&self.path, record.to_string().as_bytes())
            && !self.warned
        {
            self.warned = true;
            crate::diagnostics::diag!(
                "recipe: warning: cannot write progress file {}: {error}",
                self.path.display()
            );
        }
    }
}

fn replace_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("progress file has no file name"))?;
    let mut temporary = name.to_os_string();
    temporary.push(format!(".tmp.{}", std::process::id()));
    let temporary = path.with_file_name(temporary);
    let result = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.write_all(b"\n")?;
        file.flush()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// `2026-11-11T09:30:00Z`.
fn rfc3339(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs()) as i64;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}
