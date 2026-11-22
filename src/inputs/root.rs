//! Host-owned, per-invocation filesystem for the configured Ginux launcher.
//! An assembled image tar is selected by the coordinator and verified against
//! the content digest in domain.root. No recipe code runs during preparation.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::PreparedInvocation;

const MAX_WORKER_BYTES: u64 = 512 * 1024 * 1024;

/// Verified image bytes owned by one command's sequential dependency graph,
/// made where an image cache is built from them (`image_cache`). Only this
/// anonymous archive and the verified cache made from it are reused; every
/// domain gets a fresh root.
pub(crate) struct VerifiedImage {
    root_reference: String,
    snapshot: File,
}

impl VerifiedImage {
    fn open(root_reference: &str, kind: crate::runtime::Image) -> Result<Self> {
        let digest = crate::runtime::root_digest(root_reference)?;
        // Snapshot and hash the same bytes into an unlinked file (a
        // copy-on-write clone where the filesystem can). Later nodes read
        // these verified bytes, never a potentially changed source path.
        let (_, mut archive) = crate::runtime::open_rootfs(root_reference, kind).with_context(|| {
            format!(
                "cannot open the Ginux rootfs image; set {} to the assembled tar matching {root_reference}",
                kind.rootfs_env()
            )
        })?;
        let mut snapshot = tempfile::tempfile().context("cannot snapshot the rootfs image")?;
        let (_, actual) = crate::fsx::snapshot_hashed(
            &mut archive,
            &mut snapshot,
            crate::image::MAX_ARCHIVE_BYTES,
        )?;
        if actual != digest {
            bail!("Ginux rootfs digest mismatch: expected {digest}, got {actual}");
        }
        Ok(Self {
            root_reference: root_reference.to_owned(),
            snapshot,
        })
    }
}

/// Unpack the image `root_reference` into the new, private, empty `root`,
/// hashing every byte as it is read instead of first copying the archive to a
/// verified snapshot (`VerifiedImage`). On a filesystem without reflinks that
/// snapshot is a second full copy of the image written to the disk for every
/// root. The root is still only made of verified bytes when it is handed
/// over: an archive that does not hash to the pinned digest (read to its
/// end) fails here, before any domain starts, and `root` is removed with its
/// invocation's directory. Each member is validated as it is unpacked either
/// way (`unpack_image`).
fn unpack_verified(root_reference: &str, kind: crate::runtime::Image, root: &Path) -> Result<()> {
    let digest = crate::runtime::root_digest(root_reference)?;
    let (_, archive) = crate::runtime::open_rootfs(root_reference, kind).with_context(|| {
        format!(
            "cannot open the Ginux rootfs image; set {} to the assembled tar matching {root_reference}",
            kind.rootfs_env()
        )
    })?;
    let mut hashed = Hashed {
        input: std::io::BufReader::with_capacity(1 << 20, archive),
        hash: sha2::Sha256::default(),
        count: 0,
    };
    let unpacked = unpack_image(&mut hashed, root);
    crate::cancellation::check()?;
    // The rest of the archive after its last member (the end blocks and any
    // padding) counts as well. An archive that is not the pinned one is
    // reported as such, whatever its unpacking made of it.
    std::io::copy(&mut hashed, &mut std::io::sink())?;
    let actual = hex::encode(sha2::Digest::finalize(hashed.hash));
    if actual != digest {
        bail!("Ginux rootfs digest mismatch: expected {digest}, got {actual}");
    }
    unpacked
}

/// A reader that hashes what it reads, bounded like a snapshot of the image.
struct Hashed<R> {
    input: R,
    hash: sha2::Sha256,
    count: u64,
}

impl<R: Read> Read for Hashed<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.input.read(buffer)?;
        self.count += read as u64;
        if self.count > crate::image::MAX_ARCHIVE_BYTES {
            return Err(std::io::Error::other(format!(
                "the rootfs image exceeds its bound of {} bytes",
                crate::image::MAX_ARCHIVE_BYTES
            )));
        }
        sha2::Digest::update(&mut self.hash, &buffer[..read]);
        Ok(read)
    }
}

/// The verified bytes of `root_reference` (of kind `kind`, which selects its
/// tar), verified now unless `image` already holds them, positioned at their
/// start.
pub(super) fn verified_snapshot<'a>(
    image: &'a mut Option<VerifiedImage>,
    root_reference: &str,
    kind: crate::runtime::Image,
) -> Result<&'a mut File> {
    if image
        .as_ref()
        .is_none_or(|image| image.root_reference != root_reference)
    {
        *image = Some(VerifiedImage::open(root_reference, kind)?);
    }
    let image = image.as_mut().expect("verified image initialized");
    image.snapshot.seek(SeekFrom::Start(0))?;
    Ok(&mut image.snapshot)
}

