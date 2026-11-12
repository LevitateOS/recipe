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

/// A saved state is bounded by the request frame that carries it back.
const MAX_STATE: u64 = crate::worker::protocol::MAX_FRAME_LEN as u64;

/// The state root: the selected directory, or the per-user application state.
/// Everything the coordinator keeps (saved state, logs, default destinations)
/// lives under it.
pub(crate) fn state_root(selected: Option<&Path>) -> Result<PathBuf> {
    match selected {
        Some(path) => Ok(path.to_owned()),
        None => Ok(dirs::state_dir()
            .context("cannot locate application state; select --state-dir")?
            .join("recipe")),
    }
}

/// Default destination (lock and scope key) for `recipe` when the caller names
/// none: one per recipe, so unrelated recipes never share a lock or scope.
pub(crate) fn default_destination(state_root: &Path, recipe: &Path) -> Result<PathBuf> {
    let recipe = recipe
        .canonicalize()
        .with_context(|| format!("cannot resolve recipe {}", recipe.display()))?;
    let digest = Sha256::digest(recipe.as_os_str().as_encoded_bytes());
    Ok(state_root
        .join("destinations")
        .join(hex::encode(&digest[..16])))
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

impl Record {
    /// The one record format rule, for loading and for pruning.
    fn validate(&self, identity: &str) -> Result<()> {
        if self.version != 1
            || self.identity != identity
            || self.generation == 0
            || self.sha256.len() != 64
            || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            bail!("saved state identity or format mismatch");
        }
        let valid_folder = self
            .archive
            .strip_suffix(&format!("/{FILES}"))
            .is_some_and(|folder| {
                folder.starts_with(&format!("{GENERATION_PREFIX}{}-", self.generation))
                    && folder.len() <= 64
                    && folder
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            });
        if !valid_folder {
            bail!("invalid saved archive name");
        }
        Ok(())
    }

    /// The generation directory name; valid after [`Self::validate`].
    fn folder(&self) -> &str {
        self.archive
            .strip_suffix(&format!("/{FILES}"))
            .expect("validated record archive")
    }
}

// Saved-state layout under an identity directory.
const CURRENT: &str = "current.json";
const GENERATION_PREFIX: &str = "g-";
const FILES: &str = "files.tar";
const RECORD: &str = "record.json";

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

