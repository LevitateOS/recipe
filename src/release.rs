//! A system release as Recipe applies it (`docs/release-engineering.md`
//! sections 6 and 10): the manifest `release.json`, the base tree in
//! `base.tar.zst` as the reserved receipt entry `base`, and the group recipes
//! in `recipes.tar.zst`.
//!
//! The base archive, as the builder writes it:
//!
//! - a tar (GNU or POSIX/pax) compressed with zstd, of the root tree as it
//!   boots, member names relative to the root (`usr/bin/sh` or
//!   `./usr/bin/sh`; never absolute, never `..`);
//! - every directory has its own member; regular files, directories,
//!   symbolic links and hard links only (a hard link names an earlier regular
//!   member); nothing below a member that is not a directory;
//! - owners are the tree's numeric ids (`--numeric-owner`); the names in the
//!   tar headers are ignored. What an id means is read from the archive's own
//!   `etc/passwd` and `etc/group`, and a file is given the id that name has
//!   in the root it is written into;
//! - modes as installed, including setuid, setgid and sticky bits;
//! - extended attributes as pax `SCHILY.xattr.NAME` records (GNU tar
//!   `--xattrs --xattrs-include='*'`), file capabilities among them;
//!   `security.selinux` is ignored (the installed policy labels files);
//! - `var/` is state: what it holds is created in the system's `/var` where
//!   missing and never overwritten or recorded (as recipe state seeds);
//! - left out, if present: `boot`, `home`, `root`, `proc`, `sys`, `dev`,
//!   `run`, `tmp`, `lost+found` and what is below them; `usr/lib/recab/`;
//!   Recipe's receipt and profile (`usr/lib/recipe/system.json`,
//!   `usr/lib/recipe/profile/`); the machine files ([`MACHINE_FILES`]); and
//!   `usr/lib/levitate/release.json`, which Recipe writes from the manifest
//!   itself (the archive cannot carry the manifest that names its digest).
//!
//! The base entry in an image is written at build time by
//! `recipe --system --root TREE record-base --release release.json` from
//! the same rules ([`record_base`]).
use crate::system::{self, BASE_PACKAGE, Package, Payload, Receipt};
use crate::target::{self, Entry};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// Where a root records the release it runs.
pub const INSTALLED: &str = "usr/lib/levitate/release.json";
/// The base archive in a fetched release folder.
pub const BASE_ARCHIVE: &str = "base.tar.zst";
/// The group recipes archive in a fetched release folder.
pub const RECIPES_ARCHIVE: &str = "recipes.tar.zst";

/// Files the installation makes for this one machine (or keeps up to date
/// for it). They are never part of `base`: an update does not record, write
/// or compare them, whatever the archive carries. The account files are
/// added to instead ([`account_lines`]).
pub const MACHINE_FILES: [&str; 14] = [
    "etc/fstab",
    "etc/crypttab",
    "etc/passwd",
    "etc/group",
    "etc/shadow",
    "etc/gshadow",
    "etc/subuid",
    "etc/subgid",
    "etc/machine-id",
    "etc/hostname",
    "etc/localtime",
    "etc/adjtime",
    "etc/mtab",
    "etc/resolv.conf",
];

/// Top-level paths that are not the base's: mount points, shared state and
/// what the boot step owns.
const NOT_BASE: [&str; 9] = [
    "boot",
    "home",
    "root",
    "proc",
    "sys",
    "dev",
    "run",
    "tmp",
    "lost+found",
];

#[derive(Clone, Debug, Deserialize)]
pub struct Archive {
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RecipesArchive {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub revision: String,
}

/// `release.json`. Fields not read here (`iso`, `notes`, ...) are allowed.
#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub os: String,
    pub version: String,
    pub serial: u64,
    #[serde(default)]
    pub date: Option<String>,
    pub base: Archive,
    #[serde(default)]
    pub boot: Option<Archive>,
    pub recipes: RecipesArchive,
}

/// A manifest with the exact bytes it was read from.
#[derive(Clone, Debug)]
pub struct Release {
    pub manifest: Manifest,
    pub bytes: Vec<u8>,
    pub digest: String,
}

