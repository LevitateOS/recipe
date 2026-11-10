//! Activate published package trees without changing the recipe execution engine.
//!
//! A profile generation contains its receipt and all active package files. One
//! symlink replacement commits installation, update or removal. Old generations
//! remain inactive; removal needs neither the source recipe nor a Ginux instance.

use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

const GENERATIONS: &str = "package-generations";
const PACKAGES: &str = ".packages";
const RECEIPT: &str = ".installed.json";
const MAX_RECEIPT: u64 = 8 * 1024 * 1024;

/// An active package, recorded alongside the profile that exposes its files.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub recipe: PathBuf,
    pub files: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    packages: BTreeMap<String, InstalledPackage>,
}

struct Active {
    target: Option<PathBuf>,
    packages: BTreeMap<String, InstalledPackage>,
}

/// Coordinator-owned software storage. The public command directory is
/// `profile_path()/bin`; root and ordinary users choose different roots.
pub struct PackageStore {
    root: PathBuf,
}

impl PackageStore {
    /// Select storage without creating files (listing an unused store is empty).
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn profile_path(&self) -> PathBuf {
        self.root.join("profile")
    }

    /// Read the active receipt. No recipe evaluation or runtime is needed.
    /// Reading changes nothing, so an ordinary user may also read a store
    /// root owns (the system's software, shown by a desktop front end).
    pub fn list(&self) -> Result<Vec<InstalledPackage>> {
        if !self.root.try_exists()? {
            return Ok(Vec::new());
        }
        readable_directory(&self.root)?;
        let active = read_active_checked(&self.root, readable_directory)?;
        Ok(active.packages.into_values().collect())
    }

    /// Check the active filesystem view as well as the receipt. Keep this
    /// separate from removal so a damaged package can still be deactivated.
    pub fn verify(&self, name: &str) -> Result<InstalledPackage> {
        valid_name(name)?;
        real_owned_directory(&self.root)?;
        let active = read_active(&self.root)?;
        let package = active
            .packages
            .get(name)
            .with_context(|| format!("package {name} is not installed"))?;
        let generation = self.root.join(
            active
                .target
                .as_ref()
                .context("missing active generation")?,
        );
        let payload = generation.join(PACKAGES).join(name);
        for file in &package.files {
            let relative = Path::new(file);
            for base in [&generation, &payload] {
                let mut parent = base.to_path_buf();
                for part in relative
                    .parent()
                    .context("package file has no parent")?
                    .components()
                {
                    parent.push(part);
                    real_owned_directory(&parent)?;
                }
            }
            let metadata = fs::symlink_metadata(payload.join(relative))
                .with_context(|| format!("installed package file is missing: {file}"))?;
            ensure!(
                metadata.is_file() && metadata.mode() & 0o6000 == 0,
                "installed package file is not a regular unprivileged file: {file}"
            );
            if file.starts_with("bin/") {
                ensure!(
                    metadata.mode() & 0o111 != 0,
                    "installed command is not executable: {file}"
                );
            }
            ensure!(
                fs::read_link(generation.join(relative))? == profile_link_target(name, file),
                "active package link was changed: {file}"
            );
        }
        Ok(package.clone())
    }

    /// Copy the validated publication, then activate a complete new profile.
    /// `update_only` refuses an absent package instead of implicitly installing it.
    pub fn install(
        &self,
        name: &str,
        version: &str,
        source_recipe: &Path,
        package_output: &Path,
        update_only: bool,
    ) -> Result<InstalledPackage> {
        valid_name(name)?;
        ensure!(
            !version.is_empty() && version.len() <= 256 && !version.chars().any(char::is_control),
            "package version must be a nonempty printable string of at most 256 bytes"
        );
        let recipe = source_recipe
            .canonicalize()
            .with_context(|| format!("cannot locate recipe {}", source_recipe.display()))?;
        let (root, _lock) = self.lock()?;
        let active = read_active(&root)?;
        ensure!(
            !update_only || active.packages.contains_key(name),
            "package {name} is not installed; use recipe install first"
        );
        let pending = new_generation(&root)?;
        let mut packages = active.packages.clone();
        copy_existing(&root, &active, pending.path(), Some(name))?;
        let destination = pending.path().join(PACKAGES).join(name);
        let files = copy_package(package_output, &destination, false)?;
        ensure!(!files.is_empty(), "package {name} has no installable files");
        let installed = InstalledPackage {
            name: name.to_owned(),
            version: version.to_owned(),
            recipe,
            files,
        };
        packages.insert(name.to_owned(), installed.clone());
        finish_generation(&root, pending, packages, active.target.as_deref())?;
        Ok(installed)
    }

