//! What a run starts from, as files: the archive a Ginux instance receives
//! ([`inject_archive`]) and the directories of a host run ([`stage_inputs`]).
//! Every staged input is read from the invocation's private staging copy and
//! checked against its discovery digest. No recipe code runs here.

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{FileExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::{InputKind, PreparedInvocation, StagedInput};

/// Largest worker executable Recipe hands to a run.
const MAX_WORKER_BYTES: u64 = 512 * 1024 * 1024;

/// What [`inject_archive`] wrote: the worker's SHA-256 as it was appended
/// (the caller compares it with the digest the run's identity used), the
/// archive's size in bytes, and its member count, GNU long-name records
/// included (the bounds `ginux inject` is given).
#[derive(Debug, Clone)]
pub(crate) struct Injected {
    pub worker_digest: String,
    pub bytes: u64,
    pub members: u64,
}

/// Write the one archive a fresh Ginux instance receives before it starts
/// (`ginux inject NAME --input ARCHIVE --into /`): an uncompressed GNU tar
/// of directories, regular files and symlinks only, names relative to the
/// instance root without a leading `./`, every name once, parents first.
///
/// - `inputs/`: every staged input (directories 0555, files with their mode
///   plus read for everyone, a hard link as a copy of its target, symlinks
///   last), the input-modes manifest, the request file, the worker (0555);
/// - `inputs/.recipe-offline-cache/sha256/<hex>`: the offline cache's files
///   for the recipe's declared pins (`SOURCES` downloads and `PACKAGES`), when
///   there is a cache; `request.cache` is set here accordingly;
/// - `work/`, `work/build/`, `work/tools/`, `work/tmp/`, `out/` (0755);
/// - `etc/hosts` (0444): localhost and the pinned network names;
/// - the kept files the run starts from: each member of `restore` (a tar of
///   `work/…` and `out/…` members) re-emitted under its own name, hard links
///   as copies; without `restore`, the target view the inputs seed
///   ([`seed_target`]).
///
/// `destination` must not exist. The request is encoded after the cache is
/// decided, so it is complete when this returns.
pub(crate) fn inject_archive(
    invocation: &PreparedInvocation,
    request: &mut crate::worker::records::WorkerRequest,
    restore: Option<&Path>,
    destination: &Path,
) -> Result<Injected> {
    if std::env::consts::OS != "linux" || std::env::consts::ARCH != "x86_64" {
        bail!("Ginux execution currently requires Linux x86_64");
    }
    let cached = offline_payloads(invocation)?;
    request.cache = (!cached.is_empty()).then(|| crate::domain::OFFLINE_CACHE.to_owned());

    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| format!("cannot create {}", destination.display()))?;
    let mut archive = Archive::new(std::io::BufWriter::with_capacity(1 << 20, file));
    let inputs = crate::domain::INPUTS.trim_start_matches('/');
    archive.dir(inputs, 0o555)?;
    let input_name = |name: &str| format!("{inputs}/{name}");

    for (index, input) in invocation.inputs.iter().enumerate() {
        crate::cancellation::check()?;
        crate::domain::validate_input_name(&input.name)?;
        let name = input_name(&input.name);
        archive.parents(&name, 0o555)?;
        match &input.kind {
            InputKind::Directory => archive.dir(&name, 0o555)?,
            InputKind::Symlink(_) => {} // After every ordinary path.
            InputKind::File | InputKind::Hardlink(_) => {
                let (file, staged) = staged_copy(invocation, index)?;
                archive.checked(&name, input.mode | 0o444, file, staged)?;
            }
        }
    }
    for input in &invocation.inputs {
        if let InputKind::Symlink(target) = &input.kind {
            archive.symlink(&input_name(&input.name), target)?;
        }
    }
    // Logical input permissions, apart from the read grants the copies need.
    let modes = invocation
        .inputs
        .iter()
        .map(|input| (input.name.clone(), input.mode))
        .collect::<crate::domain::InputModes>();
    let modes = serde_json::to_vec(&modes)?;
    if modes.len() as u64 > crate::domain::MAX_INPUT_MODES_BYTES {
        bail!("input permission manifest exceeds its bound");
    }
    archive.bytes(&input_name(crate::domain::INPUT_MODES_NAME), 0o444, &modes)?;

    if !cached.is_empty() {
        let folder = input_name(crate::domain::OFFLINE_CACHE_NAME);
        archive.dir(&folder, 0o555)?;
        archive.dir(&format!("{folder}/sha256"), 0o555)?;
        for (hex, file) in cached {
            crate::cancellation::check()?;
            let size = file.metadata()?.len();
            // The worker uses a cached file only when its bytes match the pin.
            archive.file(&format!("{folder}/sha256/{hex}"), 0o444, size, file)?;
        }
    }

    let encoded = request.encode()?;
    archive.bytes(&input_name(crate::domain::REQUEST_NAME), 0o444, &encoded)?;

    let worker_path = worker_binary()?;
    let mut worker = crate::fsx::open_regular(&worker_path)
        .with_context(|| format!("cannot open worker {}", worker_path.display()))?;
    let mut header = [0u8; 20];
    worker
        .read_exact_at(&mut header, 0)
        .context("worker is not an x86_64 ELF executable")?;
    if &header[..7] != b"\x7fELF\x02\x01\x01" || header[18..20] != [62, 0] {
        bail!("worker must be a Linux x86_64 ELF executable");
    }
    let size = worker.metadata()?.len();
    if size > MAX_WORKER_BYTES {
        bail!("the worker exceeds {MAX_WORKER_BYTES} bytes");
    }
    let worker_digest = archive.file(
        &input_name(crate::domain::WORKER_NAME),
        0o555,
        size,
        &mut worker,
    )?;

    // Owned by the workload once injected, so 0755 suffices.
    for directory in crate::domain::WRITABLE_DIRS {
        archive.dir(directory.trim_start_matches('/'), 0o755)?;
    }
    // `/etc` is the image's; the image carries no `etc/hosts` (Recipe's
    // image rule), so this never meets an existing file.
    archive.bytes(
        "etc/hosts",
        0o444,
        invocation.network.hosts_file().as_bytes(),
    )?;

    match restore {
        Some(restore) => reemit_restore(&mut archive, restore)
            .with_context(|| format!("cannot add the kept files of {}", restore.display()))?,
        None => {
            let plan = seed_plan(invocation)?;
            write_seed(invocation, &plan, &mut archive)?;
        }
    }

    let members = archive.members;
    let mut writer = archive.builder.into_inner()?;
    writer.flush()?;
    let file = writer.into_inner().map_err(|error| error.into_error())?;
    let bytes = file.metadata()?.len();
    Ok(Injected {
        worker_digest,
        bytes,
        members,
    })
}

