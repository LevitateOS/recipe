//! Coordinator-owned state records paired with immutable filesystem snapshots.
//!
//! These records are how Recipe knows what is done (docs/recipe-format.md,
//! section 6.2), never from recipe code. Per recipe and destination (a
//! scope) and per build identity, each generation is one archive of the
//! run's `work` and `out` with its record: the archive's SHA-256 and length,
//! the saved values, how far the run got ([`Reached`]: an unfinished
//! checkpoint, the downloads, or a finished result with its outputs) and
//! the state key ([`crate::state::key`]). A generation counts only while its
//! archive still has the recorded SHA-256 and length.
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

/// How far the run that made a generation got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reached {
    /// A `checkpoint(name)` inside `build` or `configure`: an unfinished
    /// run, after its download step.
    Checkpoint,
    /// `recipe download`: the download step finished.
    Downloaded,
    /// The build step finished: the kept result, with its outputs.
    Built,
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
    reached: Reached,
    /// A finished result's outputs: each declared output to its domain
    /// path. Empty for the other generations.
    outputs: BTreeMap<String, String>,
    /// The state key of the run that made it ([`crate::state::key`]).
    key: String,
}

impl Record {
    /// The one record format rule, for loading and for pruning.
    fn validate(&self, identity: &str) -> Result<()> {
        if self.version != RECORD_VERSION
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
        let outputs_valid = (self.reached == Reached::Built) != self.outputs.is_empty()
            && self.outputs.len() <= crate::worker::protocol::MAX_OUTPUTS
            && self.outputs.iter().all(|(name, path)| {
                crate::domain::valid_name(name) && *path == crate::domain::output_path(name)
            });
        if !outputs_valid
            || self.key.len() != 64
            || !self.key.bytes().all(|b| b.is_ascii_hexdigit())
        {
            bail!("saved state outputs or key mismatch");
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

/// The record format; another number is not read.
const RECORD_VERSION: u32 = 2;

// Saved-state layout under an identity directory.
const CURRENT: &str = "current.json";
const GENERATION_PREFIX: &str = "g-";
const FILES: &str = "files.tar";
const RECORD: &str = "record.json";

pub(crate) struct Retained {
    directory: PathBuf,
    /// The state key the next generation records.
    key: String,
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
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        root: Option<&Path>,
        destination: &Path,
        recipe: &Path,
        key: &str,
        prepared: &PreparedInvocation,
        worker_digest: &str,
        launcher: Option<&Path>,
        options: &InvocationOptions,
        host_root: Option<&Path>,
    ) -> Result<Self> {
        let root = state_root(root)?;
        durable_directory(&root)?;
        let root = root.canonicalize()?;
        let (recipe, destination) = (recipe.canonicalize()?, destination.canonicalize()?);
        let scope = root.join(scope_name(&recipe, &destination));
        // A new scope or identity directory becomes durable with the first
        // commit inside it (see `commit`); until then it holds nothing durable.
        crate::fsx::private_dir(&scope, false)?;
        write_scope_about(&scope, &recipe, &destination)?;
        let mut defines = options.defines.clone();
        defines.sort();
        // Saved state must fit the next invocation's request frame.
        let mut state_budget = crate::worker::protocol::restored_state_budget(options)?;
        // A host run's request also carries its directory and network pins.
        if let Some(host_root) = host_root {
            let host = crate::worker::protocol::HostRun {
                root: host_root.to_string_lossy().into_owned(),
                network: prepared.network.host_grants(),
                network_max_bytes: options.execution.network_max_bytes,
                cache: crate::host::offline_cache_text()?,
            };
            let reserve = serde_json::to_vec(&host)?.len() + r#","host":"#.len();
            state_budget = state_budget.saturating_sub(reserve);
        }
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
            // Presentation and deadlines do not change what the saved files
            // mean.
            machine_events: _,
            progress_file: _,
            timeout_secs: _,
            exact_identity,
            // Shown to recipes as HOST_LIBC; the saved files of one
            // store belong to one system.
            host_libc: _,
            package_prefix,
            // The mode that ran enters below: a host run has an identity of
            // its own, and a Ginux run's identity is the one it always was.
            execution: _,
        } = &options.execution;
        // The executing tools are named by their declared behaviour versions
        // (`crate::identity`); exact identity also names their bytes. The two
        // never share an identity: `tools` is null in one and set in the other.
        let tools = match (*exact_identity, launcher) {
            (false, _) => None,
            (true, Some(launcher)) => {
                let launcher_digest = file_digest(launcher)
                    .context("cannot read the ginux launcher for the exact identity")?;
                Some((worker_digest.to_owned(), launcher_digest))
            }
            // A host run has no launcher; the worker alone executes it.
            (true, None) => Some((worker_digest.to_owned(), String::new())),
        };
        let inputs_value = serde_json::to_value(&inputs)?;
        // Requested hosts, not their resolved addresses: a DNS change must
        // not silently discard saved state.
        let identity = if let Some(host_root) = host_root {
            // A host run: no root image and no Ginux contract, its own
            // declared behaviour, and the directory its saved paths name.
            Sha256::digest(serde_json::to_vec(&(
                "recipe-state-v2-host",
                crate::identity::Declared::current(),
                crate::identity::HOST_BEHAVIOUR,
                &tools,
                host_root,
                network_hosts,
                network_max_bytes,
                &inputs_value,
                &defines,
                target_os,
                target_arch,
                jobs,
                memory_mib,
                storage_max_objects,
                storage_max_bytes,
            ))?)
        } else {
            let contract = prepared
                .contract
                .as_ref()
                .context("a Ginux run carries its contract")?;
            Sha256::digest(serde_json::to_vec(&(
                // v2: tools by declared versions (v1 hashed their bytes always).
                "recipe-state-v2",
                crate::identity::Declared::current(),
                &tools,
                network_hosts,
                network_max_bytes,
                &prepared.root_reference,
                contract.catalog,
                &contract.digest,
                inputs,
                defines,
                target_os,
                target_arch,
                jobs,
                memory_mib,
                storage_max_objects,
                storage_max_bytes,
            ))?)
        };
        // The package's folder is written into what a build carries
        // (PREFIX), so a build belongs to the store it was
        // made for: installing the same recipe into another store builds
        // again. Without a prefix (--no-activate, dependencies) the identity
        // is the one it always was, so producer states stay valid.
        let identity = hex::encode(match package_prefix {
            None => identity,
            Some(prefix) => Sha256::digest(serde_json::to_vec(&(
                "recipe-state-v2-prefix",
                hex::encode(identity),
                prefix,
            ))?),
        });
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
                    version: RECORD_VERSION,
                    identity,
                    generation: 0,
                    archive: String::new(),
                    sha256: String::new(),
                    bytes: 0,
                    states: BTreeMap::new(),
                    reached: Reached::Checkpoint,
                    outputs: BTreeMap::new(),
                    key: key.to_owned(),
                }
            }
            Err(error) => return Err(error),
        };
        let retained = Self {
            directory,
            key: key.to_owned(),
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

    /// How far the current generation got; None when there is none.
    pub fn reached(&self) -> Option<Reached> {
        (self.current.generation != 0).then_some(self.current.reached)
    }

    /// The current generation's outputs (a finished result's).
    pub fn outputs(&self) -> &BTreeMap<String, String> {
        &self.current.outputs
    }

    /// Whether the current generation's archive still has the SHA-256 and
    /// length its record names. A changed archive is reported and counts as
    /// nothing: the run starts from nothing and its next generation replaces
    /// the record.
    pub fn verify_current(&self) -> Result<bool> {
        if self.current.generation == 0 {
            return Ok(false);
        }
        let mut source = match crate::fsx::open_regular(&self.directory.join(&self.current.archive))
        {
            Ok(source) => source,
            Err(error) => return Ok(self.changed(&error)),
        };
        match crate::fsx::copy_hashed(&mut source, &mut std::io::sink(), self.archive_bound) {
            Ok((bytes, digest)) if bytes == self.current.bytes && digest == self.current.sha256 => {
                Ok(true)
            }
            Ok(_) => Ok(self.changed(&anyhow::anyhow!("its SHA-256 or length differs"))),
            Err(error) => Ok(self.changed(&error)),
        }
    }

    fn changed(&self, why: &anyhow::Error) -> bool {
        crate::diagnostics::diag!(
            "recipe: warning: the kept files of generation {} changed since Recipe recorded them \
             ({why:#}); they are not used, and the run starts from nothing",
            self.current.generation
        );
        false
    }

    /// Copy the current generation's archive to the private restore input
    /// `path` and check the copied bytes against the record (never verify one
    /// pathname read and then let the launcher consume a second mutable
    /// read). True when `path` holds it; false when there is no generation or
    /// its archive changed ([`Self::verify_current`]), and `path` is left
    /// absent.
    pub fn restore_input(&self, path: &Path) -> Result<bool> {
        if self.current.generation == 0 {
            return Ok(false);
        }
        let mut source = match crate::fsx::open_regular(&self.directory.join(&self.current.archive))
        {
            Ok(source) => source,
            Err(error) => return Ok(self.changed(&error)),
        };
        let mut output = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        let copied = crate::fsx::snapshot_hashed(&mut source, &mut output, self.archive_bound);
        drop(output);
        match copied {
            Ok((bytes, digest)) if bytes == self.current.bytes && digest == self.current.sha256 => {
                Ok(true)
            }
            other => {
                fs::remove_file(path)?;
                let why = match other {
                    Err(error) => error,
                    Ok(_) => anyhow::anyhow!("its SHA-256 or length differs"),
                };
                Ok(self.changed(&why))
            }
        }
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

    /// Keep a checkpoint: an unfinished run's files with its saved state.
    pub fn commit(&mut self, pending: Pending, recipe: &str, state: Value) -> Result<u64> {
        self.commit_as(pending, recipe, state, Reached::Checkpoint, BTreeMap::new())
    }

    fn commit_as(
        &mut self,
        pending: Pending,
        recipe: &str,
        state: Value,
        reached: Reached,
        outputs: BTreeMap<String, String>,
    ) -> Result<u64> {
        if !self.allowed.contains(recipe) || !state.is_object() {
            bail!("checkpoint names an unknown recipe or an invalid saved state");
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
            version: RECORD_VERSION,
            identity: self.current.identity.clone(),
            generation: pending.generation,
            archive: format!("{folder}/{FILES}"),
            bytes,
            sha256,
            states,
            reached,
            outputs,
            key: self.key.clone(),
        };
        record.validate(&self.current.identity)?;
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

    /// Keep the end of a run: the downloads of `recipe download`
    /// ([`Reached::Downloaded`], no outputs) or a finished result
    /// ([`Reached::Built`] with its outputs).
    pub fn finish(
        &mut self,
        export: &Path,
        state: Value,
        reached: Reached,
        outputs: BTreeMap<String, String>,
    ) -> Result<u64> {
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
        self.commit_as(pending, crate::domain::MAIN_RECIPE, state, reached, outputs)
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

// C4b: saved state only while a run is unfinished and one copy at most
// (docs/recipe-format.md, section 7 and 6.5) replaces these counts.
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

/// The scope directory name of `recipe` and `destination` (both canonical).
fn scope_name(recipe: &Path, destination: &Path) -> String {
    let mut scope = Sha256::new();
    for path in [recipe, destination] {
        let bytes = path.as_os_str().as_bytes();
        scope.update((bytes.len() as u64).to_le_bytes());
        scope.update(bytes);
    }
    hex::encode(scope.finalize())
}

/// What a scope is for: its recipe and destination, so that a clean-up over
/// every scope finds the destination lock and a report names the program.
const SCOPE_ABOUT: &str = "scope.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeAbout {
    recipe: PathBuf,
    destination: PathBuf,
}

fn write_scope_about(scope: &Path, recipe: &Path, destination: &Path) -> Result<()> {
    let path = scope.join(SCOPE_ABOUT);
    if crate::fsx::open_regular(&path).is_ok() {
        return Ok(());
    }
    let mut file = tempfile::NamedTempFile::new_in(scope)?;
    file.write_all(&serde_json::to_vec(&ScopeAbout {
        recipe: recipe.to_owned(),
        destination: destination.to_owned(),
    })?)?;
    file.persist(path)?;
    Ok(())
}

/// One scope: what Recipe keeps for one recipe at one destination.
pub(crate) struct Scope {
    pub directory: PathBuf,
    pub recipe: PathBuf,
    pub destination: PathBuf,
}

impl Scope {
    /// The scope of `recipe` at `destination`, when Recipe keeps anything
    /// for it (the state root, the destination and the scope exist).
    pub fn of(state_dir: Option<&Path>, recipe: &Path, destination: &Path) -> Result<Option<Self>> {
        let Ok(root) = state_root(state_dir)?.canonicalize() else {
            return Ok(None);
        };
        let (Ok(recipe), Ok(destination)) = (recipe.canonicalize(), destination.canonicalize())
        else {
            return Ok(None);
        };
        let directory = root.join(scope_name(&recipe, &destination));
        Ok(fs::symlink_metadata(&directory)
            .is_ok_and(|metadata| metadata.is_dir())
            .then_some(Self {
                directory,
                recipe,
                destination,
            }))
    }

    /// Every scope under the state root that names its recipe and
    /// destination.
    pub fn all(state_dir: Option<&Path>) -> Result<Vec<Self>> {
        let Ok(root) = state_root(state_dir)?.canonicalize() else {
            return Ok(Vec::new());
        };
        let mut scopes = Vec::new();
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.len() != 64
                || !name.bytes().all(|b| b.is_ascii_hexdigit())
                || !entry.file_type()?.is_dir()
            {
                continue;
            }
            let Ok(mut file) = crate::fsx::open_regular(&entry.path().join(SCOPE_ABOUT)) else {
                continue;
            };
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            let Ok(about) = serde_json::from_slice::<ScopeAbout>(&bytes) else {
                continue;
            };
            scopes.push(Self {
                directory: entry.path(),
                recipe: about.recipe,
                destination: about.destination,
            });
        }
        Ok(scopes)
    }

    /// Every generation the scope holds, of every identity.
    pub fn generations(&self) -> Result<Vec<Generation>> {
        let mut found = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(identity) = name.to_str() else {
                continue;
            };
            if identity.len() != 64
                || !identity.bytes().all(|b| b.is_ascii_hexdigit())
                || !entry.file_type()?.is_dir()
            {
                continue;
            }
            let current = owned_record(&entry.path().join(CURRENT), identity)
                .map(|record| record.folder().to_owned());
            for generation in fs::read_dir(entry.path())? {
                let generation = generation?;
                let folder = generation.file_name().to_string_lossy().into_owned();
                if !folder.starts_with(GENERATION_PREFIX) || !generation.file_type()?.is_dir() {
                    continue;
                }
                let Some(record) = owned_record(&generation.path().join(RECORD), identity) else {
                    continue;
                };
                if record.folder() != folder || generation_files(&generation.path()).is_none() {
                    continue;
                }
                found.push(Generation {
                    current: current.as_deref() == Some(folder.as_str()),
                    identity: entry.path(),
                    archive: entry.path().join(&record.archive),
                    record,
                });
            }
        }
        Ok(found)
    }

    /// How far the current generation of an identity whose state key is
    /// `key` got, counting only one whose archive still has the recorded
    /// SHA-256 and length (each archive is read once to check it).
    pub fn reached(&self, key: &str) -> Result<Option<Reached>> {
        let mut best = None;
        for generation in self.generations()? {
            if !generation.current
                || generation.record.key != key
                || best.is_some_and(|best| best >= generation.record.reached)
            {
                continue;
            }
            if generation.intact() {
                best = Some(generation.record.reached);
            }
        }
        Ok(best)
    }

    /// Delete the generations of the kinds in `kinds`, holding the
    /// destination's lock (a run of this recipe holds it while it runs).
    /// Where an identity's current generation goes, its newest remaining one
    /// becomes current; an identity left with none is removed. Returns the
    /// bytes freed.
    pub fn remove(&self, kinds: &[Reached]) -> Result<u64> {
        let _lock =
            lock_destination(&self.destination, Duration::from_secs(10)).with_context(|| {
                format!(
                    "{} is in use by a run of {}; try again when it ends",
                    self.destination.display(),
                    self.recipe.display()
                )
            })?;
        let mut freed = 0;
        let mut identities = HashSet::new();
        for generation in self.generations()? {
            identities.insert(generation.identity.clone());
            if !kinds.contains(&generation.record.reached) {
                continue;
            }
            let folder = generation.identity.join(generation.record.folder());
            let size = generation_files(&folder)
                .unwrap_or_default()
                .iter()
                .filter_map(|file| fs::symlink_metadata(file).ok())
                .map(|metadata| metadata.len())
                .sum::<u64>();
            if remove_generation(&folder)? {
                freed += size;
            }
        }
        for identity in identities {
            let name = identity
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned();
            let pointer = identity.join(CURRENT);
            let current_gone = owned_record(&pointer, &name)
                .is_none_or(|record| !identity.join(record.folder()).is_dir());
            if !current_gone {
                continue;
            }
            let newest = fs::read_dir(&identity)?
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(GENERATION_PREFIX)
                })
                .filter_map(|entry| owned_record(&entry.path().join(RECORD), &name))
                .max_by_key(|record| record.generation);
            match newest {
                Some(record) => {
                    let mut file = tempfile::NamedTempFile::new_in(&identity)?;
                    file.write_all(&serde_json::to_vec(&record)?)?;
                    crate::fsx::sync_files(&[(file.path(), None)])?;
                    file.persist(&pointer)?;
                    crate::fsx::sync_directories(&[identity.as_path()])?;
                }
                None => {
                    match fs::remove_file(&pointer) {
                        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                            return Err(error.into());
                        }
                        _ => {}
                    }
                    // Only an empty identity directory goes; anything else in
                    // it is not Recipe's to remove.
                    let _ = fs::remove_dir(&identity);
                }
            }
        }
        Ok(freed)
    }
}

/// One kept generation of a scope.
pub(crate) struct Generation {
    /// It is the current generation of its identity.
    pub current: bool,
    identity: PathBuf,
    archive: PathBuf,
    record: Record,
}

impl Generation {
    pub fn reached(&self) -> Reached {
        self.record.reached
    }

    /// The bytes it holds on disk (its archive).
    pub fn bytes(&self) -> u64 {
        self.record.bytes
    }

    /// Its archive still has the SHA-256 and length its record names.
    fn intact(&self) -> bool {
        let Ok(mut file) = crate::fsx::open_regular(&self.archive) else {
            return false;
        };
        crate::fsx::copy_hashed(&mut file, &mut std::io::sink(), u64::MAX)
            .is_ok_and(|(bytes, digest)| bytes == self.record.bytes && digest == self.record.sha256)
    }
}

/// Where a dependency of the recipe run at `destination` keeps its lock and
/// saved state: a destination of its own below the consumer's.
pub(crate) fn dependency_destination(destination: &Path, recipe: &Path) -> PathBuf {
    let digest = hex::encode(Sha256::digest(recipe.as_os_str().as_encoded_bytes()));
    destination.join(".deps").join(&digest[..32])
}
