//! What was done to a package store: an append-only log in the package root.
//!
//! Earlier generations cannot be the record, because `clear_earlier` deletes
//! them. Each committed change adds one JSON line to `history.jsonl`, under the
//! store's lock and after the change is in effect. A log that cannot be written
//! never undoes or fails the change; it is reported on the diagnostic stream.

use super::{PackageStore, now_seconds, readable_directory};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

const LOG: &str = "history.jsonl";
/// When the log grows past this, the oldest lines are dropped.
const MAX_LOG: u64 = 512 * 1024;
/// What a trim keeps: the newest lines that fit in this many bytes.
const KEEP_LOG: u64 = 256 * 1024;

/// One change of the store. The fields are fixed; one that does not apply is null.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// When, in seconds since 1970.
    pub at: u64,
    /// `install`, `update`, `remove`, `go-back`, `clear-earlier`, `hold` or `unhold`.
    pub what: String,
    pub package: Option<String>,
    /// The version before the change, where there was one.
    pub from: Option<String>,
    /// The version after the change, where there is one.
    pub to: Option<String>,
    /// For a recipe of a remote source: the source's name.
    pub source: Option<String>,
}

impl HistoryEntry {
    pub(super) fn new(
        what: &str,
        package: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        source: Option<&str>,
    ) -> Self {
        let text = |value: Option<&str>| value.map(str::to_owned);
        Self {
            at: now_seconds(),
            what: what.to_owned(),
            package: text(package),
            from: text(from),
            to: text(to),
            source: text(source),
        }
    }
}

/// Add `entry` to the log in `root`. The caller holds the store's lock.
pub(super) fn record(root: &Path, entry: HistoryEntry) {
    if let Err(error) = append(root, &entry) {
        crate::diagnostics::diag!(
            "recipe: warning: the change is made, but cannot be written to {LOG}: {error:#}"
        );
    }
}

fn append(root: &Path, entry: &HistoryEntry) -> Result<()> {
    let path = root.join(LOG);
    let mut line = serde_json::to_vec(entry)?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.uid() == unsafe { libc::geteuid() },
        "{} is not a regular file of this user",
        path.display()
    );
    file.write_all(&line)?;
    file.sync_all()?;
    if metadata.len() + line.len() as u64 > MAX_LOG {
        trim(root, &path)?;
    }
    Ok(())
}

/// Replace the log by its newest lines.
fn trim(root: &Path, path: &Path) -> Result<()> {
    let text = fs::read(path)?;
    let mut start = text.len().saturating_sub(KEEP_LOG as usize);
    // Begin at a line of its own.
    if start > 0 {
        match text[start - 1..].iter().position(|byte| *byte == b'\n') {
            Some(offset) => start = start - 1 + offset + 1,
            None => start = text.len(),
        }
    }
    let mut staged = tempfile::Builder::new()
        .prefix(".history-")
        .tempfile_in(root)?;
    staged.write_all(&text[start..])?;
    staged.as_file().sync_all()?;
    staged
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o644))?;
    staged.persist(path)?;
    Ok(())
}

impl PackageStore {
    /// Everything the log holds, newest first. A missing log is empty; a line
    /// that cannot be read (a partly written last one) is left out. Reading
    /// changes nothing, so an ordinary user may also read a store root owns.
    pub fn history(&self) -> Result<Vec<HistoryEntry>> {
        if !self.root.try_exists()? {
            return Ok(Vec::new());
        }
        readable_directory(&self.root)?;
        let file = match crate::fsx::open_regular(&self.root.join(LOG)) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(Vec::new());
            }
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        ensure!(
            (metadata.uid() == unsafe { libc::geteuid() } || metadata.uid() == 0)
                && metadata.mode() & 0o022 == 0,
            "history must be owned by the current user or root and not writable by others"
        );
        let mut bytes = Vec::new();
        file.take(MAX_LOG * 2).read_to_end(&mut bytes)?;
        let mut entries: Vec<HistoryEntry> = bytes
            .split(|byte| *byte == b'\n')
            .filter_map(|line| serde_json::from_slice(line).ok())
            .collect();
        entries.reverse();
        Ok(entries)
    }
}