/// `os`, `version` and `serial` of a release file, which is all an installed
/// root's record and `record-base` need.
#[derive(Clone, Debug, Deserialize)]
pub struct Installed {
    pub os: String,
    pub version: String,
    pub serial: u64,
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Read the release manifest at `path`.
pub fn read(path: &Path) -> Result<Release> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        bytes.len() <= 1024 * 1024,
        "{} is larger than a release manifest can be",
        path.display()
    );
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a release manifest", path.display()))?;
    ensure!(
        manifest.schema == 1,
        "{} has release schema {}; this Recipe reads schema 1",
        path.display(),
        manifest.schema
    );
    ensure!(
        !manifest.os.is_empty() && !manifest.version.is_empty(),
        "{} names no os or version",
        path.display()
    );
    Ok(Release {
        digest: sha256(&bytes),
        manifest,
        bytes,
    })
}

/// The release the root at `root` runs, if it records one.
pub fn installed(root: &Path) -> Result<Option<Installed>> {
    let path = root.join(INSTALLED);
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
            format!("{} is not a release manifest", path.display())
        })?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Check the two archives of the release folder `folder` against the
/// manifest's sizes and SHA-256 values, before anything is unpacked.
pub fn verify(folder: &Path, release: &Release) -> Result<()> {
    let m = &release.manifest;
    let files = [
        (BASE_ARCHIVE, m.base.size, &m.base.sha256),
        (RECIPES_ARCHIVE, m.recipes.size, &m.recipes.sha256),
    ];
    let paths: Vec<PathBuf> = files.iter().map(|(n, _, _)| folder.join(n)).collect();
    for ((name, size, _), path) in files.iter().zip(&paths) {
        let md = fs::metadata(path).with_context(|| {
            format!(
                "{} has no {name}; `recupdate fetch` downloads the release into its folder",
                folder.display()
            )
        })?;
        ensure!(
            md.is_file() && md.len() == *size,
            "{} is {} bytes; the release manifest says {size}. Fetch the release again.",
            path.display(),
            md.len()
        );
    }
    let digests = target::hash_all(&paths.iter().map(PathBuf::as_path).collect::<Vec<_>>())?;
    for ((name, _, want), have) in files.iter().zip(digests) {
        ensure!(
            have.eq_ignore_ascii_case(want),
            "{name} in {} does not have the SHA-256 the release manifest names ({have} instead of {want}). Fetch the release again.",
            folder.display()
        );
    }
    Ok(())
}

/// Where a root-relative path of a base tree goes.
#[derive(PartialEq)]
enum Place {
    /// Part of the base.
    Base,
    /// Below `var/`: a state seed.
    Seed,
    /// Not the base's (see the module documentation).
    Left,
}

fn place(rel: &str) -> Place {
    let top = rel.split('/').next().unwrap_or_default();
    if NOT_BASE.contains(&top)
        || rel == "usr/lib/recab"
        || rel.starts_with("usr/lib/recab/")
        || rel == system::RECEIPT
        || rel == system::PROFILE
        || rel.starts_with(&format!("{}/", system::PROFILE))
        || rel == INSTALLED
        || MACHINE_FILES.contains(&rel)
        || rel == "var"
    {
        return Place::Left;
    }
    if top == "var" {
        return Place::Seed;
    }
    Place::Base
}

/// A member name as a root-relative path.
fn member_name(path: &Path) -> Result<Option<String>> {
    let rel = target::relative(path)?;
    if rel.as_os_str().is_empty() {
        return Ok(None);
    }
    Ok(Some(
        rel.to_str()
            .context("a base archive member has a name that is not UTF-8")?
            .to_owned(),
    ))
}

/// Names by id in a passwd or group file.
fn names(text: &str) -> BTreeMap<u32, String> {
    crate::accounts::ids(text)
        .into_iter()
        .map(|(name, id)| (id, name))
        .collect()
}