/// The SHA-256 of the file at `path`, in hex (the launcher's exact identity).
pub(crate) fn file_digest(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

impl Retained {
    pub fn open(
        root: Option<&Path>,
        destination: &Path,
        recipe: &Path,
        prepared: &PreparedInvocation,
        worker_digest: &str,
        launcher: &Path,
        options: &InvocationOptions,
    ) -> Result<Self> {
        let root = state_root(root)?;
        durable_directory(&root)?;
        let root = root.canonicalize()?;
        let mut scope = Sha256::new();
        for path in [recipe.canonicalize()?, destination.canonicalize()?] {
            let bytes = path.as_os_str().as_bytes();
            scope.update((bytes.len() as u64).to_le_bytes());
            scope.update(bytes);
        }
        let scope = root.join(hex::encode(scope.finalize()));
        // A new scope or identity directory becomes durable with the first
        // commit inside it (see `commit`); until then it holds nothing durable.
        crate::fsx::private_dir(&scope, false)?;
        let mut defines = options.defines.clone();
        defines.sort();
        // Saved state must fit the next invocation's request frame.
        let state_budget = crate::worker::protocol::restored_state_budget(options)?;
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
        // Every option is either part of the identity or deliberately not:
        // adding a field to ExecutionOptions fails to compile here until it
        // is classified.
        let crate::ExecutionOptions {
            narrowing: _,
            target_os,
            target_arch,
            jobs,
            memory_mib,
            storage_max_objects,
            storage_max_bytes,
            network_hosts,
            network_max_bytes,
            // Presentation, deadlines and main-recipe force flags do not change
            // what the saved files mean.
            machine_events: _,
            progress_file: _,
            timeout_secs: _,
            force_acquire: _,
            force_build: _,
            force_install: _,
            exact_identity,
        } = &options.execution;
        // The executing tools are named by their declared behaviour versions
        // (`crate::identity`); exact identity also names their bytes. The two
        // never share an identity: `tools` is null in one and set in the other.
        let tools = if *exact_identity {
            let launcher_digest = file_digest(launcher)
                .context("cannot read the ginux launcher for the exact identity")?;
            Some((worker_digest.to_owned(), launcher_digest))
        } else {
            None
        };
        // Requested hosts, not their resolved addresses: a DNS change must
        // not silently discard saved state.
        let identity = hex::encode(Sha256::digest(serde_json::to_vec(&(
            // v2: tools by declared versions (v1 hashed their bytes always).
            "recipe-state-v2",
            crate::identity::Declared::current(),
            &tools,
            network_hosts,
            network_max_bytes,
            &prepared.root_reference,
            prepared.contract.catalog,
            &prepared.contract.digest,
            inputs,
            defines,
            target_os,
            target_arch,
            jobs,
            memory_mib,
            storage_max_objects,
            storage_max_bytes,
        ))?));
        let directory = scope.join(&identity);
        crate::fsx::private_dir(&directory, false)?;
        let allowed = prepared
            .inputs
            .iter()
            .map(|input| crate::domain::input_path(&input.name))
            .collect::<HashSet<_>>();
        let record_path = directory.join(CURRENT);
        let current = match crate::fsx::open_regular(&record_path) {
            Ok(mut file) => {
                if file.metadata()?.len() > state_budget as u64 {
                    bail!("saved state exceeds byte bound");
                }
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                let record: Record =
                    serde_json::from_slice(&bytes).context("invalid saved state record")?;
                record.validate(&identity)?;
                if record.bytes > options.execution.archive_bound() {
                    bail!("saved state identity or format mismatch");
                }
                let folder = record.folder();
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
        retained.prune_best_effort();
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
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        let (bytes, digest) =
            crate::fsx::snapshot_hashed(&mut source, &mut output, self.archive_bound)?;
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
            .prefix(&format!("{GENERATION_PREFIX}{generation}-"))
            .tempdir_in(&self.directory)?;
        let path = directory.path().join(FILES);
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
        let (bytes, sha256) =
            crate::fsx::copy_hashed(&mut archive, &mut std::io::sink(), self.archive_bound)?;
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
            archive: format!("{folder}/{FILES}"),
            bytes,
            sha256,
            states,
        };
        let data = serde_json::to_vec(&record)?;
        if data.len() > self.state_budget {
            bail!("runtime state exceeds byte bound");
        }
        let record_path = pending.directory.path().join(RECORD);
        let mut history = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&record_path)?;
        history.write_all(&data)?;
        drop(history);
        let mut current = tempfile::NamedTempFile::new_in(&self.directory)?;
        current.write_all(&data)?;
        // Two barriers per commit. First the generation (snapshot, record and
        // their directory) and the new pointer's bytes, as one batch: nothing
        // is renamed before all of it is durable.
        crate::fsx::sync_files(&[
            (&pending.path, None),
            (&record_path, None),
            (current.path(), None),
        ])?;
        crate::fsx::sync_directories(&[pending.directory.path()])?;
        // Preserve every complete previous generation, including if publishing
        // the current pointer or delivering the worker acknowledgement fails.
        let _history_directory = pending.directory.keep();
        current.persist(self.directory.join(CURRENT))?;
        // Then the pointer, plus the identity and scope directories' own
        // entries the first time they hold a commit (otherwise already clean,
        // and their fsync returns without waiting).
        let scope = self.directory.parent().context("identity has no scope")?;
        let root = scope.parent().context("scope has no state root")?;
        crate::fsx::sync_directories(&[&self.directory, scope, root])?;
        self.current = record;
        self.prune_best_effort();
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
        // `commit` hashes the snapshot within the archive bound and makes it
        // durable with the rest of the generation.
        if !crate::fsx::reflink(&input, &output)? {
            crate::fsx::copy_hashed(&mut input, &mut output, self.archive_bound)?;
        }
        drop(output);
        self.commit(pending, crate::domain::MAIN_RECIPE, state)
    }
}

fn validate_states(states: &BTreeMap<String, Value>, allowed: &HashSet<String>) -> Result<()> {
    if states.len() > crate::worker::protocol::MAX_RESTORED_STATES
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
    record.validate(identity).ok()?;
    Some(record)
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
                Some(name) if name == FILES || name == RECORD
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
    /// Removing old generations is housekeeping: it never fails the
    /// invocation that triggered it (the newest pair is already durable).
    fn prune_best_effort(&self) {
        if let Err(error) = self.prune() {
            crate::diagnostics::diag!("recipe: warning: cannot prune saved state: {error:#}");
        }
    }

    fn prune(&self) -> Result<()> {
        let mut generations = Vec::new();
        let current = self.current.archive.split('/').next().unwrap_or("");
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir()
                || !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(GENERATION_PREFIX)
            {
                continue;
            }
            if let Some(record) = owned_record(&entry.path().join(RECORD), &self.current.identity) {
                generations.push((record.generation, entry.path()));
            } else if entry.file_name() != current
                && generation_files(&entry.path()).is_some_and(|files| {
                    files.len() == 1 && files[0].file_name().is_some_and(|name| name == FILES)
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
            let pointer = entry.path().join(CURRENT);
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
                (entry.file_name() == CURRENT && entry.file_type().is_ok_and(|kind| kind.is_file()))
                    || (entry.file_type().is_ok_and(|kind| kind.is_dir())
                        && entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(GENERATION_PREFIX)
                        && owned_record(&entry.path().join(RECORD), &identity).is_some()
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
            fs::remove_file(path.join(CURRENT))?;
            fs::remove_dir(path)?;
        }
        // No barrier: removals that a crash undoes are repeated by the next
        // prune, and the pointer they depend on was made durable by `commit`.
        Ok(())
    }
}