/// The offline cache's files for the pins this recipe declares, by SHA-256:
/// its `SOURCES` downloads and every package of its pins file. Downloads a
/// recipe computes while it runs are not among them.
fn offline_payloads(invocation: &PreparedInvocation) -> Result<Vec<(String, File)>> {
    let Some(cache) = crate::offline_cache::dir()? else {
        return Ok(Vec::new());
    };
    let declared = &invocation.declaration.declared;
    let mut pins = std::collections::BTreeSet::new();
    for source in declared.sources.values() {
        if let crate::declarations::Origin::Url { sha256, .. } = &source.origin {
            pins.insert(sha256.to_ascii_lowercase());
        }
    }
    if let Some(file) = &declared.pins {
        for section in &file.sections {
            for package in &section.packages {
                pins.insert(package.sha256.to_ascii_lowercase());
            }
        }
    }
    Ok(pins
        .into_iter()
        .filter_map(|hex| {
            let file = crate::offline_cache::open(&cache, &hex)?;
            Some((hex, file))
        })
        .collect())
}

/// The staged copy holding input `index`'s bytes, and the input those bytes
/// were discovered as: the input itself, or the target of a hard link.
fn staged_copy(invocation: &PreparedInvocation, index: usize) -> Result<(File, &StagedInput)> {
    let input = &invocation.inputs[index];
    let (index, input) = match &input.kind {
        InputKind::File => (index, input),
        InputKind::Hardlink(target) => invocation
            .inputs
            .iter()
            .enumerate()
            .find(|(_, candidate)| candidate.name == *target && candidate.kind == InputKind::File)
            .with_context(|| format!("input hardlink target is not regular: {}", input.name))?,
        _ => bail!("input {} has no file contents", input.name),
    };
    let path = invocation.staging_dir().join(format!("input-{index}"));
    let file = crate::fsx::open_regular(&path)
        .with_context(|| format!("cannot open the staged copy of {}", input.name))?;
    Ok((file, input))
}