/// sysusers.d lines that make the accounts of the base's `passwd` and
/// `group` the root at `root` lacks, with the base's ids where the root has
/// them free. An account the root has is left as it is (the users and
/// passwords the installation made among them). A new group also gets the
/// base's members.
pub fn account_lines(root: &Path, passwd: &str, group: &str) -> Result<Vec<String>> {
    let have = system::account_files(root)?;
    let (users, groups) = (
        crate::accounts::ids(&have.passwd),
        crate::accounts::ids(&have.group),
    );
    let user_ids: BTreeSet<u32> = users.values().copied().collect();
    let group_ids: BTreeSet<u32> = groups.values().copied().collect();
    let quote = |value: &str| -> Option<String> {
        if value.is_empty() {
            Some("-".into())
        } else if !value.contains('\'') {
            Some(format!("'{value}'"))
        } else if !value.contains('"') {
            Some(format!("\"{value}\""))
        } else {
            None
        }
    };
    let mut lines = Vec::new();
    let mut members = Vec::new();
    for line in group.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 3 || f[0].is_empty() || f[0].starts_with('#') || groups.contains_key(f[0]) {
            continue;
        }
        let id = match f[2].parse::<u32>() {
            Ok(id) if !group_ids.contains(&id) => id.to_string(),
            _ => "-".into(),
        };
        lines.push(format!("g {} {id}", f[0]));
        for member in f.get(3).copied().unwrap_or_default().split(',') {
            if !member.is_empty() {
                members.push(format!("m {member} {}", f[0]));
            }
        }
    }
    let base_groups = names(group);
    for line in passwd.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() < 7 || f[0].is_empty() || f[0].starts_with('#') || users.contains_key(f[0]) {
            continue;
        }
        let uid = f[2].parse::<u32>().ok().filter(|id| !user_ids.contains(id));
        let gid = f[3]
            .parse::<u32>()
            .ok()
            .and_then(|gid| base_groups.get(&gid).cloned());
        let id = match (uid, gid) {
            (Some(uid), Some(group)) => format!("{uid}:{group}"),
            (Some(uid), None) => uid.to_string(),
            (None, _) => "-".into(),
        };
        let (Some(gecos), Some(home), Some(shell)) = (quote(f[4]), quote(f[5]), quote(f[6])) else {
            continue;
        };
        lines.push(format!("u {} {id} {gecos} {home} {shell}", f[0]));
    }
    lines.extend(members);
    Ok(lines)
}

