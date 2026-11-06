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
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
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
                    eprintln!("recipe: waiting for shared destination");
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
        private_directory(&scope)?;
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
        private_directory(&directory)?;
        let allowed = prepared
            .inputs
            .iter()
            .map(|input| format!("/inputs/{}", input.name))
            .collect::<HashSet<_>>();
        let record_path = directory.join("current.json");
        let current = match regular(&record_path) {
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
        Ok(Self {
            directory,
            current,
            allowed,
            state_budget,
            archive_bound: options.execution.archive_bound(),
        })
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
        let mut source = regular(&self.directory.join(&self.current.archive))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        let (bytes, digest) = copy_hash(&mut source, &mut output, self.archive_bound)?;
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
        let mut archive = regular(&pending.path)?;
        let (bytes, sha256) = copy_hash(&mut archive, &mut std::io::sink(), self.archive_bound)?;
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
        Ok(self.current.generation)
    }

    pub fn finish(&mut self, export: &Path, state: Value) -> Result<u64> {
        let pending = self.begin()?;
        let mut input = regular(export)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&pending.path)?;
        copy_hash(&mut input, &mut output, self.archive_bound)?;
        output.sync_all()?;
        drop(output);
        self.commit(pending, "/inputs/main.rhai", state)
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
fn regular(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        bail!("state input is not a regular file");
    }
    Ok(file)
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
    private_directory(&path)
}
fn private_directory(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {
            File::open(path.parent().context("missing state parent")?)?.sync_all()?;
            Ok(())
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::AlreadyExists
                && fs::symlink_metadata(path)?.file_type().is_dir() =>
        {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}
fn copy_hash(
    input: &mut impl Read,
    output: &mut impl Write,
    maximum: u64,
) -> Result<(u64, String)> {
    let mut bytes = [0u8; 65536];
    let mut count = 0u64;
    let mut hash = Sha256::new();
    loop {
        let read = input.read(&mut bytes)?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .filter(|n| *n <= maximum)
            .context("snapshot byte bound exceeded")?;
        hash.update(&bytes[..read]);
        output.write_all(&bytes[..read])?;
    }
    Ok((count, hex::encode(hash.finalize())))
}