/// A tar being written for `ginux inject` or a host restore: GNU headers,
/// owner [`crate::domain::UID`], time 0; each name once, a directory named
/// twice is written once.
struct Archive<W: Write> {
    builder: tar::Builder<W>,
    names: HashSet<String>,
    directories: HashSet<String>,
    members: u64,
}

impl<W: Write> Archive<W> {
    fn new(writer: W) -> Self {
        Self {
            builder: tar::Builder::new(writer),
            names: HashSet::new(),
            directories: HashSet::new(),
            members: 0,
        }
    }

    fn header(kind: tar::EntryType, mode: u32, size: u64) -> tar::Header {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_mode(mode & 0o7777);
        header.set_uid(crate::domain::UID);
        header.set_gid(crate::domain::UID);
        header.set_mtime(0);
        header.set_size(size);
        header
    }

    /// Take `name` for one member: relative, no `.`/`..`/empty component,
    /// no NUL, not taken before. A name too long for the header adds a GNU
    /// long-name record.
    fn claim(&mut self, name: &str) -> Result<()> {
        if name.is_empty()
            || name.contains('\0')
            || name
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            bail!("invalid archive member name {name:?}");
        }
        if !self.names.insert(name.to_owned()) {
            bail!("archive member named twice: {name}");
        }
        self.members += 1 + u64::from(name.len() >= 100);
        Ok(())
    }

    /// Each missing parent directory of `name`, outermost first.
    fn parents(&mut self, name: &str, mode: u32) -> Result<()> {
        let mut end = 0;
        while let Some(offset) = name[end..].find('/') {
            end += offset;
            self.dir(&name[..end], mode)?;
            end += 1;
        }
        Ok(())
    }

    fn dir(&mut self, name: &str, mode: u32) -> Result<()> {
        if self.directories.contains(name) {
            return Ok(());
        }
        self.claim(name)?;
        self.directories.insert(name.to_owned());
        let mut header = Self::header(tar::EntryType::Directory, mode, 0);
        self.builder
            .append_data(&mut header, name, std::io::empty())
            .with_context(|| format!("cannot write archive member {name}"))
    }

    /// A regular file of exactly `size` bytes from `data`; returns their
    /// SHA-256.
    fn file(&mut self, name: &str, mode: u32, size: u64, data: impl Read) -> Result<String> {
        self.claim(name)?;
        let mut header = Self::header(tar::EntryType::Regular, mode, size);
        let mut hashed = Hashed {
            inner: data.take(size),
            hash: sha2::Sha256::default(),
            count: 0,
        };
        self.builder
            .append_data(&mut header, name, &mut hashed)
            .with_context(|| format!("cannot write archive member {name}"))?;
        if hashed.count != size {
            bail!(
                "{name} changed while it was archived: {} of {size} bytes",
                hashed.count
            );
        }
        Ok(hex::encode(sha2::Digest::finalize(hashed.hash)))
    }

    fn bytes(&mut self, name: &str, mode: u32, data: &[u8]) -> Result<()> {
        self.file(name, mode, data.len() as u64, data).map(drop)
    }

    /// A staged input's bytes, checked against its discovery size and digest.
    fn checked(&mut self, name: &str, mode: u32, file: File, input: &StagedInput) -> Result<()> {
        let digest = self.file(name, mode, input.size, file)?;
        if digest != hex::encode(input.sha256) {
            bail!("staged input changed after discovery: /{name}");
        }
        Ok(())
    }

    fn symlink(&mut self, name: &str, target: impl AsRef<Path>) -> Result<()> {
        let target = target.as_ref();
        self.claim(name)?;
        self.members += u64::from(target.as_os_str().len() >= 100);
        let mut header = Self::header(tar::EntryType::Symlink, 0o777, 0);
        self.builder
            .append_link(&mut header, name, target)
            .with_context(|| format!("cannot write archive member {name}"))
    }
}

/// A reader that hashes and counts what it reads.
struct Hashed<R> {
    inner: R,
    hash: sha2::Sha256,
    count: u64,
}

impl<R: Read> Read for Hashed<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.count += read as u64;
        sha2::Digest::update(&mut self.hash, &buffer[..read]);
        Ok(read)
    }
}

/// `left` bytes of `file` from `position`, without moving any file offset.
struct At<'a> {
    file: &'a File,
    position: u64,
    left: u64,
}