    /// Deactivate only this package, using the installed receipt rather than
    /// whatever a changed or missing recipe would remove today.
    pub fn remove(&self, name: &str) -> Result<InstalledPackage> {
        valid_name(name)?;
        let (root, _lock) = self.lock()?;
        let active = read_active(&root)?;
        let mut packages = active.packages.clone();
        let removed = packages
            .remove(name)
            .with_context(|| format!("package {name} is not installed"))?;
        let pending = new_generation(&root)?;
        copy_existing(&root, &active, pending.path(), Some(name))?;
        finish_generation(&root, pending, packages, active.target.as_deref())?;
        Ok(removed)
    }

    fn lock(&self) -> Result<(PathBuf, File)> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(&self.root)?;
        real_owned_directory(&self.root)?;
        let root = self.root.canonicalize()?;
        let lock = crate::fsx::open_lease(&root.join(".packages.lock"), true)?;
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            crate::cancellation::check()?;
            match lock.try_lock_exclusive() {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    ensure!(
                        Instant::now() < deadline,
                        "package profile is busy: {}",
                        root.display()
                    );
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => return Err(error).context("cannot lock package profile"),
            }
        }
        create_directory(&root.join(GENERATIONS))?;
        Ok((root, lock))
    }
}

fn valid_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name.as_bytes()[0].is_ascii_alphanumeric()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b)),
        "invalid package name {name:?}"
    );
    Ok(())
}

fn valid_file(path: &str) -> Result<()> {
    let components: Vec<_> = Path::new(path).components().collect();
    ensure!(
        components.len() >= 2
            && components
                .iter()
                .all(|part| matches!(part, Component::Normal(_)))
            && !path.contains('\0')
            && !path.chars().any(char::is_control),
        "invalid package file path {path:?}"
    );
    ensure!(
        (components[0].as_os_str() == "bin" && components.len() == 2)
            || components[0].as_os_str() == "share",
        "package files must be bin/COMMAND or share/PATH: {path}"
    );
    // Components normalizes repeated slashes and trailing separators; receipts
    // contain one canonical relative spelling, never aliases of the same file.
    ensure!(
        components
            .iter()
            .map(|c| c.as_os_str().to_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("/")
            == path,
        "non-canonical package file path {path:?}"
    );
    Ok(())
}

fn readable_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir()
            && (metadata.uid() == unsafe { libc::geteuid() } || metadata.uid() == 0)
            && metadata.mode() & 0o022 == 0,
        "package directory must be real, owned by the current user or root and not writable by others: {}",
        path.display()
    );
    Ok(())
}

fn real_owned_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o022 == 0,
        "package directory must be real, owned by the current user and not writable by others: {}",
        path.display()
    );
    Ok(())
}

fn create_directory(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o755).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    real_owned_directory(path)
}

fn profile_target(root: &Path) -> Result<Option<PathBuf>> {
    let profile = root.join("profile");
    match fs::symlink_metadata(&profile) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
        Ok(metadata) => {
            ensure!(
                metadata.file_type().is_symlink(),
                "refusing to replace unmanaged profile {}",
                profile.display()
            );
            let target = fs::read_link(&profile)?;
            let parts: Vec<_> = target.components().collect();
            ensure!(
                parts.len() == 2
                    && parts[0].as_os_str() == GENERATIONS
                    && matches!(parts[1], Component::Normal(_))
                    && parts[1].as_os_str().to_str().is_some_and(|name| {
                        name.starts_with("g-")
                            && name.len() <= 64
                            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    }),
                "profile does not point to a managed package generation: {}",
                profile.display()
            );
            Ok(Some(target))
        }
    }
}

