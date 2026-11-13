//! Activate published package trees without changing the recipe execution engine.
//!
//! A profile generation contains its receipt and all active package files. One
//! symlink replacement commits installation, update or removal. Old generations
//! remain inactive; removal needs neither the source recipe nor a Ginux instance.
//! An earlier generation is what `go_back` returns a package to, until
//! `clear_earlier` deletes them.

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

mod history;
pub use history::HistoryEntry;

/// An active package, recorded alongside the profile that exposes its files.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub recipe: PathBuf,
    pub files: Vec<String>,
    /// For a recipe fetched from a remote source: the source's name, the
    /// commit the recipe file was fetched at and the file's SHA-256.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// What the recipe was allowed to use when this was installed, so that a
    /// later recipe can be compared with it. Absent in older receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<crate::needs::Needs>,
    /// When this version was activated, in seconds since 1970.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_on: Option<u64>,
    /// The length of its files together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    /// Held: 'recipe update' leaves it as it is. Absent when not held.
    #[serde(default, skip_serializing_if = "is_false")]
    pub held: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// An earlier version of a package that an inactive generation still holds.
#[derive(Clone, Debug, Serialize)]
pub struct KeptPackage {
    pub version: String,
    pub generation: String,
    pub installed_on: Option<u64>,
    pub size_bytes: Option<u64>,
}

/// What the store holds on disk. Files shared between generations are
/// counted once, with the active generation first.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Storage {
    /// The active generation: what is installed now.
    pub active_bytes: u64,
    /// Inactive generations, and what only they hold.
    pub earlier_generations: u64,
    pub earlier_bytes: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    packages: BTreeMap<String, InstalledPackage>,
    /// The generation this one replaced, which orders the earlier ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous: Option<String>,
}

struct Active {
    target: Option<PathBuf>,
    packages: BTreeMap<String, InstalledPackage>,
    previous: Option<String>,
}

/// One inactive generation as its receipt records it.
struct Earlier {
    name: String,
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