impl Read for At<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let want = buffer
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        if want == 0 {
            return Ok(0);
        }
        let read = self.file.read_at(&mut buffer[..want], self.position)?;
        self.position += read as u64;
        self.left -= read as u64;
        Ok(read)
    }
}

/// A restore member's name relative to the instance root: `./` and a
/// trailing `/` dropped; it must lie in one of the writable trees.
fn restore_name(path: &Path) -> Result<String> {
    let text = path
        .to_str()
        .with_context(|| format!("kept file name is not UTF-8: {}", path.display()))?;
    let name = text
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_owned();
    if !name
        .split('/')
        .next()
        .is_some_and(|root| crate::domain::WRITABLE_ROOTS.contains(&root))
    {
        bail!("kept file outside the writable trees: {text:?}");
    }
    Ok(name)
}

/// Every member of the kept-files tar `path` into `archive` under its own
/// name: directories (those the skeleton already has are not repeated),
/// regular files, symlinks, and hard links as copies of their target's bytes
/// (`ginux inject` accepts no hard link).
fn reemit_restore<W: Write>(archive: &mut Archive<W>, path: &Path) -> Result<()> {
    let file = File::open(path)?;
    let source = file.try_clone()?;
    let mut kept = tar::Archive::new(std::io::BufReader::with_capacity(1 << 20, file));
    // Regular members by name: (data offset in the tar, size, mode).
    let mut regular = HashMap::<String, (u64, u64, u32)>::new();
    for entry in kept.entries()? {
        crate::cancellation::check()?;
        let mut entry = entry?;
        let name = restore_name(&entry.path()?)?;
        let mode = entry.header().mode()?;
        match entry.header().entry_type() {
            tar::EntryType::Directory => archive.dir(&name, mode)?,
            tar::EntryType::Regular | tar::EntryType::Continuous => {
                let position = entry.raw_file_position();
                let size = entry.size();
                archive.file(&name, mode, size, &mut entry)?;
                regular.insert(name, (position, size, mode));
            }
            tar::EntryType::Symlink => {
                let target = entry
                    .link_name()?
                    .with_context(|| format!("kept symlink {name} has no target"))?
                    .into_owned();
                archive.symlink(&name, &target)?;
            }
            tar::EntryType::Link => {
                let target = entry
                    .link_name()?
                    .with_context(|| format!("kept hard link {name} has no target"))?;
                let target = restore_name(&target)?;
                let &(position, size, mode) = regular
                    .get(&target)
                    .with_context(|| format!("kept hard link {name} names no earlier file"))?;
                archive.file(
                    &name,
                    mode,
                    size,
                    At {
                        file: &source,
                        position,
                        left: size,
                    },
                )?;
            }
            other => bail!("kept file {name} is of an unsupported kind ({other:?})"),
        }
    }
    Ok(())
}