fn read_active(root: &Path) -> Result<Active> {
    read_active_checked(root, real_owned_directory)
}

/// `directory` is the ownership rule for the store's directories: the caller's
/// own for anything that changes the store, the caller's or root's for a read.
fn read_active_checked(root: &Path, directory: fn(&Path) -> Result<()>) -> Result<Active> {
    let Some(target) = profile_target(root)? else {
        return Ok(Active {
            target: None,
            packages: BTreeMap::new(),
        });
    };
    directory(&root.join(GENERATIONS))?;
    let generation = root.join(&target);
    directory(&generation)?;
    directory(&generation.join(PACKAGES))?;
    let mut bytes = Vec::new();
    crate::fsx::open_regular(&generation.join(RECEIPT))?
        .take(MAX_RECEIPT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_RECEIPT,
        "installed package receipt is too large"
    );
    let receipt: Receipt =
        serde_json::from_slice(&bytes).context("invalid installed package receipt")?;
    ensure!(
        receipt.schema_version == 1,
        "unsupported installed package receipt version"
    );
    for (name, package) in &receipt.packages {
        valid_name(name)?;
        ensure!(&package.name == name, "installed package name mismatch");
        let mut seen = BTreeSet::new();
        for path in &package.files {
            valid_file(path)?;
            ensure!(seen.insert(path), "duplicate installed package path {path}");
        }
        directory(&generation.join(PACKAGES).join(name))?;
    }
    Ok(Active {
        target: Some(target),
        packages: receipt.packages,
    })
}

fn new_generation(root: &Path) -> Result<tempfile::TempDir> {
    let generation = tempfile::Builder::new()
        .prefix("g-")
        .tempdir_in(root.join(GENERATIONS))?;
    create_directory(&generation.path().join(PACKAGES))?;
    Ok(generation)
}

fn copy_existing(root: &Path, active: &Active, pending: &Path, omit: Option<&str>) -> Result<()> {
    for (name, package) in &active.packages {
        if omit == Some(name.as_str()) {
            continue;
        }
        let source = root
            .join(
                active
                    .target
                    .as_ref()
                    .context("missing installed generation")?,
            )
            .join(PACKAGES)
            .join(name);
        let destination = pending.join(PACKAGES).join(name);
        let files = copy_package(&source, &destination, true)?;
        ensure!(
            files == package.files,
            "installed files no longer match the receipt for {name}"
        );
    }
    Ok(())
}

/// Package outputs deliberately expose a small install surface. No symlink,
/// device, special permission, arbitrary host path or activation script crosses
/// this boundary, and no program in the publication is executed here.
fn copy_package(source: &Path, destination: &Path, retained: bool) -> Result<Vec<String>> {
    let metadata = fs::symlink_metadata(source)?;
    ensure!(metadata.is_dir(), "package output is not a real directory");
    ensure!(
        metadata.mode() & 0o6000 == 0,
        "package output directory has setuid/setgid permissions"
    );
    create_directory(destination)?;
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(source)
        .follow_links(false)
        .min_depth(1)
        .sort_by_file_name()
    {
        crate::cancellation::check()?;
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let relative_text = relative.to_str().context("package paths must be UTF-8")?;
        let metadata = fs::symlink_metadata(entry.path())?;
        ensure!(
            metadata.mode() & 0o6000 == 0,
            "package has setuid/setgid permissions: {relative_text}"
        );
        ensure!(
            metadata.is_dir() || metadata.is_file(),
            "package symlinks and special files are unsupported: {relative_text}"
        );
        let first = relative.components().next().context("empty package path")?;
        ensure!(
            first.as_os_str() == "bin" || first.as_os_str() == "share",
            "unsupported package directory: {relative_text}"
        );
        let output = destination.join(relative);
        if metadata.is_dir() {
            ensure!(
                relative
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))),
                "invalid package directory"
            );
            create_directory(&output)?;
        } else {
            valid_file(relative_text)?;
            if first.as_os_str() == "bin" {
                ensure!(
                    metadata.mode() & 0o111 != 0,
                    "package command is not executable: {relative_text}"
                );
            }
            if retained {
                fs::hard_link(entry.path(), &output)?;
                ensure!(
                    fs::symlink_metadata(&output)?.is_file(),
                    "installed package file changed during activation"
                );
            } else {
                let mut input = crate::fsx::open_regular(entry.path())?;
                let mode = if metadata.mode() & 0o111 != 0 {
                    0o555
                } else {
                    0o444
                };
                let mut target = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&output)?;
                std::io::copy(&mut input, &mut target)?;
                target.set_permissions(fs::Permissions::from_mode(mode))?;
                target.sync_all()?;
            }
            files.push(relative_text.to_owned());
        }
    }
    files.sort();
    Ok(files)
}

