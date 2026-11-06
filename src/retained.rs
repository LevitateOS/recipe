//! Coordinator-owned state records paired with immutable filesystem snapshots.
use crate::inputs::PreparedInvocation;
use crate::worker::protocol::InvocationOptions;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_STATE: u64 = 4 * 1024 * 1024;

pub(crate) fn default_directory() -> Result<PathBuf> {
    Ok(dirs::state_dir()
        .context("cannot locate application state; select --state-dir")?
        .join("recipe"))
}

pub(crate) fn lock_destination(destination: &Path, timeout: Duration) -> Result<File> {
    fs::create_dir_all(destination)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(destination.join(".recipe-state.lock"))?;
    if !file.metadata()?.is_file() {
        bail!("destination lock is not a regular file");
    }
    let deadline = Instant::now() + timeout;
    let mut reported_wait = false;
    loop {
        crate::cancellation::check()?;
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if !reported_wait {
                    crate::diagnostics::line("recipe: waiting for shared destination");
                    reported_wait = true;
                }
                if Instant::now() >= deadline {
                    bail!("state destination is busy: {}", destination.display());
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error).context("cannot lock state destination"),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u32,
    identity: String,
    generation: u64,
    archive: String,
    sha256: String,
    bytes: u64,
    states: BTreeMap<String, Value>,
}

pub(crate) struct Retained {
    directory: PathBuf,
    current: Record,
    allowed: HashSet<String>,
    state_budget: usize,
    archive_bound: u64,
}
pub(crate) struct Pending {
    directory: tempfile::TempDir,
    pub path: PathBuf,
    pub generation: u64,
}

impl Retained {
    pub fn open(
        root: Option<&Path>,
        destination: &Path,
        recipe: &Path,
        prepared: &PreparedInvocation,
        worker_digest: &str,
        options: &InvocationOptions,
    ) -> Result<Self> {
        let root = match root {
            Some(path) => path.to_owned(),
            None => default_directory()?,
        };
        durable_directory(&root)?;
        let root = root.canonicalize()?;
        let mut scope = Sha256::new();
        for path in [recipe.canonicalize()?, destination.canonicalize()?] {
            let bytes = path.as_os_str().as_bytes();
            scope.update((bytes.len() as u64).to_le_bytes());
            scope.update(bytes);
        }
        let scope = root.join(hex::encode(scope.finalize()));
        crate::fsx::private_dir(&scope, true)?;
        let mut defines = options.defines.clone();
        defines.sort();
        // Reserve room for options, the longest cleanup reason and framing on
        // the next invocation, not just for the record being written now.
        let state_budget = crate::worker::protocol::MAX_FRAME_LEN
            .saturating_sub(serde_json::to_vec(&defines)?.len() + 8192);
        let inputs = prepared
            .inputs
            .iter()
            .map(|input| {
                (
                    &input.name,
                    &input.kind,
                    input.sha256,
                    input.size,
                    input.mode,
                )
            })
            .collect::<Vec<_>>();
        let identity = hex::encode(Sha256::digest(serde_json::to_vec(&(
            "recipe-state-v1-transport-v6",
            &prepared.network,
            &prepared.root_reference,
            worker_digest,
            inputs,
            defines,
            &options.execution.target_os,
            &options.execution.target_arch,
            options.execution.jobs,
            options.execution.memory_mib,
            options.execution.storage_max_objects,
            options.execution.storage_max_bytes,
        ))?));
        let directory = scope.join(&identity);
        crate::fsx::private_dir(&directory, true)?;
        let allowed = prepared
            .inputs
            .iter()
            .map(|input| crate::domain::input_path(&input.name))
            .collect::<HashSet<_>>();
        let record_path = directory.join("current.json");
        let current = match crate::fsx::open_regular(&record_path) {
            Ok(mut file) => {
                if file.metadata()?.len() > MAX_STATE.min(state_budget as u64) {
                    bail!("saved state exceeds byte bound");
                }
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                let record: Record =
                    serde_json::from_slice(&bytes).context("invalid saved state record")?;
                if record.version != 1
                    || record.identity != identity
                    || record.generation == 0
                    || record.bytes > options.execution.archive_bound()
                    || record.sha256.len() != 64
                    || !record.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    bail!("saved state identity or format mismatch");
                }
                let Some(folder) = record.archive.strip_suffix("/files.tar") else {
                    bail!("invalid saved archive name");
                };
                if !folder.starts_with(&format!("g-{}-", record.generation))
                    || folder.len() > 64
                    || !folder
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    bail!("invalid saved archive name");
                }
                if fs::symlink_metadata(directory.join(folder))?
                    .file_type()
                    .is_symlink()
                {
                    bail!("saved generation directory is a symlink");
                }
                validate_states(&record.states, &allowed)?;
                record
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                Record {
                    version: 1,
                    identity,
                    generation: 0,
                    archive: String::new(),
                    sha256: String::new(),
                    bytes: 0,
                    states: BTreeMap::new(),
                }
            }
            Err(error) => return Err(error),
        };
        let retained = Self {
            directory,
            current,
            allowed,
            state_budget,
            archive_bound: options.execution.archive_bound(),
        };
        retained.prune()?;
        Ok(retained)
    }

    pub fn states(&self) -> &BTreeMap<String, Value> {
        &self.current.states
    }

    /// Hash the same bytes copied to the private restore input, never verify one
    /// pathname read and then let the launcher consume a second mutable read.
    pub fn restore_input(&self, path: &Path) -> Result<bool> {
        if self.current.generation == 0 {
            return Ok(false);
        }
        let mut source = crate::fsx::open_regular(&self.directory.join(&self.current.archive))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        let (bytes, digest) = crate::fsx::copy_hashed(&mut source, &mut output, self.archive_bound)?;
        if bytes != self.current.bytes || digest != self.current.sha256 {
            bail!("saved filesystem snapshot digest mismatch");
        }
        Ok(true)
    }

    pub fn begin(&self) -> Result<Pending> {
        let generation = self
            .current
            .generation
            .checked_add(1)
            .context("state generation overflow")?;
        let directory = tempfile::Builder::new()
            .prefix(&format!("g-{generation}-"))
            .tempdir_in(&self.directory)?;
        let path = directory.path().join("files.tar");
        if path.as_os_str().as_bytes().len() > 512 {
            bail!("state snapshot destination exceeds launcher path bound");
        }
        Ok(Pending {
            directory,
            path,
            generation,
        })
    }

    pub fn commit(&mut self, pending: Pending, recipe: &str, state: Value) -> Result<u64> {
        if !self.allowed.contains(recipe) || !state.is_object() {
            bail!("checkpoint names an unknown recipe or invalid ctx.state");
        }
        let mut states = self.current.states.clone();
        states.insert(recipe.to_owned(), state);
        validate_states(&states, &self.allowed)?;
        let mut archive = crate::fsx::open_regular(&pending.path)?;
        let (bytes, sha256) = crate::fsx::copy_hashed(&mut archive, &mut std::io::sink(), self.archive_bound)?;
        let folder = pending
            .directory
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .context("state path is not UTF-8")?;
        let record = Record {
            version: 1,
            identity: self.current.identity.clone(),
            generation: pending.generation,
            archive: format!("{folder}/files.tar"),
            bytes,
            sha256,
            states,
        };
        let data = serde_json::to_vec(&record)?;
        if data.len() as u64 > MAX_STATE || data.len() > self.state_budget {
            bail!("runtime state exceeds byte bound");
        }
        let mut history = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(pending.directory.path().join("record.json"))?;
        history.write_all(&data)?;
        history.sync_all()?;
        File::open(pending.directory.path())?.sync_all()?;
        // Preserve every complete previous generation, including if publishing
        // the current pointer or delivering the worker acknowledgement fails.
        let _history_directory = pending.directory.keep();
        let mut current = tempfile::NamedTempFile::new_in(&self.directory)?;
        current.write_all(&data)?;
        current.as_file().sync_all()?;
        current.persist(self.directory.join("current.json"))?;
        File::open(&self.directory)?.sync_all()?;
        self.current = record;
        self.prune()?;
        Ok(self.current.generation)
    }

    pub fn finish(&mut self, export: &Path, state: Value) -> Result<u64> {
        let pending = self.begin()?;
        let mut input = crate::fsx::open_regular(export)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&pending.path)?;
        crate::fsx::copy_hashed(&mut input, &mut output, self.archive_bound)?;
        output.sync_all()?;
        drop(output);
        self.commit(pending, crate::domain::MAIN_RECIPE, state)
    }
}

