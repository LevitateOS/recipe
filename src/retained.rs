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
use anyhow::{Context, Result, bail, ensure};
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
        // Only a finished result names outputs; a recipe that declares none
        // keeps a finished result without them.
        let outputs_valid = (self.reached == Reached::Built || self.outputs.is_empty())
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
    /// This is the scope's download slot (`recipe download`): its directory
    /// is named by the state key, not by the full identity.
    downloads: bool,
    /// For a build or install: the download slot of the same state key
    /// (its directory and its current Downloaded record), if one is kept.
    download: Option<(PathBuf, Record)>,
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
        downloads: bool,
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
            };
            let cache = crate::host::offline_cache_text()?;
            let reserve = serde_json::to_vec(&host)?.len()
                + r#","host":"#.len()
                + serde_json::to_vec(&cache)?.len();
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
            // Shown to recipes as HOST_LIBC: what a build fetches and
            // places depends on it.
            host_libc,
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
                host_libc,
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
                (defines, host_libc),
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
        // `recipe download` runs the main recipe alone, without its
        // dependencies' outputs, so its full identity is never a build's.
        // Its downloads are kept in the scope's download slot, named by the
        // state key (which names the sources and pins), where a build or
        // install of the same key finds them.
        if downloads {
            ensure!(
                key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()),
                "a download's state key is missing"
            );
        }
        let download_slot = scope.join(key);
        let identity = if downloads { key.to_owned() } else { identity };
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
        let download = (!downloads && download_slot != directory)
            .then(|| {
                let record = owned_record(&download_slot.join(CURRENT), key)?;
                (record.reached == Reached::Downloaded
                    && record.bytes <= options.execution.archive_bound()
                    && validate_states(&record.states, &allowed).is_ok()
                    && fs::symlink_metadata(download_slot.join(record.folder()))
                        .is_ok_and(|metadata| metadata.is_dir()))
                .then_some((download_slot, record))
            })
            .flatten();
        let retained = Self {
            directory,
            key: key.to_owned(),
            current,
            downloads,
            download,
            allowed,
            state_budget,
            archive_bound: options.execution.archive_bound(),
        };
        // Other identities stay until this one keeps a generation of its own
        // (`commit_as`): a run that fails before that leaves the saved state
        // it found.
        retained.prune_best_effort(false);
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
            Err(error) => return Ok(changed(&self.current, &error)),
        };
        match crate::fsx::copy_hashed(&mut source, &mut std::io::sink(), self.archive_bound) {
            Ok((bytes, digest)) if bytes == self.current.bytes && digest == self.current.sha256 => {
                Ok(true)
            }
            Ok(_) => Ok(changed(
                &self.current,
                &anyhow::anyhow!("its SHA-256 or length differs"),
            )),
            Err(error) => Ok(changed(&self.current, &error)),
        }
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
        restore_record(&self.directory, &self.current, path, self.archive_bound)
    }

    /// Whether a build or install of this state key finds kept downloads
    /// (`recipe download`) in the scope's download slot.
    pub fn has_download(&self) -> bool {
        self.download.is_some()
    }

    /// The saved values of the kept downloads.
    pub fn download_states(&self) -> BTreeMap<String, Value> {
        self.download
            .as_ref()
            .map(|(_, record)| record.states.clone())
            .unwrap_or_default()
    }

    /// [`Self::restore_input`] from the download slot of this state key. The
    /// slot stays: the downloads are kept beside a build's generations until
    /// they are cleaned up (`recipe cleanup --downloads`, or after install).
    pub fn restore_download(&self, path: &Path) -> Result<bool> {
        match &self.download {
            Some((directory, record)) => {
                restore_record(directory, record, path, self.archive_bound)
            }
            None => Ok(false),
        }
    }

    /// A `RERUN` recipe's successful run: its result is never kept, and an
    /// unfinished run it checkpointed is finished, so nothing of this
    /// identity stays to be resumed. The caller holds the destination lock.
    pub fn discard(&mut self) -> Result<()> {
        if self.current.generation == 0 {
            return Ok(());
        }
        for entry in fs::read_dir(&self.directory)?.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir())
                && entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(GENERATION_PREFIX)
            {
                remove_generation(&entry.path())?;
            }
        }
        match fs::remove_file(self.directory.join(CURRENT)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(error.into());
            }
            _ => {}
        }
        self.current.generation = 0;
        self.current.states.clear();
        self.current.outputs.clear();
        Ok(())
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
        self.prune_best_effort(true);
        Ok(self.current.generation)
    }

    /// Keep the end of a run: the downloads of `recipe download`
    /// ([`Reached::Downloaded`], no outputs) or a finished result
    /// ([`Reached::Built`] with its outputs). The run's archive at `export`
    /// is moved into the generation, so it is not there afterwards. Only
    /// where the state root is on another filesystem than the run's scratch
    /// directory (`crate::temporary::pool`) is it cloned or copied instead
    /// (a rename cannot cross filesystems).
    pub fn finish(
        &mut self,
        export: &Path,
        state: Value,
        reached: Reached,
        outputs: BTreeMap<String, String>,
    ) -> Result<u64> {
        let pending = self.begin()?;
        match fs::rename(export, &pending.path) {
            Ok(()) => {
                fs::set_permissions(
                    &pending.path,
                    std::os::unix::fs::PermissionsExt::from_mode(0o600),
                )?;
            }
            Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
                let mut input = crate::fsx::open_regular(export)?;
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&pending.path)?;
                if !crate::fsx::reflink(&input, &output)? {
                    crate::fsx::copy_hashed(&mut input, &mut output, self.archive_bound)?;
                }
                drop(output);
            }
            Err(error) => return Err(error).context("cannot keep the run's archive"),
        }
        // `commit` hashes the snapshot within the archive bound and makes it
        // durable with the rest of the generation.
        self.commit_as(pending, crate::domain::MAIN_RECIPE, state, reached, outputs)
    }
}