/// A private, empty folder on the filesystem of the root at `root`, where the
/// base is unpacked so its files can be moved into the root.
pub fn staging(root: &Path) -> Result<PathBuf> {
    let dir = root.join(".recipe-apply-release");
    if fs::symlink_metadata(&dir).is_ok() {
        crate::fsx::make_removable(&dir);
        fs::remove_dir_all(&dir)
            .with_context(|| format!("removing the earlier staging folder {}", dir.display()))?;
    }
    fs::create_dir(&dir).with_context(|| format!("creating {}", dir.display()))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// Remove a staging folder.
pub fn discard(dir: &Path) {
    crate::fsx::make_removable(dir);
    let _ = fs::remove_dir_all(dir);
}

/// Make the missing directories above `rel` in `dir`, refusing a path below
/// a member that is not a directory.
fn parents(dir: &Path, rel: &str, kinds: &BTreeMap<String, String>) -> Result<PathBuf> {
    let mut path = dir.to_path_buf();
    let parts: Vec<&str> = rel.split('/').collect();
    let mut sofar = String::new();
    for part in &parts[..parts.len() - 1] {
        if !sofar.is_empty() {
            sofar.push('/');
        }
        sofar.push_str(part);
        if let Some(kind) = kinds.get(&sofar) {
            ensure!(
                kind == "dir",
                "the base archive has {rel} below {sofar}, which is not a directory"
            );
        }
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => bail!("the base archive has {rel} below {sofar}, which is not a directory"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&path)?;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
    Ok(dir.join(rel))
}

/// The base tree unpacked: the payload of the package `base`.
pub struct Unpacked {
    pub payload: Payload,
    /// Members left out (not the base's), by count.
    pub left_out: usize,
}

/// Unpack `base.tar.zst` of `folder` into `dir` (from [`staging`]) and make
/// the payload of the package `base` for the root at `root`: every file of
/// the tree with its digest, mode, owner name and attributes, the release's
/// accounts the root lacks, `var/` as seeds, and the manifest itself as
/// `usr/lib/levitate/release.json`.
pub fn unpack_base(folder: &Path, release: &Release, root: &Path, dir: &Path) -> Result<Unpacked> {
    let archive = folder.join(BASE_ARCHIVE);
    let file = fs::File::open(&archive)?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .with_context(|| format!("{} is not zstd", archive.display()))?;
    let mut tar = tar::Archive::new(decoder);
    let mut files: BTreeMap<String, Entry> = BTreeMap::new();
    let mut seeds: BTreeMap<String, Entry> = BTreeMap::new();
    let mut paths: BTreeMap<String, String> = BTreeMap::new();
    let mut kinds: BTreeMap<String, String> = BTreeMap::new();
    let (mut passwd, mut group) = (String::new(), String::new());
    let mut left_out = 0;
    let mut buffer = vec![0u8; 1 << 16];
    for member in tar
        .entries()
        .with_context(|| format!("reading {}", archive.display()))?
    {
        let mut member = member.with_context(|| format!("reading {}", archive.display()))?;
        let Some(rel) = member_name(&member.path()?)? else {
            continue;
        };
        let kind = member.header().entry_type();
        let where_ = place(&rel);
        if where_ == Place::Left {
            if kind.is_file() && (rel == "etc/passwd" || rel == "etc/group") {
                let mut text = String::new();
                member
                    .by_ref()
                    .take(16 * 1024 * 1024)
                    .read_to_string(&mut text)
                    .with_context(|| format!("reading {rel} of the base archive"))?;
                if rel == "etc/passwd" {
                    passwd = text;
                } else {
                    group = text;
                }
            }
            left_out += 1;
            continue;
        }
        ensure!(
            !kinds.contains_key(&rel),
            "the base archive has {rel} twice"
        );
        let header = member.header();
        let mut xattrs = BTreeMap::new();
        let mode = header.mode()? & 0o7777;
        let uid = u32::try_from(header.uid()?).context("an owner id is too large")?;
        let gid = u32::try_from(header.gid()?).context("a group id is too large")?;
        if let Some(extensions) = member.pax_extensions()? {
            for extension in extensions {
                let extension = extension?;
                let Ok(key) = extension.key() else { continue };
                if let Some(name) = key.strip_prefix("SCHILY.xattr.")
                    && name != "security.selinux"
                {
                    xattrs.insert(name.to_owned(), hex::encode(extension.value_bytes()));
                }
            }
        }
        let mut entry = Entry {
            kind: String::new(),
            mode,
            uid,
            gid,
            user: None,
            group: None,
            xattrs,
            link: None,
            sha256: None,
        };
        if kind.is_dir() {
            entry.kind = "dir".into();
            let dest = parents(dir, &rel, &kinds)?;
            match fs::symlink_metadata(&dest) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => bail!("the base archive has {rel} as a directory and as something else"),
                Err(_) => fs::create_dir(&dest)?,
            }
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o700))?;
        } else if kind.is_symlink() {
            entry.kind = "symlink".into();
            entry.link = Some(
                member
                    .link_name()?
                    .context("a symbolic link member has no target")?
                    .to_str()
                    .context("a link target is not UTF-8")?
                    .to_owned(),
            );
            entry.mode = 0o777;
        } else if kind.is_hard_link() {
            let to = member_name(
                &member
                    .link_name()?
                    .context("a hard link member has no target")?,
            )?
            .context("a hard link names the root")?;
            let first = files
                .get(&to)
                .or_else(|| seeds.get(&to))
                .filter(|e| e.kind == "file")
                .with_context(|| {
                    format!("the hard link {rel} names {to}, which is not an earlier regular file")
                })?
                .clone();
            if where_ == Place::Seed {
                // A seed is copied, not linked: it takes its first name's bytes.
                let source = paths.get(&to).cloned().context("hard link seed source")?;
                paths.insert(rel.clone(), source);
                kinds.insert(rel.clone(), "file".into());
                seeds.insert(rel, first);
                continue;
            }
            ensure!(
                place(&to) == Place::Base,
                "the hard link {rel} names {to}, which is not part of the base"
            );
            entry = first;
            entry.kind = "hardlink".into();
            entry.link = Some(to);
        } else if kind.is_file() || kind == tar::EntryType::Continuous {
            entry.kind = "file".into();
            let dest = parents(dir, &rel, &kinds)?;
            let mut out = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&dest)
                .with_context(|| format!("unpacking {rel}"))?;
            let mut hash = Sha256::new();
            loop {
                let n = member.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
                out.write_all(&buffer[..n])?;
            }
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o600))?;
            entry.sha256 = Some(hex::encode(hash.finalize()));
        } else {
            bail!(
                "the base archive has {rel} of kind {kind:?}; a base holds regular files, directories and links only"
            );
        }
        if entry.kind != "hardlink" {
            paths.insert(rel.clone(), rel.clone());
        }
        kinds.insert(rel.clone(), entry.kind.clone());
        if where_ == Place::Seed {
            seeds.insert(rel, entry);
        } else {
            system::allowed(&rel)?;
            files.insert(rel, entry);
        }
    }
    ensure!(
        !files.is_empty(),
        "the base archive of {} holds no files",
        folder.display()
    );
    // The manifest itself, as this root's record of what it runs.
    let dest = parents(dir, INSTALLED, &kinds)?;
    fs::write(&dest, &release.bytes)?;
    files.insert(
        INSTALLED.into(),
        Entry {
            kind: "file".into(),
            mode: 0o644,
            uid: 0,
            gid: 0,
            user: None,
            group: None,
            xattrs: BTreeMap::new(),
            link: None,
            sha256: Some(release.digest.clone()),
        },
    );
    paths.insert(INSTALLED.into(), INSTALLED.into());
    // Owner names from the base's own account files: the ids are resolved
    // against the root it is written into.
    let (users, groups) = (names(&passwd), names(&group));
    for entry in files.values_mut().chain(seeds.values_mut()) {
        if entry.uid != 0
            && let Some(name) = users.get(&entry.uid)
        {
            entry.user = Some(name.clone());
        }
        if entry.gid != 0
            && let Some(name) = groups.get(&entry.gid)
        {
            entry.group = Some(name.clone());
        }
    }
    // A hard link carries its first name's owner.
    let firsts = files.clone();
    for entry in files.values_mut() {
        if entry.kind == "hardlink"
            && let Some(first) = entry.link.as_ref().and_then(|l| firsts.get(l))
        {
            entry.user = first.user.clone();
            entry.group = first.group.clone();
        }
    }
    let accounts = account_lines(root, &passwd, &group)?;
    Ok(Unpacked {
        payload: Payload {
            package: base_package(release, files),
            root: dir.to_path_buf(),
            paths,
            removals: Vec::new(),
            accounts,
            seeds,
        },
        left_out,
    })
}