fn validate_states(states: &BTreeMap<String, Value>, allowed: &HashSet<String>) -> Result<()> {
    if states.len() > 1024
        || states
            .iter()
            .any(|(name, value)| !allowed.contains(name) || !value.is_object())
    {
        bail!("saved runtime state names unknown recipes or invalid state objects");
    }
    Ok(())
}
fn durable_directory(path: &Path) -> Result<()> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    if path.is_dir() {
        return Ok(());
    }
    let parent = path.parent().context("state directory has no parent")?;
    durable_directory(parent)?;
    crate::fsx::private_dir(&path, true)
}

// Per destination/recipe scope: retain the current and three recent input
// identities, each with its current and previous complete filesystem generation.
// The caller holds the existing destination lock throughout this operation.
const RETAIN_IDENTITIES: usize = 4;
const RETAIN_GENERATIONS: usize = 2;

fn owned_record(path: &Path, identity: &str) -> Option<Record> {
    let mut file = crate::fsx::open_regular(path).ok()?;
    if file.metadata().ok()?.len() > MAX_STATE {
        return None;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let record: Record = serde_json::from_slice(&bytes).ok()?;
    (record.version == 1 && record.identity == identity && record.generation != 0).then_some(record)
}
fn generation_files(path: &Path) -> Option<Vec<PathBuf>> {
    let entries = fs::read_dir(path)
        .ok()?
        .collect::<std::io::Result<Vec<_>>>()
        .ok()?;
    if entries.is_empty()
        || entries.iter().any(|entry| {
            !matches!(
                entry.file_name().to_str(),
                Some("files.tar" | "record.json")
            ) || !entry.file_type().is_ok_and(|kind| kind.is_file())
        })
    {
        return None;
    }
    Some(entries.into_iter().map(|entry| entry.path()).collect())
}
fn remove_generation(path: &Path) -> Result<bool> {
    let Some(files) = generation_files(path) else {
        return Ok(false);
    };
    for file in files {
        fs::remove_file(file)?;
    }
    fs::remove_dir(path)?;
    Ok(true)
}
impl Retained {
    fn prune(&self) -> Result<()> {
        let mut generations = Vec::new();
        let current = self.current.archive.split('/').next().unwrap_or("");
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir()
                || !entry.file_name().to_string_lossy().starts_with("g-")
            {
                continue;
            }
            if let Some(record) =
                owned_record(&entry.path().join("record.json"), &self.current.identity)
            {
                generations.push((record.generation, entry.path()));
            } else if entry.file_name() != current
                && generation_files(&entry.path()).is_some_and(|files| {
                    files.len() == 1 && files[0].file_name().is_some_and(|name| name == "files.tar")
                })
            {
                // A crashed, never-committed pending snapshot; no current record
                // refers to it, and the destination lock excludes active writers.
                remove_generation(&entry.path())?;
            }
        }
        generations.sort_by_key(|(generation, _)| std::cmp::Reverse(*generation));
        let mut kept = 0;
        for (_, path) in generations {
            if path.file_name().is_some_and(|name| name == current) {
                continue;
            }
            if kept < RETAIN_GENERATIONS - 1 {
                kept += 1;
            } else {
                remove_generation(&path)?;
            }
        }
        let scope = self.directory.parent().context("identity has no scope")?;
        let mut identities = Vec::new();
        for entry in fs::read_dir(scope)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if entry.path() == self.directory
                || !entry.file_type()?.is_dir()
                || name.len() != 64
                || !name.bytes().all(|b| b.is_ascii_hexdigit())
            {
                continue;
            }
            let pointer = entry.path().join("current.json");
            if owned_record(&pointer, name).is_some() {
                identities.push((
                    fs::metadata(&pointer)?.modified()?,
                    entry.path(),
                    name.to_owned(),
                ));
            }
        }
        identities.sort_by_key(|(time, _, _)| std::cmp::Reverse(*time));
        for (_, path, identity) in identities.into_iter().skip(RETAIN_IDENTITIES - 1) {
            let entries = fs::read_dir(&path)?.collect::<std::io::Result<Vec<_>>>()?;
            // Foreign or manually added data is never recursively removed.
            let owned = entries.iter().all(|entry| {
                (entry.file_name() == "current.json"
                    && entry.file_type().is_ok_and(|kind| kind.is_file()))
                    || (entry.file_type().is_ok_and(|kind| kind.is_dir())
                        && entry.file_name().to_string_lossy().starts_with("g-")
                        && owned_record(&entry.path().join("record.json"), &identity).is_some()
                        && generation_files(&entry.path()).is_some())
            });
            if !owned {
                continue;
            }
            for entry in entries {
                if entry.file_type()?.is_dir() {
                    remove_generation(&entry.path())?;
                }
            }
            fs::remove_file(path.join("current.json"))?;
            fs::remove_dir(path)?;
        }
        File::open(&self.directory)?.sync_all()?;
        File::open(scope)?.sync_all()?;
        Ok(())
    }
}