    /// The folder package `name` lives in once activated, through the profile
    /// symlink, so it is the same in every generation. A recipe sees it as
    /// `RUNTIME.paths.prefix`.
    pub fn package_prefix(&self, name: &str) -> Result<PathBuf> {
        valid_name(name)?;
        Ok(self.profile_path().join(PACKAGES).join(name))
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
            let bases: &[&PathBuf] = if private(file) {
                &[&payload]
            } else {
                &[&generation, &payload]
            };
            for base in bases {
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
                private(file)
                    || fs::read_link(generation.join(relative))? == profile_link_target(name, file),
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
        needs: Option<&crate::needs::Needs>,
        package_output: &Path,
        update_only: bool,
    ) -> Result<InstalledPackage> {
        let origin = crate::sources::origin(source_recipe);
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
        let mut size_bytes = 0;
        for file in &files {
            size_bytes += fs::symlink_metadata(destination.join(file))?.len();
        }
        let installed = InstalledPackage {
            name: name.to_owned(),
            version: version.to_owned(),
            recipe,
            files,
            source: origin.as_ref().map(|origin| origin.source.clone()),
            commit: origin.as_ref().map(|origin| origin.commit.clone()),
            sha256: origin.map(|origin| origin.sha256),
            needs: needs.cloned(),
            installed_on: Some(now_seconds()),
            size_bytes: Some(size_bytes),
            // Installing again keeps a package held.
            held: active.packages.get(name).is_some_and(|before| before.held),
        };
        let before = active
            .packages
            .get(name)
            .map(|before| before.version.clone());
        packages.insert(name.to_owned(), installed.clone());
        finish_generation(&root, pending, packages, active.target.as_deref())?;
        history::record(
            &root,
            HistoryEntry::new(
                if update_only { "update" } else { "install" },
                Some(name),
                before.as_deref(),
                Some(version),
                installed.source.as_deref(),
            ),
        );
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
        history::record(
            &root,
            HistoryEntry::new(
                "remove",
                Some(name),
                Some(&removed.version),
                None,
                removed.source.as_deref(),
            ),
        );
        Ok(removed)
    }

    /// Hold or release one package: a new generation that differs only in the
    /// receipt's `held`. Holding a held package changes nothing.
    pub fn set_held(&self, name: &str, held: bool) -> Result<InstalledPackage> {
        valid_name(name)?;
        let (root, _lock) = self.lock()?;
        let active = read_active(&root)?;
        let now = active
            .packages
            .get(name)
            .with_context(|| format!("package {name} is not installed"))?;
        if now.held == held {
            return Ok(now.clone());
        }
        let pending = new_generation(&root)?;
        let mut packages = active.packages.clone();
        copy_existing(&root, &active, pending.path(), None)?;
        let changed = InstalledPackage {
            held,
            ..now.clone()
        };
        packages.insert(name.to_owned(), changed.clone());
        finish_generation(&root, pending, packages, active.target.as_deref())?;
        history::record(
            &root,
            HistoryEntry::new(
                if held { "hold" } else { "unhold" },
                Some(name),
                Some(&changed.version),
                Some(&changed.version),
                changed.source.as_deref(),
            ),
        );
        Ok(changed)
    }

    /// The earlier versions of each active package that can be gone back to,
    /// newest first, each version once. Reading changes nothing.
    pub fn kept(&self) -> Result<BTreeMap<String, Vec<KeptPackage>>> {
        let mut kept = BTreeMap::<String, Vec<KeptPackage>>::new();
        if !self.root.try_exists()? {
            return Ok(kept);
        }
        readable_directory(&self.root)?;
        let active = read_active_checked(&self.root, readable_directory)?;
        for earlier in earlier_generations(&self.root, &active, readable_directory)? {
            for (name, package) in &earlier.packages {
                let Some(now) = active.packages.get(name) else {
                    continue;
                };
                let versions = kept.entry(name.clone()).or_default();
                if same_installation(package, now)
                    || versions.iter().any(|kept| kept.version == package.version)
                {
                    continue;
                }
                versions.push(KeptPackage {
                    version: package.version.clone(),
                    generation: earlier.name.clone(),
                    installed_on: package.installed_on,
                    size_bytes: package.size_bytes,
                });
            }
        }
        kept.retain(|_, versions| !versions.is_empty());
        Ok(kept)
    }

    /// Activate an earlier version of one package again: the newest one that
    /// differs from the active one, or the newest of `version`. Its files are
    /// the ones an earlier generation still holds; no recipe is run. The
    /// other packages stay as they are, and the version left behind stays in
    /// its generation, so going back can itself be gone back on.
    pub fn go_back(&self, name: &str, version: Option<&str>) -> Result<InstalledPackage> {
        valid_name(name)?;
        let (root, _lock) = self.lock()?;
        let active = read_active(&root)?;
        let now = active
            .packages
            .get(name)
            .with_context(|| format!("package {name} is not installed"))?;
        let (generation, earlier) = earlier_generations(&root, &active, real_owned_directory)?
            .into_iter()
            .find_map(|earlier| {
                let package = earlier.packages.get(name)?;
                (!same_installation(package, now)
                    && version.is_none_or(|version| version == package.version))
                .then(|| (earlier.name.clone(), package.clone()))
            })
            .with_context(|| match version {
                Some(version) => format!("no earlier generation holds {name} {version}"),
                None => format!("no earlier version of {name} is kept"),
            })?;
        let pending = new_generation(&root)?;
        let mut packages = active.packages.clone();
        copy_existing(&root, &active, pending.path(), Some(name))?;
        let source = root
            .join(GENERATIONS)
            .join(&generation)
            .join(PACKAGES)
            .join(name);
        let files = copy_package(&source, &pending.path().join(PACKAGES).join(name), true)?;
        ensure!(
            files == earlier.files,
            "the kept files of {name} {} no longer match their receipt",
            earlier.version
        );
        // Going back keeps the package held or free as it is now.
        let back = InstalledPackage {
            installed_on: Some(now_seconds()),
            held: now.held,
            ..earlier
        };
        let left = now.version.clone();
        packages.insert(name.to_owned(), back.clone());
        finish_generation(&root, pending, packages, active.target.as_deref())?;
        history::record(
            &root,
            HistoryEntry::new(
                "go-back",
                Some(name),
                Some(&left),
                Some(&back.version),
                back.source.as_deref(),
            ),
        );
        Ok(back)
    }

    /// What the store holds on disk. Reading changes nothing.
    pub fn storage(&self) -> Result<Storage> {
        let mut storage = Storage::default();
        if !self.root.try_exists()? {
            return Ok(storage);
        }
        readable_directory(&self.root)?;
        let active = read_active_checked(&self.root, readable_directory)?;
        let mut seen = BTreeSet::new();
        if let Some(target) = &active.target {
            storage.active_bytes = generation_bytes(&self.root.join(target), &mut seen)?;
        }
        for earlier in earlier_generations(&self.root, &active, readable_directory)? {
            storage.earlier_generations += 1;
            storage.earlier_bytes +=
                generation_bytes(&self.root.join(GENERATIONS).join(&earlier.name), &mut seen)?;
        }
        Ok(storage)
    }

    /// Delete every inactive generation. Nothing that is installed changes;
    /// afterwards no package can be gone back on. Returns what `storage`
    /// counted as earlier.
    pub fn clear_earlier(&self) -> Result<Storage> {
        let (root, _lock) = self.lock()?;
        let active = read_active(&root)?;
        let mut seen = BTreeSet::new();
        let mut cleared = Storage::default();
        if let Some(target) = &active.target {
            cleared.active_bytes = generation_bytes(&root.join(target), &mut seen)?;
        }
        let active_name = active.target.as_ref().and_then(|target| target.file_name());
        for name in generation_names(&root)? {
            if Some(std::ffi::OsStr::new(&name)) == active_name {
                continue;
            }
            crate::cancellation::check()?;
            let generation = root.join(GENERATIONS).join(&name);
            real_owned_directory(&generation)?;
            cleared.earlier_generations += 1;
            cleared.earlier_bytes += generation_bytes(&generation, &mut seen)?;
            // Package files are read-only; their directories are not.
            fs::remove_dir_all(&generation)
                .with_context(|| format!("cannot delete {}", generation.display()))?;
        }
        File::open(root.join(GENERATIONS))?.sync_all()?;
        history::record(
            &root,
            HistoryEntry::new("clear-earlier", None, None, None, None),
        );
        Ok(cleared)
    }

    fn lock(&self) -> Result<(PathBuf, File)> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(&self.root)?;
        // A group-writable umask (002, Ubuntu's default) leaves the store
        // group-writable when Recipe or a source fetch created it. The store
        // is Recipe's own: tighten a real directory this user owns.
        let metadata = fs::symlink_metadata(&self.root)?;
        if metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o022 != 0
        {
            fs::set_permissions(
                &self.root,
                fs::Permissions::from_mode(metadata.mode() & 0o7755),
            )?;
        }
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

/// `lib/` is what a package carries for its own programs: kept in the package,
/// linked nowhere, so two packages never meet over it.
fn private(file: &str) -> bool {
    file.starts_with("lib/")
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
            || components[0].as_os_str() == "share"
            || components[0].as_os_str() == "lib",
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
            previous: None,
        });
    };
    directory(&root.join(GENERATIONS))?;
    let receipt = read_receipt(&root.join(&target), directory)?;
    Ok(Active {
        target: Some(target),
        packages: receipt.packages,
        previous: receipt.previous,
    })
}