fn changed(record: &Record, why: &anyhow::Error) -> bool {
    crate::diagnostics::diag!(
        "recipe: warning: the kept files of generation {} changed since Recipe recorded them \
         ({why:#}); they are not used, and the run starts from nothing",
        record.generation
    );
    false
}

/// Copy the archive of `record` (in `directory`) to the restore input `path`
/// and check the copy against the record; see [`Retained::restore_input`].
fn restore_record(directory: &Path, record: &Record, path: &Path, bound: u64) -> Result<bool> {
    let mut source = match crate::fsx::open_regular(&directory.join(&record.archive)) {
        Ok(source) => source,
        Err(error) => return Ok(changed(record, &error)),
    };
    let mut output = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let copied = crate::fsx::snapshot_hashed(&mut source, &mut output, bound);
    drop(output);
    match copied {
        Ok((bytes, digest)) if bytes == record.bytes && digest == record.sha256 => Ok(true),
        other => {
            fs::remove_file(path)?;
            let why = match other {
                Err(error) => error,
                Ok(_) => anyhow::anyhow!("its SHA-256 or length differs"),
            };
            Ok(changed(record, &why))
        }
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

// Saved state is one copy at most (docs/recipe-format.md, sections 6.5 and
// 7): per scope (recipe and destination) only the current generation of the
// identity that ran last is kept, and beside it the downloads of the state
// key last downloaded (the download slot). The caller holds the destination
// lock throughout pruning.

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
    /// invocation that triggered it (the current generation is already
    /// durable). With `others`, the scope's other identities go too.
    fn prune_best_effort(&self, others: bool) {
        if let Err(error) = self.prune(others) {
            crate::diagnostics::diag!("recipe: warning: cannot prune saved state: {error:#}");
        }
    }

    /// Delete every generation of this identity but the current one, a
    /// crashed never-committed snapshot, and with `others` every other
    /// identity of the scope of the same kind (build material, or a download
    /// slot): one copy of each at most.
    fn prune(&self, others: bool) -> Result<()> {
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
        for (_, path) in generations {
            if path.file_name().is_some_and(|name| name == current) {
                continue;
            }
            remove_generation(&path)?;
        }
        if !others || self.current.generation == 0 {
            return Ok(());
        }
        let scope = self.directory.parent().context("identity has no scope")?;
        for entry in fs::read_dir(scope)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(identity) = name.to_str() else {
                continue;
            };
            if entry.path() == self.directory
                || !entry.file_type()?.is_dir()
                || identity.len() != 64
                || !identity.bytes().all(|b| b.is_ascii_hexdigit())
            {
                continue;
            }
            let path = entry.path();
            let entries = fs::read_dir(&path)?.collect::<std::io::Result<Vec<_>>>()?;
            // Downloads and build material are kept apart: a download slot
            // (only Downloaded generations) replaces only another download
            // slot, and a build's generation never removes a download slot.
            let kinds: Vec<Reached> = entries
                .iter()
                .filter_map(|entry| owned_record(&entry.path().join(RECORD), identity))
                .map(|record| record.reached)
                .collect();
            let download_slot =
                !kinds.is_empty() && kinds.iter().all(|kind| *kind == Reached::Downloaded);
            if !kinds.is_empty() && download_slot != self.downloads {
                continue;
            }
            // Foreign or manually added data is never recursively removed:
            // only the pointer, committed generations and crashed snapshots.
            let owned = entries.iter().all(|entry| {
                (entry.file_name() == CURRENT && entry.file_type().is_ok_and(|kind| kind.is_file()))
                    || (entry.file_type().is_ok_and(|kind| kind.is_dir())
                        && entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(GENERATION_PREFIX)
                        && generation_files(&entry.path()).is_some_and(|files| {
                            owned_record(&entry.path().join(RECORD), identity).is_some()
                                || (files.len() == 1
                                    && files[0].file_name().is_some_and(|name| name == FILES))
                        }))
            });
            if !owned {
                continue;
            }
            for entry in entries {
                if entry.file_type()?.is_dir() {
                    remove_generation(&entry.path())?;
                }
            }
            match fs::remove_file(path.join(CURRENT)) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error.into());
                }
                _ => {}
            }
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
    /// destination's lock (a run of this recipe holds it while it runs, so
    /// nothing a run in progress uses is deleted). Waits up to `wait` for
    /// the lock. Returns the bytes freed.
    pub fn remove(&self, kinds: &[Reached], wait: Duration) -> Result<u64> {
        let _lock = lock_destination(&self.destination, wait).with_context(|| {
            format!(
                "{} is in use by a run of {}; try again when it ends",
                self.destination.display(),
                self.recipe.display()
            )
        })?;
        self.remove_locked(kinds)
    }

    /// [`Self::remove`] by a caller that holds the destination's lock
    /// already (the run that just finished). Where an identity's current
    /// generation goes, its newest remaining one becomes current; an
    /// identity left with none is removed, and a scope left with none too.
    pub fn remove_locked(&self, kinds: &[Reached]) -> Result<u64> {
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
        // A scope that holds nothing but its own description goes too, so
        // nothing of the program is left (a run makes it again).
        let empty = fs::read_dir(&self.directory)?
            .filter_map(|entry| entry.ok())
            .all(|entry| entry.file_name() == SCOPE_ABOUT);
        if empty {
            match fs::remove_file(self.directory.join(SCOPE_ABOUT)) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error.into());
                }
                _ => {}
            }
            let _ = fs::remove_dir(&self.directory);
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