fn base_package(release: &Release, files: BTreeMap<String, Entry>) -> Package {
    Package {
        name: BASE_PACKAGE.into(),
        version: release.manifest.version.clone(),
        recipe: PathBuf::from("/").join(INSTALLED),
        recipe_digest: release.digest.clone(),
        architecture: std::env::consts::ARCH.into(),
        target: "release".into(),
        dependencies: Vec::new(),
        files,
        accounts: Vec::new(),
    }
}

/// Unpack `recipes.tar.zst` of `folder` into the empty folder `dir`: regular
/// files and directories only.
pub fn unpack_recipes(folder: &Path, dir: &Path) -> Result<()> {
    let archive = folder.join(RECIPES_ARCHIVE);
    let decoder = zstd::stream::read::Decoder::new(fs::File::open(&archive)?)
        .with_context(|| format!("{} is not zstd", archive.display()))?;
    let mut tar = tar::Archive::new(decoder);
    let mut kinds = BTreeMap::new();
    for member in tar.entries()? {
        let mut member = member?;
        let Some(rel) = member_name(&member.path()?)? else {
            continue;
        };
        let kind = member.header().entry_type();
        let dest = parents(dir, &rel, &kinds)?;
        if kind.is_dir() {
            if fs::symlink_metadata(&dest).is_err() {
                fs::create_dir(&dest)?;
            }
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
            kinds.insert(rel, "dir".to_owned());
        } else if kind.is_file() || kind == tar::EntryType::Continuous {
            let mut out = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&dest)
                .with_context(|| format!("unpacking {rel} of the recipes archive"))?;
            std::io::copy(&mut member, &mut out)?;
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o644))?;
            kinds.insert(rel, "file".to_owned());
        } else {
            bail!("the recipes archive has {rel}, which is not a regular file or a directory");
        }
    }
    Ok(())
}

/// The names of the extended attributes of `path` (not followed).
fn xattr_names(path: &Path) -> Result<Vec<String>> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let mut buffer = vec![0u8; 65536];
    let len = unsafe { libc::llistxattr(c.as_ptr(), buffer.as_mut_ptr().cast(), buffer.len()) };
    if len < 0 {
        let error = std::io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ENOTSUP) => Ok(Vec::new()),
            _ => Err(error).with_context(|| format!("listing attributes of {}", path.display())),
        };
    }
    buffer.truncate(len as usize);
    Ok(buffer
        .split(|b| *b == 0)
        .filter(|n| !n.is_empty())
        .filter_map(|n| std::str::from_utf8(n).ok().map(str::to_owned))
        .collect())
}

/// What `record-base` recorded.
pub struct Recorded {
    pub files: usize,
    pub version: String,
    pub serial: u64,
}