/// Put the staged inputs of a host run into `inputs` (created here): each
/// file by hard link to its private staged copy where both share a
/// filesystem, else as a copy; either way every byte is read back and checked
/// against its discovery digest. Then write the input permission manifest and
/// the worker's request file ([`crate::domain::REQUEST_NAME`]).
/// [`seal_inputs`] then makes them read-only.
pub(crate) fn stage_inputs(
    invocation: &PreparedInvocation,
    inputs: &Path,
    request: &crate::worker::records::WorkerRequest,
) -> Result<()> {
    let inputs = inputs.to_path_buf();
    std::fs::create_dir(&inputs)?;
    for (index, input) in invocation.inputs.iter().enumerate() {
        crate::domain::validate_input_name(&input.name)?;
        ensure_parents(&inputs, Path::new(&input.name))?;
        match input.kind {
            super::InputKind::Directory => {
                let path = inputs.join(&input.name);
                if !path.exists() {
                    std::fs::create_dir(&path)?;
                }
                if !std::fs::symlink_metadata(&path)?.is_dir() {
                    bail!("input directory collision: {}", input.name);
                }
                continue;
            }
            super::InputKind::Hardlink(_) => continue,
            super::InputKind::Symlink(_) => continue, // Install links after all ordinary paths.
            super::InputKind::File => {}
        }
        let staged = invocation.staging_dir().join(format!("input-{index}"));
        let path = inputs.join(&input.name);
        // The staged file is this invocation's own, in the coordinator's
        // private pool, and nothing reads or writes it after this: take it by
        // hard link where it shares a filesystem with `inputs`, so a large
        // input (a whole system root) does not take its size twice on a disk
        // without reflinks. The link is read back and hashed like a copy.
        let (destination, (size, digest)) = match std::fs::hard_link(&staged, &path) {
            Ok(()) => {
                let mut linked = crate::fsx::open_regular(&path)?;
                let hashed =
                    crate::fsx::copy_hashed(&mut linked, &mut std::io::sink(), input.size)?;
                (linked, hashed)
            }
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::EXDEV | libc::EMLINK | libc::EPERM | libc::EOPNOTSUPP)
                ) =>
            {
                let mut source = crate::fsx::open_regular(&staged)?;
                let mut destination = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                let hashed =
                    crate::fsx::snapshot_hashed(&mut source, &mut destination, input.size)?;
                (destination, hashed)
            }
            Err(error) => {
                return Err(error).with_context(|| format!("cannot stage /inputs/{}", input.name));
            }
        };
        if size != input.size || digest != hex::encode(input.sha256) {
            bail!(
                "staged input changed after discovery: /inputs/{}",
                input.name
            );
        }
        // Explicitly imported bytes must be readable by the worker's
        // identity, even when the coordinator read a host-private 0600 file.
        destination.set_permissions(std::fs::Permissions::from_mode(input.mode | 0o444))?;
    }
    for input in &invocation.inputs {
        if let super::InputKind::Hardlink(target) = &input.kind {
            crate::domain::validate_input_name(target)?;
            ensure_parents(&inputs, Path::new(&input.name))?;
            let target = inputs.join(target);
            if !std::fs::symlink_metadata(&target)?.is_file() {
                bail!("input hardlink target is not regular");
            }
            std::fs::hard_link(target, inputs.join(&input.name))?;
        }
    }
    for input in &invocation.inputs {
        if let super::InputKind::Symlink(target) = &input.kind {
            ensure_parents(&inputs, Path::new(&input.name))?;
            std::os::unix::fs::symlink(target, inputs.join(&input.name))?;
        }
    }
    // Preserve logical input permissions independently from the readability
    // required to consume immutable inputs. Tree copying uses this coordinator-
    // authored metadata instead of publishing the snapshot's read grants.
    let modes = invocation
        .inputs
        .iter()
        .map(|input| (input.name.clone(), input.mode))
        .collect::<crate::domain::InputModes>();
    let manifest = inputs.join(crate::domain::INPUT_MODES_NAME);
    let modes = serde_json::to_vec(&modes)?;
    if modes.len() as u64 > crate::domain::MAX_INPUT_MODES_BYTES {
        bail!("input permission manifest exceeds its bound");
    }
    std::fs::write(&manifest, modes)?;
    std::fs::set_permissions(manifest, std::fs::Permissions::from_mode(0o444))?;
    // The request: checked and bounded by its encoder
    // (`crate::domain::MAX_REQUEST_BYTES`), written once, read-only.
    let request = request.encode()?;
    let path = inputs.join(crate::domain::REQUEST_NAME);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("cannot write the request {}", path.display()))?;
    file.write_all(&request)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o444))?;
    Ok(())
}

/// Make every directory of the staged inputs read-only.
pub(crate) fn seal_inputs(inputs: &Path) -> Result<()> {
    for entry in walkdir::WalkDir::new(inputs).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_dir() {
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o555))?;
        }
    }
    Ok(())
}

pub(crate) fn worker_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("RECIPE_WORKER_BIN") {
        if path.is_empty() {
            bail!("RECIPE_WORKER_BIN is empty");
        }
        return PathBuf::from(path)
            .canonicalize()
            .context("cannot resolve RECIPE_WORKER_BIN");
    }
    let current = crate::runtime::own_program();
    if current.file_name().is_some_and(|name| name == "recipe") {
        return Ok(current);
    }
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = directory.join("recipe");
        if candidate.is_file() && candidate.metadata()?.permissions().mode() & 0o111 != 0 {
            return candidate
                .canonicalize()
                .context("cannot resolve installed Recipe worker");
        }
    }
    bail!("cannot locate the Recipe worker; install recipe on PATH or set RECIPE_WORKER_BIN")
}