fn finish_generation(
    root: &Path,
    pending: tempfile::TempDir,
    packages: BTreeMap<String, InstalledPackage>,
    previous: Option<&Path>,
) -> Result<()> {
    let mut owners = BTreeMap::<&str, &str>::new();
    for (name, package) in &packages {
        for file in &package.files {
            if let Some(other) = owners.insert(file, name) {
                bail!("package file conflict: {file} is provided by both {other} and {name}");
            }
            let destination = pending.path().join(file);
            let parent = destination.parent().context("package file has no parent")?;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o755)
                .create(parent)?;
            // The generated profile only links into its own immutable package
            // snapshots. Directory/file conflicts fail before profile activation.
            symlink(profile_link_target(name, file), &destination).with_context(|| {
                format!("cannot expose package file {file}; conflicting file or directory")
            })?;
        }
    }
    let receipt = serde_json::to_vec(&Receipt {
        schema_version: 1,
        packages,
    })?;
    ensure!(
        receipt.len() as u64 <= MAX_RECEIPT,
        "installed package receipt is too large"
    );
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(pending.path().join(RECEIPT))?;
    file.write_all(&receipt)?;
    file.sync_all()?;
    fs::set_permissions(pending.path(), fs::Permissions::from_mode(0o755))?;
    for entry in walkdir::WalkDir::new(pending.path())
        .follow_links(false)
        .contents_first(true)
    {
        let entry = entry?;
        if entry.file_type().is_dir() {
            File::open(entry.path())?.sync_all()?;
        }
    }
    let generation = pending.keep();
    File::open(root.join(GENERATIONS))?.sync_all()?;
    let target =
        PathBuf::from(GENERATIONS).join(generation.file_name().context("generation name missing")?);
    let temporary = tempfile::Builder::new()
        .prefix(".profile-")
        .tempfile_in(root)?
        .into_temp_path();
    fs::remove_file(&temporary)?;
    symlink(&target, &temporary)?;
    ensure!(
        profile_target(root)?.as_deref() == previous,
        "package profile changed during activation"
    );
    crate::cancellation::check()?;
    if previous.is_some() {
        fs::rename(&temporary, root.join("profile"))?;
    } else {
        let source = CString::new(temporary.as_os_str().as_bytes())?;
        let destination = CString::new(root.join("profile").as_os_str().as_bytes())?;
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .context("refusing to replace an existing unmanaged profile");
        }
    }
    File::open(root)?
        .sync_all()
        .context("profile activated, but syncing its directory failed")?;
    Ok(())
}

fn profile_link_target(name: &str, file: &str) -> PathBuf {
    let mut target = PathBuf::new();
    for _ in 1..Path::new(file).components().count() {
        target.push("..");
    }
    target.push(PACKAGES);
    target.push(name);
    target.push(file);
    target
}