/// Write the entry `base` for the image tree at `tree`, at build time: copy
/// the release manifest `manifest` (only `os`, `version` and `serial` are
/// read) to the tree's `usr/lib/levitate/release.json`, and record every
/// path of the tree that is the base's under the same rules as
/// [`unpack_base`] (numeric owners as they are in the tree, extended
/// attributes but `security.selinux`, `var/` and paths another package of
/// the tree's receipt owns left out) as the package `base` in the tree's
/// receipt. It is the last step before the image is made from the tree.
pub fn record_base(tree: &Path, manifest: &Path) -> Result<Recorded> {
    let bytes = fs::read(manifest).with_context(|| format!("reading {}", manifest.display()))?;
    let installed: Installed = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a release manifest", manifest.display()))?;
    let host = crate::clibrary::detect_root(tree);
    ensure!(
        host.os_id.as_deref() == Some(installed.os.as_str()),
        "{} is a release of {}, and the tree {} is {} ({})",
        manifest.display(),
        installed.os,
        tree.display(),
        host.os_id.as_deref().unwrap_or("of no known system"),
        host.os_found
    );
    let mut receipt = system::read_receipt(tree)?;
    // A tree a build staged keeps each file's owner, mode and attributes in
    // its target metadata, not on disk (the build does not run as root), and
    // the image is made from that metadata. The base entry records the same.
    let staged = tree.join(target::MANIFEST).exists();
    let mut metadata = if staged {
        target::load(tree)?
    } else {
        target::Manifest::default()
    };
    let dest = tree.join(INSTALLED);
    fs::create_dir_all(dest.parent().unwrap())?;
    fs::write(&dest, &bytes)?;
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o644))?;
    if unsafe { libc::geteuid() } == 0 {
        std::os::unix::fs::lchown(&dest, Some(0), Some(0))?;
    }
    if staged {
        // The folder made above is root's in the image too.
        let folder = Path::new(INSTALLED)
            .parent()
            .and_then(Path::to_str)
            .unwrap_or_default()
            .to_owned();
        metadata.entries.entry(folder).or_insert(Entry {
            kind: "dir".into(),
            mode: 0o755,
            uid: 0,
            gid: 0,
            user: None,
            group: None,
            xattrs: BTreeMap::new(),
            link: None,
            sha256: None,
        });
    }
    let others: BTreeSet<&String> = receipt
        .packages
        .values()
        .filter(|p| p.name != BASE_PACKAGE)
        .flat_map(|p| p.files.keys())
        .collect();
    let mut files = BTreeMap::new();
    let mut to_hash: Vec<(String, PathBuf)> = Vec::new();
    let mut inodes: BTreeMap<(u64, u64), String> = BTreeMap::new();
    let walk = walkdir::WalkDir::new(tree)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|item| {
            item.path()
                .strip_prefix(tree)
                .ok()
                .and_then(Path::to_str)
                .is_none_or(|rel| rel.is_empty() || place(rel) == Place::Base)
        });
    for item in walk {
        let item = item?;
        let rel = item
            .path()
            .strip_prefix(tree)?
            .to_str()
            .context("a tree path is not UTF-8")?
            .to_owned();
        if rel.is_empty() || others.contains(&rel) {
            continue;
        }
        system::allowed(&rel)?;
        let md = fs::symlink_metadata(item.path())?;
        let mut xattrs = BTreeMap::new();
        if !md.file_type().is_symlink() {
            for name in xattr_names(item.path())? {
                if name == "security.selinux" {
                    continue;
                }
                if let Some(value) = system::xattr(item.path(), &name)? {
                    xattrs.insert(name, hex::encode(value));
                }
            }
        }
        let mut entry = Entry {
            kind: String::new(),
            mode: md.mode() & 0o7777,
            uid: md.uid(),
            gid: md.gid(),
            user: None,
            group: None,
            xattrs,
            link: None,
            sha256: None,
        };
        if let Some(stated) = metadata.entries.get(&rel) {
            entry.mode = stated.mode & 0o7777;
            entry.uid = stated.uid;
            entry.gid = stated.gid;
            entry.xattrs = stated
                .xattrs
                .iter()
                .filter(|(name, _)| name.as_str() != "security.selinux")
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();
        }
        if md.is_dir() {
            entry.kind = "dir".into();
        } else if md.file_type().is_symlink() {
            entry.kind = "symlink".into();
            entry.link = Some(
                fs::read_link(item.path())?
                    .to_str()
                    .context("a link target is not UTF-8")?
                    .to_owned(),
            );
        } else if md.is_file() {
            entry.kind = "file".into();
            if md.nlink() > 1 {
                match inodes.get(&(md.dev(), md.ino())) {
                    Some(first) => {
                        entry.kind = "hardlink".into();
                        entry.link = Some(first.clone());
                    }
                    None => {
                        inodes.insert((md.dev(), md.ino()), rel.clone());
                    }
                }
            }
            if entry.kind == "file" {
                to_hash.push((rel.clone(), item.path().to_path_buf()));
            }
        } else {
            bail!(
                "{} is not a regular file, directory or link; a base holds no other kind",
                item.path().display()
            );
        }
        files.insert(rel, entry);
    }
    let digests = target::hash_all(&to_hash.iter().map(|(_, p)| p.as_path()).collect::<Vec<_>>())?;
    for ((rel, _), digest) in to_hash.iter().zip(digests) {
        files.get_mut(rel).unwrap().sha256 = Some(digest);
    }
    let firsts = files.clone();
    for entry in files.values_mut() {
        if entry.kind == "hardlink" {
            let first = &firsts[entry.link.as_ref().unwrap()];
            entry.sha256 = first.sha256.clone();
        }
    }
    files.insert(
        INSTALLED.into(),
        Entry {
            kind: "file".into(),
            mode: 0o644,
            uid: 0,
            gid: 0,
            user: None,
            group: None,
            xattrs: BTreeMap::new(),
            link: None,
            sha256: Some(sha256(&bytes)),
        },
    );
    let release = Release {
        manifest: Manifest {
            schema: 1,
            os: installed.os.clone(),
            version: installed.version.clone(),
            serial: installed.serial,
            date: None,
            base: Archive {
                url: String::new(),
                sha256: String::new(),
                size: 0,
            },
            boot: None,
            recipes: RecipesArchive {
                url: String::new(),
                sha256: String::new(),
                size: 0,
                revision: String::new(),
            },
        },
        digest: sha256(&bytes),
        bytes,
    };
    let count = files.len();
    receipt
        .packages
        .insert(BASE_PACKAGE.into(), base_package(&release, files));
    system::write_receipt(tree, &receipt)?;
    if staged {
        // The two files written here are root's in the image, like the rest.
        for rel in [INSTALLED, system::RECEIPT] {
            let path = tree.join(rel);
            let digest = target::hash_all(&[path.as_path()])?.remove(0);
            metadata.entries.insert(
                rel.into(),
                Entry {
                    kind: "file".into(),
                    mode: 0o644,
                    uid: 0,
                    gid: 0,
                    user: None,
                    group: None,
                    xattrs: BTreeMap::new(),
                    link: None,
                    sha256: Some(digest),
                },
            );
        }
        target::save(tree, &metadata)?;
    }
    Ok(Recorded {
        files: count,
        version: installed.version,
        serial: installed.serial,
    })
}