/// The checked receipt of one generation.
fn read_receipt(generation: &Path, directory: fn(&Path) -> Result<()>) -> Result<Receipt> {
    directory(generation)?;
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
    Ok(receipt)
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Whether two receipts record the same installation of a package.
fn same_installation(a: &InstalledPackage, b: &InstalledPackage) -> bool {
    a.version == b.version && a.files == b.files && a.sha256 == b.sha256 && a.recipe == b.recipe
}

/// The names of every finished generation in the store.
fn generation_names(root: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for item in fs::read_dir(root.join(GENERATIONS))? {
        let item = item?;
        // A generation still being made has no receipt yet.
        if let Some(name) = item.file_name().to_str()
            && name.starts_with("g-")
            && item.path().join(RECEIPT).is_file()
        {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// The inactive generations, newest first: the ones each receipt names as
/// its predecessor, then the rest (older receipts name none) by their time.
/// A generation that cannot be read is left out; it cannot be gone back to.
fn earlier_generations(
    root: &Path,
    active: &Active,
    directory: fn(&Path) -> Result<()>,
) -> Result<Vec<Earlier>> {
    let Some(active_name) = active
        .target
        .as_ref()
        .and_then(|target| target.file_name()?.to_str())
    else {
        return Ok(Vec::new());
    };
    let mut receipts = BTreeMap::new();
    for name in generation_names(root)? {
        if name != active_name
            && let Ok(receipt) = read_receipt(&root.join(GENERATIONS).join(&name), directory)
        {
            receipts.insert(name, receipt);
        }
    }
    let mut ordered = Vec::new();
    let mut next = active.previous.clone();
    while let Some(name) = next {
        let Some(receipt) = receipts.remove(&name) else {
            break;
        };
        next = receipt.previous.clone();
        ordered.push(Earlier {
            name,
            packages: receipt.packages,
        });
    }
    let mut rest: Vec<_> = receipts
        .into_iter()
        .map(|(name, receipt)| {
            let made = fs::metadata(root.join(GENERATIONS).join(&name).join(RECEIPT))
                .and_then(|metadata| metadata.modified())
                .ok();
            (made, name, receipt)
        })
        .collect();
    rest.sort_by(|a, b| (b.0, &b.1).cmp(&(a.0, &a.1)));
    ordered.extend(rest.into_iter().map(|(_, name, receipt)| Earlier {
        name,
        packages: receipt.packages,
    }));
    Ok(ordered)
}

/// The length of the files of one generation that `seen` does not hold yet.
fn generation_bytes(generation: &Path, seen: &mut BTreeSet<(u64, u64)>) -> Result<u64> {
    let mut bytes = 0;
    for entry in walkdir::WalkDir::new(generation).follow_links(false) {
        let metadata = entry?.metadata()?;
        if metadata.is_file() && seen.insert((metadata.dev(), metadata.ino())) {
            bytes += metadata.len();
        }
    }
    Ok(bytes)
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
    let mut copied = Vec::new();
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
            first.as_os_str() == "bin"
                || first.as_os_str() == "share"
                || first.as_os_str() == "lib",
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
                copied.push(output);
            }
            files.push(relative_text.to_owned());
        }
    }
    // Copied bytes are durable before the generation's directories are synced
    // and its profile is switched (`finish_generation`): one batch instead of
    // one fsync per file. Hardlinked files are a durable generation's already.
    crate::fsx::sync_files(
        &copied
            .iter()
            .map(|path| (path.as_path(), None))
            .collect::<Vec<_>>(),
    )?;
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
        for file in package.files.iter().filter(|file| !private(file)) {
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
        previous: previous
            .and_then(|target| target.file_name()?.to_str())
            .map(str::to_owned),
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