/// Owns the isolated root until the launcher and worker have been reaped.
pub(crate) struct PreparedRoot {
    directory: crate::temporary::TemporaryDirectory,
    pub(crate) worker_digest: String,
    /// The read-only binds the launcher makes in its own mount namespace
    /// before it starts ([`super::InputKind::Bind`]): (host folder, empty
    /// folder under this root's `inputs`).
    binds: Vec<(CString, CString)>,
    /// Whether the offline cache is bound at [`crate::domain::OFFLINE_CACHE`].
    offline_cache: bool,
}

impl PreparedRoot {
    pub(crate) fn lease_fd(&self) -> std::os::fd::RawFd {
        self.directory.lease_fd()
    }
    pub(crate) fn path(&self) -> &Path {
        self.directory.path()
    }
    pub(crate) fn binds(&self) -> &[(CString, CString)] {
        &self.binds
    }
    /// The offline cache directory the worker reads, a domain path, when this
    /// root has it.
    pub(crate) fn offline_cache(&self) -> Option<String> {
        self.offline_cache
            .then(|| crate::domain::OFFLINE_CACHE.to_owned())
    }
}

/// Make the private Ginux root of one invocation. `request` is the worker's
/// request record: its offline cache is set here (this root decides whether
/// the cache is bound), then it is staged with the other inputs.
pub(crate) fn prepare_root(
    invocation: &PreparedInvocation,
    image: &mut Option<VerifiedImage>,
    request: &mut crate::worker::records::WorkerRequest,
) -> Result<PreparedRoot> {
    if std::env::consts::OS != "linux" || std::env::consts::ARCH != "x86_64" {
        bail!("Ginux execution currently requires Linux x86_64");
    }
    let mut root = PreparedRoot {
        worker_digest: String::new(),
        directory: crate::temporary::TemporaryDirectory::new()?,
        binds: Vec::new(),
        offline_cache: false,
    };
    // A clone of the verified image's cached tree where the filesystem can
    // clone; otherwise the image unpacked here and verified as it unpacks
    // (see image_cache).
    let kind = invocation.root_image;
    let unpacked =
        !super::image_cache::populate(&invocation.root_reference, kind, image, root.path())?;
    if unpacked {
        // Verified bytes already at hand (the cache's build made them) are
        // unpacked as they are; otherwise the image is hashed as it unpacks.
        if image
            .as_ref()
            .is_some_and(|image| image.root_reference == invocation.root_reference)
        {
            unpack_image(
                verified_snapshot(image, &invocation.root_reference, kind)?,
                root.path(),
            )?;
        } else {
            unpack_verified(&invocation.root_reference, kind, root.path())?;
        }
    }

    super::network::install_hosts(&invocation.network, root.path())?;
    let inputs = root
        .path()
        .join(crate::domain::INPUTS.trim_start_matches('/'));
    // The offline cache's `sha256` folder, bound read-only like a live root's
    // folders: the domain reads the payloads in place, and nothing of the
    // cache is copied or linked into this root. A file is still used only
    // when its bytes match the pin (`crate::helpers::acquire::download`).
    // It is not an input, so it enters no identity: a run's results never
    // depend on it. Decided before staging, because the request names it.
    let mut cache_payloads = None;
    if let Some(cache) = crate::offline_cache::dir()?
        && cache.join("sha256").is_dir()
    {
        if super::can_bind() {
            cache_payloads = Some(cache.join("sha256"));
            root.offline_cache = true;
        } else {
            crate::diagnostics::diag!(
                "recipe: the offline cache {} is not given to this Ginux run: binding it \
                 read-only needs root with CAP_SYS_ADMIN and a / that is a mount (not a chroot)",
                cache.display()
            );
        }
    }
    request.cache = root.offline_cache();
    stage_inputs(invocation, &inputs, request)?;
    use std::os::unix::ffi::OsStrExt;
    let path = |path: &Path| CString::new(path.as_os_str().as_bytes()).map_err(anyhow::Error::from);
    root.binds = invocation
        .binds()
        .map(|(host, name)| Ok((path(host)?, path(&inputs.join(name))?)))
        .collect::<Result<_>>()?;
    if let Some(payloads) = cache_payloads {
        let folder = inputs.join(crate::domain::OFFLINE_CACHE_NAME);
        std::fs::create_dir(&folder)?;
        std::fs::create_dir(folder.join("sha256"))?;
        root.binds
            .push((path(&payloads)?, path(&folder.join("sha256"))?));
    }
    let worker_path = worker_binary()?;
    let mut worker = crate::fsx::open_regular(&worker_path)
        .with_context(|| format!("cannot open worker {}", worker_path.display()))?;
    let mut header = [0u8; 20];
    worker
        .read_exact(&mut header)
        .context("worker is not an x86_64 ELF executable")?;
    if &header[..7] != b"\x7fELF\x02\x01\x01" || header[18..20] != [62, 0] {
        bail!("worker must be a Linux x86_64 ELF executable");
    }
    worker.seek(SeekFrom::Start(0))?;
    let mut destination = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(inputs.join(crate::domain::WORKER_NAME))?;
    root.worker_digest =
        crate::fsx::snapshot_hashed(&mut worker, &mut destination, MAX_WORKER_BYTES)?.1;
    destination.set_permissions(std::fs::Permissions::from_mode(0o555))?;

    // No shared cached tree or mutable hardlinks are handed to the domain (an
    // input's only other name is this invocation's private staged copy).
    // Write syscalls still require Ginux storage mediation; these directories
    // do not grant native host filesystem access.
    for path in crate::domain::WRITABLE_DIRS {
        let directory = root.path().join(path.trim_start_matches('/'));
        std::fs::create_dir(&directory)?;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o777))?;
    }
    seal_inputs(&inputs)?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o555))?;
    // An unpacked root is the whole runtime image written anew (about 1.2 GB
    // on a filesystem without reflinks, ext4 among them). Left dirty, the
    // kernel writes it back while the run is under way, and a program's page
    // faults and reads then wait behind that writeback, uninterruptibly, for
    // seconds. Ginux stops every running task before it lets one task's
    // path or file-mapping call proceed and gives up after 5 s: such a wait
    // aborted runs ("launch supervision failure: argument peer interrupt
    // timed out", root effects after an update). So the run starts only once
    // the root is on the disk. A cloned root writes no data and is not
    // flushed.
    if unpacked {
        crate::fsx::sync_filesystem(root.path())?;
    }
    Ok(root)
}