/// A record of an apply in progress on a running root, so that a power loss
/// in the middle can be finished by running the same command again.
#[derive(Clone, Debug, serde::Serialize, Deserialize)]
pub struct Progress {
    pub schema: u32,
    pub root: PathBuf,
    pub folder: PathBuf,
    pub os: String,
    pub version: String,
    pub serial: u64,
    /// `base`, `groups` or `effects`: the step that was started last.
    pub step: String,
    pub started_at: u64,
}

impl Progress {
    pub fn load(path: &Path) -> Result<Option<Self>> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                format!("{} is not a record of an update", path.display())
            })?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Write the record durably before the step it names starts.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension("json.new");
        let mut out = fs::File::create(&temp)?;
        out.write_all(&serde_json::to_vec_pretty(self)?)?;
        out.sync_all()?;
        fs::rename(&temp, path)?;
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}

/// The receipt's packages an update installs again from the release's
/// recipes: all but `base`, the effects package and the kept modules of the
/// previous kernel.
pub fn groups(receipt: &Receipt) -> Vec<String> {
    receipt
        .packages
        .keys()
        .filter(|n| {
            *n != BASE_PACKAGE
                && *n != system::EFFECTS_PACKAGE
                && *n != system::PREVIOUS_KERNEL_PACKAGE
        })
        .cloned()
        .collect()
}