fn ensure_parents(root: &Path, relative: &Path) -> Result<()> {
    let mut directory = root.to_path_buf();
    for component in relative.parent().unwrap_or(Path::new("")).components() {
        directory.push(component);
        match std::fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => bail!(
                "input entry traverses a non-directory: {}",
                relative.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&directory)?
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// The inputs that seed a run's target view (docs/recipe-format.md, section
/// 3.6): the `target` input (the caller's tree) as the recipe's one
/// directory output, and the dependencies' merged tools as `/work/tools`.
/// Each entry is (input index, member name relative to the domain root); the
/// flag says whether `target` is among them.
fn seed_plan(invocation: &PreparedInvocation) -> Result<(Vec<(usize, String)>, bool)> {
    let outputs = &invocation.declaration.needs.outputs;
    let target_output = match outputs.iter().collect::<Vec<_>>().as_slice() {
        [(name, crate::needs::PathKind::Directory)] => Some(
            crate::domain::output_path(name)
                .trim_start_matches('/')
                .to_owned(),
        ),
        _ => None,
    };
    let target_output = target_output.as_deref();
    let tools = crate::domain::TOOLS.trim_start_matches('/');
    let mapping = |name: &str| -> Option<String> {
        let seeds = target_output
            .map(|output| ("target", output))
            .into_iter()
            .chain([(crate::domain::DEPENDENCY_TOOLS, tools)]);
        for (source, destination) in seeds {
            if name == source || name.starts_with(&format!("{source}/")) {
                return Some(format!(
                    "{destination}{}",
                    name.strip_prefix(source).unwrap()
                ));
            }
        }
        None
    };
    if target_output.is_none() && invocation.inputs.iter().any(|input| input.name == "target") {
        bail!(
            "a target input seeds the recipe's one directory output; this recipe declares {} output(s): {}",
            outputs.len(),
            outputs
                .iter()
                .map(|(name, kind)| format!("{name} ({})", kind.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let selected: Vec<(usize, String)> = invocation
        .inputs
        .iter()
        .enumerate()
        .filter_map(|(index, input)| mapping(&input.name).map(|name| (index, name)))
        .collect();
    for name in ["target", crate::domain::DEPENDENCY_TOOLS] {
        let named = |kind: Option<&InputKind>| {
            selected.iter().any(|(index, _)| {
                let input = &invocation.inputs[*index];
                input.name == name && kind.is_none_or(|kind| input.kind == *kind)
            })
        };
        if named(None) && !named(Some(&InputKind::Directory)) {
            bail!("{name} input must be a directory tree");
        }
    }
    let target = selected
        .iter()
        .any(|(index, _)| invocation.inputs[*index].name == "target");
    Ok((selected, target))
}

/// Write the seed members of `plan` ([`seed_plan`]) from the staged copies:
/// the output tree first when `target` is seeded (every parent precedes its
/// entries), hard links as copies, symlinks last.
fn write_seed<W: Write>(
    invocation: &PreparedInvocation,
    (plan, target): &(Vec<(usize, String)>, bool),
    archive: &mut Archive<W>,
) -> Result<()> {
    if *target {
        archive.dir(crate::domain::OUT.trim_start_matches('/'), 0o755)?;
    }
    for (index, name) in plan {
        crate::cancellation::check()?;
        let input = &invocation.inputs[*index];
        match &input.kind {
            InputKind::Directory => archive.dir(name, input.mode)?,
            InputKind::Symlink(_) => {}
            InputKind::File | InputKind::Hardlink(_) => {
                let (file, staged) = staged_copy(invocation, *index)?;
                archive.checked(name, input.mode, file, staged)?;
            }
        }
    }
    for (index, name) in plan {
        if let InputKind::Symlink(target) = &invocation.inputs[*index].kind {
            archive.symlink(name, target)?;
        }
    }
    Ok(())
}

/// Seed a host run's target view into the new tar `destination`, from the
/// staged copies of the selected inputs ([`seed_plan`]); false, and no file,
/// when no input seeds anything. A Ginux run gets the same members inside
/// its injected archive ([`inject_archive`]).
pub(crate) fn seed_target(invocation: &PreparedInvocation, destination: &Path) -> Result<bool> {
    let plan = seed_plan(invocation)?;
    if plan.0.is_empty() {
        return Ok(false);
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut archive = Archive::new(std::io::BufWriter::new(file));
    write_seed(invocation, &plan, &mut archive)?;
    // Scratch for this run only, read back from the page cache and removed
    // with the run directory: nothing to make durable.
    archive.builder.into_inner()?.flush()?;
    Ok(true)
}