/// Put the staged inputs into `inputs` (created here): each file by hard link
/// to its private staged copy where both share a filesystem, else as a copy;
/// either way every byte is read back and checked against its discovery
/// digest. Then write the input permission manifest and the worker's request
/// file ([`crate::domain::REQUEST_NAME`]).
/// [`seal_inputs`] then makes them read-only. Shared by the Ginux root and
/// the host run.
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
            // The empty folder the launcher binds the host folder on
            // (`crate::launch`); a host run is never given one.
            super::InputKind::Bind => {
                std::fs::create_dir(inputs.join(&input.name))?;
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
        // Explicitly imported bytes must be readable after Ginux drops to its
        // domain uid, even when the coordinator read a host-private 0600 file.
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
                "rootfs image entry traverses a non-directory: {}",
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

pub(super) fn unpack_image(snapshot: impl Read, root: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(snapshot);
    let mut validator = crate::image::Validator::default();
    let mut directories = HashMap::new();
    for entry in archive.entries()? {
        crate::cancellation::check()?;
        let mut entry = entry?;
        let relative = crate::image::member_path(&entry.path()?)?;
        let kind = entry.header().entry_type();
        if relative.as_os_str().is_empty() && kind.is_dir() {
            continue;
        }
        if relative.as_os_str().is_empty() {
            bail!("empty rootfs image entry");
        }
        let link = entry.link_name()?.map(|link| link.into_owned());
        validator.check(
            &relative,
            kind,
            entry.size(),
            entry.header().mode()?,
            link.as_deref(),
        )?;
        let mode = crate::image::installed_mode(entry.header().mode()?);
        ensure_parents(root, &relative)?;
        let destination = root.join(&relative);
        if kind.is_dir() {
            match std::fs::symlink_metadata(&destination) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => bail!(
                    "rootfs directory conflicts with a file: {}",
                    relative.display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(&destination)?
                }
                Err(error) => return Err(error.into()),
            }
            directories.insert(destination, mode);
        } else if kind.is_file() {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            let count = std::io::copy(&mut entry, &mut file)?;
            if count != entry.size() {
                bail!("truncated rootfs image entry: {}", relative.display());
            }
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
            // Written back while the rest unpacks, so the flush before the
            // run (`prepare_root`) finds little left to wait for.
            crate::fsx::start_writeback(&file);
        } else if kind.is_symlink() {
            // Absolute symlinks are meaningful inside chroot (e.g. /proc/mounts).
            // They are never traversed while constructing or cleaning the root.
            std::os::unix::fs::symlink(link.expect("validated symlink target"), destination)?;
        } else {
            // A hardlink aliases an earlier verified regular file in this
            // private snapshot; host sources and shared caches stay separate.
            let target = crate::image::member_path(&link.expect("validated hardlink target"))?;
            ensure_parents(root, &target)?;
            std::fs::hard_link(root.join(target), destination)?;
        }
    }
    // Include implicitly-created parent directories too.
    let mut image_directories = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
    image_directories.sort_by_key(|entry| std::cmp::Reverse(entry.depth()));
    for entry in image_directories {
        if entry.depth() > 0 && entry.file_type().is_dir() {
            let mode = directories.get(entry.path()).copied().unwrap_or(0o555);
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(())
}

/// Seed the owned target from explicitly selected, already verified input bytes.
/// Read only the invocation snapshot, never the original host input pathname.
///
/// The `target` input (the caller's tree) seeds the recipe's one output, a
/// directory, before `build` runs (docs/recipe-format.md, section 3.6); the
/// dependencies' merged tools seed `/work/tools`.
pub(crate) fn seed_target(
    invocation: &PreparedInvocation,
    root: &Path,
    destination: &Path,
) -> Result<bool> {
    use super::InputKind;
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
    let mapping = |name: &str| -> Option<String> {
        let seeds = target_output
            .map(|output| ("target", output))
            .into_iter()
            .chain([(crate::domain::DEPENDENCY_TOOLS, "work/tools")]);
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
    let selected: Vec<_> = invocation
        .inputs
        .iter()
        .filter(|input| mapping(&input.name).is_some())
        .collect();
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
    if selected.is_empty() {
        return Ok(false);
    }
    for name in ["target", crate::domain::DEPENDENCY_TOOLS] {
        if selected.iter().any(|input| input.name == name)
            && !selected
                .iter()
                .any(|input| input.name == name && input.kind == InputKind::Directory)
        {
            bail!("{name} input must be a directory tree");
        }
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut archive = tar::Builder::new(file);
    if selected.iter().any(|input| input.name == "target") {
        // The output's parent, as the restore expects every parent before
        // its entries.
        let mut header = tar::Header::new_ustar();
        header.set_mode(0o755);
        header.set_uid(crate::domain::UID);
        header.set_gid(crate::domain::UID);
        header.set_mtime(0);
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_cksum();
        archive.append_data(
            &mut header,
            crate::domain::OUT.trim_start_matches('/'),
            std::io::empty(),
        )?;
    }
    for input in selected {
        let name = mapping(&input.name).expect("selected seed input");
        let mut header = tar::Header::new_ustar();
        header.set_mode(input.mode);
        header.set_uid(crate::domain::UID);
        header.set_gid(crate::domain::UID);
        header.set_mtime(0);
        match &input.kind {
            InputKind::Directory => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_cksum();
                archive.append_data(&mut header, name, std::io::empty())?;
            }
            InputKind::Symlink(target) => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                header.set_cksum();
                archive.append_link(&mut header, name, target)?;
            }
            InputKind::Hardlink(target) if mapping(target).is_some() => {
                header.set_entry_type(tar::EntryType::Link);
                header.set_size(0);
                let link = mapping(target).unwrap();
                archive.append_link(&mut header, name, link)?;
            }
            InputKind::Bind => bail!("a bound input cannot seed a target: {}", input.name),
            InputKind::File | InputKind::Hardlink(_) => {
                let mut file = crate::fsx::open_regular(
                    &root
                        .join(crate::domain::INPUTS.trim_start_matches('/'))
                        .join(&input.name),
                )?;
                if file.metadata()?.len() != input.size {
                    bail!("target snapshot size changed: {}", input.name);
                }
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(input.size);
                header.set_cksum();
                archive.append_data(&mut header, name, &mut file)?;
            }
        }
    }
    archive.finish()?;
    // Scratch for this launch only, read back from the page cache and
    // removed with the run directory: nothing to make durable.
    archive.into_inner()?;
    Ok(true)
}
