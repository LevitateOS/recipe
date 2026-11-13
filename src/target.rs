//! Intended target metadata is data, never a request for build-host privileges.
//!
//! Package producers keep writable/readable staging inodes and carry this manifest
//! through publication and product composition. The image writer consumes the
//! manifest rather than the coordinator's UID/GID, umask or xattrs.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

pub const MANIFEST: &str = ".recipe-target-metadata.json";
const MAX_MANIFEST: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub kind: String,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Raw xattr bytes, hex encoded (including security.capability).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub xattrs: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub entries: BTreeMap<String, Entry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removals: Vec<String>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: 1,
            entries: BTreeMap::new(),
            removals: Vec::new(),
        }
    }
}

pub fn relative(path: &Path) -> Result<PathBuf> {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => (),
            Component::Normal(name) => result.push(name),
            _ => bail!("unsafe target member: {}", path.display()),
        }
    }
    ensure!(result.as_os_str().len() <= 4096, "target member too long");
    Ok(result)
}

/// Resolve target directory links inside the target root. Absolute links reset
/// to this root, never to the compiler/coordinator filesystem. The final inode
/// is not followed, so its own symlink metadata can be represented.
pub fn resolve_member(root: &Path, member: &Path) -> Result<PathBuf> {
    Resolver::new(root).resolve(member)
}

/// `resolve_member` with a memory of root-relative paths already confirmed to
/// be real directories, so bulk extraction does not re-lstat every ancestor of
/// every entry. Callers must never replace a remembered directory with another
/// kind of inode while the resolver is alive.
pub struct Resolver<'a> {
    root: &'a Path,
    dirs: HashSet<PathBuf>,
    /// The staging mode `ensure_dir` gives a directory it creates; `None`
    /// leaves it to the process umask.
    create_mode: Option<u32>,
}

impl<'a> Resolver<'a> {
    pub fn new(root: &'a Path) -> Self {
        Self {
            root,
            dirs: HashSet::new(),
            create_mode: None,
        }
    }

    /// Same semantics as `resolve_member`.
    pub fn resolve(&mut self, member: &Path) -> Result<PathBuf> {
        // Normalize a link lexically first. Repeated links are rejected by a
        // bounded explicit resolver below, avoiding recursive unbounded traversal.
        let mut path = member.to_path_buf();
        for _ in 0..40 {
            let mut relative = PathBuf::new();
            let parts = path.components().collect::<Vec<_>>();
            let mut replaced = false;
            for (i, part) in parts.iter().enumerate() {
                match part {
                    Component::RootDir => relative.clear(),
                    Component::CurDir => (),
                    Component::ParentDir => ensure!(relative.pop(), "target link escapes root"),
                    Component::Normal(name) => relative.push(name),
                    _ => bail!("invalid target path"),
                }
                if i + 1 < parts.len() && !self.dirs.contains(&relative) {
                    let full = self.root.join(&relative);
                    match fs::symlink_metadata(&full) {
                        Ok(m) if m.file_type().is_symlink() => {
                            let link = fs::read_link(&full)?;
                            relative.pop();
                            path = relative
                                .join(link)
                                .join(parts[i + 1..].iter().collect::<PathBuf>());
                            replaced = true;
                            break;
                        }
                        Ok(m) if m.is_dir() => {
                            self.dirs.insert(relative.clone());
                        }
                        _ => (),
                    }
                }
            }
            if !replaced {
                return Ok(self.root.join(relative));
            }
        }
        bail!("target symlink loop")
    }

    /// Where a directory member lands: `resolve`, except that a final
    /// component which is a symlink to a directory inside the root is
    /// followed too. apk, rpm and tar (`--keep-directory-symlink`) all keep
    /// such a symlink (Alpine's `var/run -> ../run`, which avahi ships as a
    /// directory) and give the directory's metadata to its target. Any other
    /// final component is left as `resolve` returns it.
    pub fn resolve_directory(&mut self, member: &Path) -> Result<PathBuf> {
        let resolved = self.resolve(member)?;
        if !fs::symlink_metadata(&resolved).is_ok_and(|m| m.file_type().is_symlink()) {
            return Ok(resolved);
        }
        // Every non-final component is followed, so resolving a child of
        // `member` follows `member` itself, within the root. A link that
        // escapes the root or loops keeps `resolve`'s answer.
        let Ok(child) = self.resolve(&member.join("child")) else {
            return Ok(resolved);
        };
        match child.parent() {
            Some(target) if fs::symlink_metadata(target).is_ok_and(|m| m.is_dir()) => {
                Ok(target.to_path_buf())
            }
            _ => Ok(resolved),
        }
    }

    /// Make `dir` (a resolved path under the root) and its ancestors real
    /// directories, creating missing ones. Returns false when the final
    /// component exists as something other than a directory; an intermediate
    /// non-directory is an error.
    pub fn ensure_dir(&mut self, dir: &Path) -> Result<bool> {
        let rel = dir.strip_prefix(self.root)?.to_path_buf();
        let mut cur = PathBuf::new();
        let count = rel.components().count();
        for (i, part) in rel.components().enumerate() {
            cur.push(part);
            if self.dirs.contains(&cur) {
                continue;
            }
            let full = self.root.join(&cur);
            match fs::symlink_metadata(&full) {
                Ok(m) if m.is_dir() => {}
                Ok(_) if i + 1 == count => return Ok(false),
                Ok(_) => bail!(
                    "target path component is not a directory: {}",
                    cur.display()
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => match fs::create_dir(&full) {
                    Ok(()) => {
                        if let Some(mode) = self.create_mode {
                            fs::set_permissions(&full, fs::Permissions::from_mode(mode))?;
                        }
                    }
                    Err(e) => ensure!(
                        e.kind() == std::io::ErrorKind::AlreadyExists
                            && fs::symlink_metadata(&full).is_ok_and(|m| m.is_dir()),
                        "cannot create {}: {e}",
                        full.display()
                    ),
                },
                Err(e) => return Err(e.into()),
            }
            self.dirs.insert(cur.clone());
        }
        Ok(true)
    }
}

pub fn load(root: &Path) -> Result<Manifest> {
    let path = root.join(MANIFEST);
    if !path.try_exists()? {
        return Ok(Manifest::default());
    }
    ensure!(
        fs::symlink_metadata(&path)?.is_file(),
        "target manifest must be a regular file"
    );
    let mut bytes = Vec::new();
    File::open(&path)?
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_MANIFEST,
        "target metadata exceeds bound"
    );
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    ensure!(
        matches!(manifest.version, 1 | 2),
        "unsupported target metadata version"
    );
    for name in &manifest.removals {
        ensure!(
            !name.is_empty()
                && relative(Path::new(name))? == Path::new(name)
                && !manifest.entries.contains_key(name),
            "invalid target removal"
        );
    }
    for (name, entry) in &manifest.entries {
        ensure!(
            relative(Path::new(name))? == Path::new(name),
            "noncanonical target member"
        );
        ensure!(entry.mode <= 0o7777, "invalid target mode: {name}");
        ensure!(
            ["file", "dir", "symlink", "hardlink"].contains(&entry.kind.as_str()),
            "unsupported target kind: {name}"
        );
        for (name, value) in &entry.xattrs {
            ensure!(
                !name.contains(['\0', '\n', '=']) && !name.is_empty(),
                "invalid target xattr name"
            );
            ensure!(hex::decode(value)?.len() <= 65536, "target xattr too large");
        }
        if entry.kind == "hardlink" {
            relative(Path::new(
                entry.link.as_deref().context("missing hardlink target")?,
            ))?;
        }
    }
    Ok(manifest)
}

pub fn save(root: &Path, manifest: &Manifest) -> Result<()> {
    let bytes = serde_json::to_vec(manifest)?;
    ensure!(
        bytes.len() as u64 <= MAX_MANIFEST,
        "target metadata exceeds bound"
    );
    // This file is ordinary data and survives Ginux's checkpoint/export rules.
    let path = root.join(MANIFEST);
    ensure!(
        !fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()),
        "target manifest is a symlink"
    );
    fs::write(path, bytes)?;
    Ok(())
}

/// A file's SHA-256, read in 1 MiB pieces: inside Ginux each `read` is one
/// supervised round trip, so `io::copy`'s 8 KiB buffer cost 128 of them per
/// MiB of a 3.3 GB tree.
fn hash(path: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => digest.update(&buffer[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(hex::encode(digest.finalize()))
}

fn current(path: &Path) -> Result<Entry> {
    observed(path, &fs::symlink_metadata(path)?)
}

/// The entry `path` has now, from its `lstat` metadata `md`.
fn observed(path: &Path, md: &fs::Metadata) -> Result<Entry> {
    let (kind, link) = if md.is_dir() {
        ("dir", None)
    } else if md.file_type().is_symlink() {
        (
            "symlink",
            Some(
                fs::read_link(path)?
                    .into_os_string()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("non-UTF8 target link"))?,
            ),
        )
    } else if md.is_file() {
        ("file", None)
    } else {
        bail!("unsupported target inode: {}", path.display())
    };
    Ok(Entry {
        kind: kind.into(),
        mode: md.mode() & 0o777,
        uid: 0,
        gid: 0,
        user: None,
        group: None,
        xattrs: BTreeMap::new(),
        link,
        sha256: None,
    })
}

/// Finish a producer after its explicit policy changes. Missing package paths
/// (e.g. RPM ghosts) are omitted; generated paths default to root ownership.
pub fn finalize(root: &Path) -> Result<()> {
    save(root, &snapshot(root, true)?)
}

/// Inspect published ordinary package metadata without modifying its cache.
pub fn inspect(root: &Path) -> Result<Manifest> {
    snapshot(root, false)
}

/// The target's own account file, or None. In a host run it is resolved and
/// opened like any helper path, so a link there cannot reach outside the
/// run's directories.
fn read_account_file(path: &Path) -> Option<String> {
    use crate::helpers::confine;
    if !confine::active() {
        return fs::read_to_string(path).ok();
    }
    let path = confine::derived(path, confine::Access::Read).ok()?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut confine::open_read(&path).ok()?, &mut text).ok()?;
    Some(text)
}

fn snapshot(root: &Path, grant_staging: bool) -> Result<Manifest> {
    let mut manifest = load(root)?;
    let accounts = |file: &str| -> Result<BTreeMap<String, u32>> {
        let mut ids = BTreeMap::from([("root".into(), 0)]);
        if let Some(text) = read_account_file(&root.join(file)) {
            for line in text.lines() {
                let fields = line.split(':').collect::<Vec<_>>();
                if fields.len() >= 3 {
                    ids.insert(fields[0].into(), fields[2].parse()?);
                }
            }
        }
        Ok(ids)
    };
    let users = accounts("etc/passwd")?;
    let groups = accounts("etc/group")?;
    let mut entries = BTreeMap::new();
    let mut inodes = HashMap::<(u64, u64), (String, String)>::new();
    for item in walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
    {
        let item = item?;
        let rel = item
            .path()
            .strip_prefix(root)?
            .to_str()
            .context("non-UTF8 target path")?
            .to_owned();
        if rel == MANIFEST {
            continue;
        }
        // One `lstat` per entry: inside Ginux every call is a supervised
        // round trip, and a desktop tree has some 136,000 entries.
        let md = item.metadata()?;
        let observed = observed(item.path(), &md)?;
        // Installation clients may leave unreadable or non-writable inodes.
        // Grant access only to staging, after observing the authored metadata.
        // Target permissions remain the independent package/recipe entry.
        // Most entries already allow it; only the others are changed.
        if grant_staging && observed.kind != "symlink" {
            let grant = if observed.kind == "dir" { 0o700 } else { 0o600 };
            let staged = md.mode() & 0o777;
            if staged & grant != grant {
                fs::set_permissions(item.path(), fs::Permissions::from_mode(staged | grant))?;
            }
        }

        let mut entry = manifest
            .entries
            .remove(&rel)
            .unwrap_or_else(|| observed.clone());
        if let Some(user) = entry.user.take() {
            entry.uid = *users
                .get(&user)
                .with_context(|| format!("target user {user} for {rel} is missing"))?;
        }
        if let Some(group) = entry.group.take() {
            entry.gid = *groups
                .get(&group)
                .with_context(|| format!("target group {group} for {rel} is missing"))?;
        }
        ensure!(
            entry.kind == observed.kind || (entry.kind == "hardlink" && observed.kind == "file"),
            "target type changed without metadata update: {rel}"
        );
        if observed.kind == "symlink" {
            entry.link = observed.link;
        }
        if observed.kind == "file" {
            // A further name of an inode already seen is a hardlink to the
            // first name, with its digest: its bytes are not read again.
            if let Some((first, digest)) = inodes.get(&(md.dev(), md.ino())) {
                entry.sha256 = Some(digest.clone());
                entry.kind = "hardlink".into();
                entry.link = Some(first.clone());
            } else {
                let digest = hash(item.path())?;
                entry.sha256 = Some(digest.clone());
                entry.kind = "file".into();
                entry.link = None;
                inodes.insert((md.dev(), md.ino()), (rel.clone(), digest));
            }
        }
        entries.insert(rel, entry);
    }
    Ok(Manifest {
        version: if manifest.removals.is_empty() { 1 } else { 2 },
        entries,
        removals: manifest.removals,
    })
}

/// Record a deliberate recipe mode change, independently of staging grants.
pub fn set_mode(root: &Path, path: &Path, mode: u32) -> Result<PathBuf> {
    ensure!(mode <= 0o7777, "invalid target mode");
    let path = relative(path)?;
    let resolved = resolve_member(root, &path)?;
    let rel = resolved.strip_prefix(root)?;
    let mut manifest = load(root)?;
    let name = rel.to_str().context("non-UTF8 target path")?.to_owned();
    let mut entry = manifest
        .entries
        .remove(&name)
        .unwrap_or(current(&root.join(rel))?);
    entry.mode = mode;
    manifest.entries.insert(name, entry);
    save(root, &manifest)?;
    Ok(resolved)
}

/// Copy a published package/product without adopting host ownership or following
/// directory symlinks. Metadata for replaced files is replaced too.
pub fn merge_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    let incoming = load(source)?;
    let mut result = load(destination)?;
    let mut inodes = HashMap::new();
    for item in walkdir::WalkDir::new(source)
        .follow_links(false)
        .min_depth(1)
        .sort_by_file_name()
    {
        let item = item?;
        let rel = item.path().strip_prefix(source)?;
        if rel == Path::new(MANIFEST) {
            continue;
        }
        // A destination ancestor must be a real directory, never a host link.
        for ancestor in rel
            .parent()
            .into_iter()
            .flat_map(Path::ancestors)
            .filter(|p| !p.as_os_str().is_empty())
        {
            ensure!(
                !fs::symlink_metadata(destination.join(ancestor))
                    .is_ok_and(|m| m.file_type().is_symlink()),
                "target merge traverses symlink: {}",
                ancestor.display()
            );
        }
        let dest = destination.join(rel);
        let md = fs::symlink_metadata(item.path())?;
        if md.is_dir() {
            ensure!(
                !fs::symlink_metadata(&dest).is_ok_and(|m| !m.is_dir()),
                "target directory replaces another inode: {}",
                rel.display()
            );
            fs::create_dir_all(&dest)?;
        } else {
            if fs::symlink_metadata(&dest).is_ok() {
                fs::remove_file(&dest)?;
            }
            if md.file_type().is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(item.path())?, &dest)?;
            } else if md.is_file() {
                if let Some(first) = inodes.get(&(md.dev(), md.ino())) {
                    fs::hard_link(first, &dest)?;
                } else {
                    fs::copy(item.path(), &dest)?;
                    inodes.insert((md.dev(), md.ino()), dest.clone());
                }
                fs::set_permissions(
                    &dest,
                    fs::Permissions::from_mode((md.mode() & 0o777) | 0o600),
                )?;
            } else {
                bail!("unsupported package inode: {}", rel.display());
            }
        }
        let name = rel.to_str().context("non-UTF8 target path")?.to_owned();
        let entry = incoming
            .entries
            .get(&name)
            .cloned()
            .unwrap_or(current(item.path())?);
        result.entries.insert(name, entry);
    }
    save(destination, &result)
}

/// Write a root-owned-by-policy tar stream for EROFS. Staging metadata is never
/// installed. A published producer's content hash prevents stale capabilities
/// or setid modes from being attached to a replaced binary.
pub fn write_tar(root: &Path, output: impl Write) -> Result<()> {
    let manifest = load(root)?;
    let mut writer = tar::Builder::new(output);
    let mut files = Vec::new();
    let mut links = Vec::new();
    for item in walkdir::WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
    {
        let item = item?;
        let rel = item
            .path()
            .strip_prefix(root)?
            .to_str()
            .context("non-UTF8 target path")?
            .to_owned();
        let observed = current(item.path())?;
        let entry = manifest
            .entries
            .get(&rel)
            .cloned()
            .unwrap_or(observed.clone());
        ensure!(
            entry.kind == observed.kind || (entry.kind == "hardlink" && observed.kind == "file"),
            "stale target type: {rel}"
        );
        if let Some(expected) = &entry.sha256 {
            ensure!(
                hash(item.path())? == *expected,
                "stale target metadata for changed content: {rel}"
            );
        }
        if entry.kind == "symlink" {
            ensure!(entry.link == observed.link, "stale target symlink: {rel}");
        }
        if entry.kind == "hardlink" {
            links.push((rel, entry));
        } else {
            files.push((rel, entry));
        }
    }
    let mut emitted = BTreeMap::new();
    for (name, entry) in files.into_iter().chain(links) {
        let path = if name.is_empty() { "." } else { &name };
        let mut header = tar::Header::new_gnu();
        header.set_mode(entry.mode);
        header.set_uid(entry.uid.into());
        header.set_gid(entry.gid.into());
        header.set_mtime(0);
        let xattrs = entry
            .xattrs
            .iter()
            .map(|(name, data)| Ok((format!("SCHILY.xattr.{name}"), hex::decode(data)?)))
            .collect::<Result<Vec<_>>>()?;
        if !xattrs.is_empty() {
            writer.append_pax_extensions(xattrs.iter().map(|(k, v)| (k.as_str(), v.as_slice())))?;
        }
        match entry.kind.as_str() {
            "file" => {
                let mut file = File::open(root.join(&name))?;
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(file.metadata()?.len());
                writer.append_data(&mut header, path, &mut file)?;
            }
            "dir" => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                writer.append_data(&mut header, path, std::io::empty())?;
            }
            "symlink" => {
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_size(0);
                writer.append_link(
                    &mut header,
                    path,
                    entry.link.as_ref().context("missing symlink target")?,
                )?;
            }
            "hardlink" => {
                let target = entry.link.as_ref().context("missing hardlink target")?;
                let prior: &Entry = emitted
                    .get(target)
                    .context("hardlink must refer to an earlier regular target")?;
                ensure!(
                    prior.mode == entry.mode
                        && prior.uid == entry.uid
                        && prior.gid == entry.gid
                        && prior.xattrs == entry.xattrs
                        && prior.sha256 == entry.sha256,
                    "hardlink metadata disagrees: {name}"
                );
                header.set_entry_type(tar::EntryType::Link);
                header.set_size(0);
                writer.append_link(&mut header, path, target)?;
            }
            _ => bail!("unsupported target kind"),
        }
        emitted.insert(name, entry);
    }
    writer.finish()?;
    Ok(())
}

/// Declare an owned path absent in the next system selection.
pub fn remove(root: &Path, path: &Path) -> Result<()> {
    let path = relative(path)?;
    ensure!(!path.as_os_str().is_empty(), "cannot remove target root");
    let name = path.to_str().context("non-UTF8 removal")?.to_owned();
    let mut manifest = load(root)?;
    let resolved = resolve_member(root, &path)?;
    if let Ok(meta) = fs::symlink_metadata(&resolved) {
        if meta.is_dir() {
            fs::remove_dir(&resolved)?;
        } else {
            fs::remove_file(&resolved)?;
        }
    }
    manifest.entries.remove(&name);
    if !manifest.removals.contains(&name) {
        manifest.removals.push(name);
    }
    manifest.version = 2;
    save(root, &manifest)
}

/// Archive extraction into a target root, one member at a time: the one place
/// that decides where a member lands (through the root's links, as
/// [`Resolver`] resolves them at that moment), what it replaces (a
/// non-directory is replaced, a directory is merged and takes the member's
/// staging mode), and what the manifest records for it (the member's metadata
/// under its canonical path, a hardlink's target canonical too). Hardlinks
/// are made at [`finish`](Extraction::finish), once every member is in place.
///
/// The package helpers feed it archive entries (`target_extract_rpm`,
/// `target_extract_tar`, `target_capture_apks`); [`replay`] feeds it a
/// package's recorded members, so a tree composed from per-package trees is
/// the tree sequential extraction makes.
pub struct Extraction<'a> {
    root: &'a Path,
    resolver: Resolver<'a>,
    hardlinks: Vec<(PathBuf, String)>,
}

impl<'a> Extraction<'a> {
    pub fn new(root: &'a Path) -> Self {
        Self {
            root,
            resolver: Resolver::new(root),
            hardlinks: Vec::new(),
        }
    }

    /// Parent directories this extraction creates get the staging mode `mode`
    /// rather than the process umask's.
    pub fn with_dir_mode(mut self, mode: u32) -> Self {
        self.resolver.create_mode = Some(mode);
        self
    }

    /// A hardlink member's target as a plain relative archive path, as
    /// extraction records it before resolving it. Idempotent.
    pub fn normalize(entry: &mut Entry) -> Result<()> {
        if entry.kind == "hardlink" {
            entry.link = Some(
                relative(Path::new(
                    entry.link.as_deref().context("missing hardlink target")?,
                ))?
                .to_str()
                .context("non-UTF8 hardlink")?
                .into(),
            );
        }
        Ok(())
    }

    /// Apply the member `rel` with the (normalized) metadata `entry`. With
    /// `extract` the root changes too, and `write(dest, mode)` creates a file
    /// member's bytes at `dest`, with the staging mode `mode`. Records
    /// `entry` in `manifest` and returns the member's canonical path and
    /// whether it replaced a non-directory there.
    pub fn member(
        &mut self,
        manifest: &mut Manifest,
        rel: &Path,
        mut entry: Entry,
        extract: bool,
        write: impl FnOnce(&Path, u32) -> Result<()>,
    ) -> Result<(String, bool)> {
        let name = rel.display();
        let resolved = if entry.kind == "dir" {
            self.resolver.resolve_directory(rel)?
        } else {
            self.resolver.resolve(rel)?
        };
        let mut replaced = false;
        if extract {
            let dest = resolved.clone();
            if let Some(parent) = dest.parent() {
                ensure!(
                    self.resolver.ensure_dir(parent)?,
                    "package parent is not a directory: {name}"
                );
            }
            if entry.kind == "dir" {
                ensure!(
                    self.resolver.ensure_dir(&dest)?,
                    "package directory conflict: {name}"
                );
                fs::set_permissions(
                    &dest,
                    fs::Permissions::from_mode((entry.mode & 0o777) | 0o700),
                )?;
            } else {
                if fs::symlink_metadata(&dest).is_ok() {
                    fs::remove_file(&dest)?;
                    replaced = true;
                }
                match entry.kind.as_str() {
                    "file" => write(&dest, (entry.mode & 0o777) | 0o600)?,
                    "symlink" => {
                        std::os::unix::fs::symlink(
                            entry.link.as_ref().context("missing symlink target")?,
                            &dest,
                        )?;
                    }
                    "hardlink" => self
                        .hardlinks
                        .push((dest, entry.link.clone().context("missing hardlink target")?)),
                    _ => bail!("unsupported package inode"),
                }
            }
        }
        let canonical = resolved
            .strip_prefix(self.root)?
            .to_str()
            .context("non-UTF8 package path")?
            .to_owned();
        if entry.kind == "hardlink" {
            let resolved = self.resolver.resolve(Path::new(
                entry.link.as_deref().context("missing hardlink target")?,
            ))?;
            entry.link = Some(
                resolved
                    .strip_prefix(self.root)?
                    .to_str()
                    .context("non-UTF8 hardlink")?
                    .into(),
            );
        }
        manifest.entries.insert(canonical.clone(), entry);
        Ok((canonical, replaced))
    }

    /// Make the hardlink members, each to what its target resolves to now.
    pub fn finish(mut self) -> Result<()> {
        for (dest, link) in std::mem::take(&mut self.hardlinks) {
            let source = self.resolver.resolve(Path::new(&link))?;
            ensure!(
                fs::symlink_metadata(&source)?.is_file(),
                "hardlink target is not a regular file"
            );
            fs::hard_link(source, dest)?;
        }
        Ok(())
    }
}

/// A file capability of an RPM payload path: the path as the package names it
/// (without its leading `/`) and the raw `security.capability` bytes, hex.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub path: String,
    pub xattr: String,
}

/// Add `capabilities` to the manifest entries of the payload paths they name,
/// as those resolve in `root` now. A capability without a payload entry is an
/// error.
pub fn apply_capabilities(
    root: &Path,
    manifest: &mut Manifest,
    capabilities: &[Capability],
) -> Result<()> {
    for capability in capabilities {
        let path = relative(Path::new(&capability.path))?;
        let resolved = resolve_member(root, &path)?;
        let name = resolved
            .strip_prefix(root)?
            .to_str()
            .context("non-UTF8 RPM path")?;
        if let Some(entry) = manifest.entries.get_mut(name) {
            entry
                .xattrs
                .insert("security.capability".into(), capability.xattr.clone());
        } else {
            bail!("RPM capability has no payload: {name}");
        }
    }
    Ok(())
}

/// The version of the [`Members`] format.
pub const MEMBERS_VERSION: u32 = 1;
const MAX_MEMBERS: u64 = 256 * 1024 * 1024;

/// One archive member as extraction applied it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    /// The archive path, relative, as [`relative`] makes it.
    pub path: String,
    /// The metadata from the archive header ([`Extraction::normalize`]d).
    pub entry: Entry,
    /// A file member: where its bytes are in the package's own root (its
    /// canonical path there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

/// One package's extraction into an empty root of its own, recorded so that
/// [`replay`] can apply the same extraction to another root without the
/// archive: every member in archive order and the file capabilities applied
/// after them. `target_extract_rpm(archive, root, members)` writes it beside
/// the package's root.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Members {
    pub version: u32,
    /// The staging mode a directory got when extraction created it as a
    /// parent (the extracting process's umask applied to 0777).
    pub dir_mode: u32,
    pub members: Vec<Member>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<Capability>,
}

pub fn save_members(path: &Path, members: &Members) -> Result<()> {
    let bytes = serde_json::to_vec(members)?;
    ensure!(
        bytes.len() as u64 <= MAX_MEMBERS,
        "member record exceeds bound"
    );
    fs::write(path, bytes)?;
    Ok(())
}

pub fn load_members(path: &Path) -> Result<Members> {
    ensure!(
        fs::symlink_metadata(path)?.is_file(),
        "member record must be a regular file"
    );
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_MEMBERS + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_MEMBERS,
        "member record exceeds bound"
    );
    let members: Members = serde_json::from_slice(&bytes)?;
    ensure!(
        members.version == MEMBERS_VERSION,
        "unsupported member record version"
    );
    ensure!(members.dir_mode <= 0o7777, "invalid member directory mode");
    Ok(members)
}

/// Where [`replay`] put one non-directory member.
#[derive(Clone, Debug)]
pub struct Placed {
    /// Its canonical path in the root.
    pub path: String,
    /// Its metadata as the manifest records it (before capabilities).
    pub entry: Entry,
    /// A file: the package-root file its bytes came from.
    pub source: Option<PathBuf>,
    /// Whether it replaced a non-directory already there.
    pub replaced: bool,
}

/// Apply a package to `root` as extracting its archive into `root` would: the
/// recorded members in archive order through [`Extraction`] (with the
/// package's own directory mode for created parents), its hardlinks, then its
/// file capabilities, all resolved against `root` as it is at that point.
/// Each file's bytes come from `package_root` (its record's `data`): a
/// hardlink to that file when `link` is set and its staging mode is the one
/// extraction gives it, else a copy with that mode. `manifest` is `root`'s,
/// updated in memory; the caller saves it. Returns the non-directory members
/// in order.
pub fn replay(
    package_root: &Path,
    members: &Members,
    root: &Path,
    manifest: &mut Manifest,
    link: bool,
) -> Result<Vec<Placed>> {
    let mut extraction = Extraction::new(root).with_dir_mode(members.dir_mode);
    let mut placed = Vec::new();
    for member in &members.members {
        let rel = relative(Path::new(&member.path))?;
        ensure!(
            rel == Path::new(&member.path) && !member.path.is_empty(),
            "noncanonical package member: {}",
            member.path
        );
        ensure!(
            member.path != MANIFEST,
            "package uses reserved metadata path"
        );
        let mut entry = member.entry.clone();
        Extraction::normalize(&mut entry)?;
        let source = match entry.kind.as_str() {
            "file" => {
                let data = member
                    .data
                    .as_deref()
                    .with_context(|| format!("file member without data: {}", member.path))?;
                let source = package_root.join(relative(Path::new(data))?);
                ensure!(
                    fs::symlink_metadata(&source).is_ok_and(|m| m.is_file()),
                    "package tree lacks the file {data}"
                );
                Some(source)
            }
            _ => None,
        };
        let recorded = entry.clone();
        let (path, replaced) = extraction.member(manifest, &rel, entry, true, |dest, mode| {
            place_file(source.as_deref().expect("file source"), dest, mode, link)
        })?;
        if recorded.kind != "dir" {
            placed.push(Placed {
                entry: manifest.entries.get(&path).cloned().unwrap_or(recorded),
                path,
                source,
                replaced,
            });
        }
    }
    extraction.finish()?;
    apply_capabilities(root, manifest, &members.capabilities)?;
    Ok(placed)
}

/// `source`'s bytes at `dest` with the staging mode `mode`: a hardlink when
/// allowed and the modes agree (never across filesystems), else a copy.
fn place_file(source: &Path, dest: &Path, mode: u32, link: bool) -> Result<()> {
    if link && fs::symlink_metadata(source)?.mode() & 0o7777 == mode {
        match fs::hard_link(source, dest) {
            Ok(()) => return Ok(()),
            Err(e) if matches!(e.raw_os_error(), Some(libc::EXDEV | libc::EMLINK)) => {}
            Err(e) => return Err(e).with_context(|| format!("linking {}", dest.display())),
        }
    }
    fs::copy(source, dest).with_context(|| format!("copying {}", source.display()))?;
    fs::set_permissions(dest, fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// Write the staging tree under `root` as a tar stream [`unpack_tree`] turns
/// back into the same tree: every entry below `root` in name order with its
/// kind, staging mode (all 12 bits), modification time and link target; a
/// file whose inode an earlier entry has, as a hardlink to that entry; the
/// target metadata manifest as the plain file it is. `root` itself and
/// ownership are not recorded. With `consume`, each entry is removed once it
/// is written, and `root` at the end, so the tree and its archive never take
/// the room of two trees (a Ginux domain keeps its files in memory).
pub fn write_tree(root: &Path, output: impl Write, consume: bool) -> Result<()> {
    let mut writer = tar::Builder::new(output);
    writer.follow_symlinks(false);
    let mut inodes = HashMap::<(u64, u64), String>::new();
    let mut dirs = Vec::new();
    for item in walkdir::WalkDir::new(root)
        .follow_links(false)
        .min_depth(1)
        .sort_by_file_name()
    {
        let item = item?;
        let name = item
            .path()
            .strip_prefix(root)?
            .to_str()
            .context("non-UTF8 tree path")?
            .to_owned();
        let md = fs::symlink_metadata(item.path())?;
        let mut header = tar::Header::new_gnu();
        header.set_mode(md.mode() & 0o7777);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(md.mtime().max(0) as u64);
        if md.is_dir() {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            writer.append_data(&mut header, &name, std::io::empty())?;
            if consume {
                // Its entries are removed next; its mode is in the archive.
                if md.mode() & 0o700 != 0o700 {
                    fs::set_permissions(
                        item.path(),
                        fs::Permissions::from_mode(md.mode() | 0o700),
                    )?;
                }
                dirs.push(item.path().to_path_buf());
            }
            continue;
        } else if md.file_type().is_symlink() {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            writer.append_link(&mut header, &name, fs::read_link(item.path())?)?;
        } else if md.is_file() {
            if let Some(first) = inodes.get(&(md.dev(), md.ino())) {
                header.set_entry_type(tar::EntryType::Link);
                header.set_size(0);
                writer.append_link(&mut header, &name, first)?;
            } else {
                // A file its owner cannot read (a setuid helper with mode 4111,
                // say) is readable once its mode is in the header; only a tree
                // that is consumed is changed so.
                if consume && md.mode() & 0o400 == 0 {
                    fs::set_permissions(
                        item.path(),
                        fs::Permissions::from_mode(md.mode() | 0o400),
                    )?;
                }
                let mut file = File::open(item.path())
                    .with_context(|| format!("reading {}", item.path().display()))?;
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(file.metadata()?.len());
                writer.append_data(&mut header, &name, &mut file)?;
                inodes.insert((md.dev(), md.ino()), name);
            }
        } else {
            bail!("unsupported tree inode: {}", item.path().display());
        }
        if consume {
            fs::remove_file(item.path())?;
        }
    }
    writer.into_inner()?.flush()?;
    if consume {
        for dir in dirs.iter().rev() {
            fs::remove_dir(dir)?;
        }
        fs::remove_dir(root)?;
    }
    Ok(())
}

/// Reads through an inner reader, counting the bytes.
struct Counted<'c, R> {
    inner: R,
    count: &'c std::cell::Cell<u64>,
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.set(self.count.get() + n as u64);
        Ok(n)
    }
}

/// Make the tree [`write_tree`] wrote under `root`, which must be an empty
/// directory: the same paths, kinds, staging modes, modification times, link
/// targets, hardlinks and target metadata manifest. `root`'s own mode is left
/// as it is. Every member's parent must be a directory an earlier member made,
/// so nothing is written through a link, and every name appears once.
/// `progress` gets the bytes read so far.
///
/// Inside a Ginux domain whose owned storage offers archive import, the tree
/// goes to the supervisor as one tar stream ([`import_tree`]) instead of about
/// five mediated calls per member; elsewhere it is made member by member
/// ([`extract_tree`]). Both leave the same tree.
pub fn unpack_tree(input: impl Read, root: &Path, progress: &mut dyn FnMut(u64)) -> Result<()> {
    ensure!(
        fs::symlink_metadata(root).is_ok_and(|m| m.is_dir()),
        "tree root is not a directory: {}",
        root.display()
    );
    ensure!(
        fs::read_dir(root)?.next().is_none(),
        "tree root is not empty: {}",
        root.display()
    );
    match open_import(root)? {
        Some(stream) => import_tree(input, root, stream, progress)?,
        None => extract_tree(input, root, progress)?,
    }
    // The manifest the tree carried must be one Recipe reads.
    load(root)?;
    Ok(())
}

fn tree_time(seconds: u64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TreeKind {
    Dir,
    File,
    Symlink,
}

/// One member of a [`write_tree`] archive, checked by [`TreeCheck`].
struct TreeMember {
    rel: PathBuf,
    kind: tar::EntryType,
    mode: u32,
    mtime: u64,
    /// A hardlink's root-relative target, or a symlink's target as written.
    link: Option<PathBuf>,
}

/// What [`unpack_tree`] requires of its members, whichever way it applies
/// them: a name below the root, once; a parent an earlier directory member
/// made; a hardlink target an earlier file member made.
struct TreeCheck {
    kinds: HashMap<PathBuf, TreeKind>,
}

impl TreeCheck {
    fn new() -> Self {
        Self {
            kinds: HashMap::from([(PathBuf::new(), TreeKind::Dir)]),
        }
    }

    fn admit<R: Read>(&mut self, entry: &tar::Entry<'_, R>) -> Result<TreeMember> {
        let rel = relative(&entry.path()?)?;
        ensure!(!rel.as_os_str().is_empty(), "tree member names the root");
        let parent = rel.parent().unwrap_or(Path::new(""));
        ensure!(
            self.kinds.get(parent) == Some(&TreeKind::Dir),
            "tree member before its directory: {}",
            rel.display()
        );
        ensure!(
            !self.kinds.contains_key(&rel),
            "tree member appears twice: {}",
            rel.display()
        );
        let header = entry.header();
        let kind = header.entry_type();
        let (tree_kind, link) = match kind {
            tar::EntryType::Directory => (TreeKind::Dir, None),
            tar::EntryType::Regular => (TreeKind::File, None),
            tar::EntryType::Link => {
                let target = relative(&entry.link_name()?.context("hardlink without target")?)?;
                ensure!(
                    self.kinds.get(&target) == Some(&TreeKind::File),
                    "tree hardlink before its file: {}",
                    rel.display()
                );
                (TreeKind::File, Some(target))
            }
            tar::EntryType::Symlink => (
                TreeKind::Symlink,
                Some(
                    entry
                        .link_name()?
                        .context("symlink without target")?
                        .into_owned(),
                ),
            ),
            other => bail!("unsupported tree member {:?}: {}", other, rel.display()),
        };
        let member = TreeMember {
            rel,
            kind,
            mode: header.mode()? & 0o7777,
            mtime: header.mtime()?,
            link,
        };
        self.kinds.insert(member.rel.clone(), tree_kind);
        Ok(member)
    }
}

/// Give the symbolic link `dest` the modification time `mtime`, not following it.
fn set_link_time(dest: &Path, mtime: u64) -> Result<()> {
    let path = std::ffi::CString::new(dest.as_os_str().as_encoded_bytes())?;
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
        libc::timespec {
            tv_sec: mtime.try_into().context("invalid symlink time")?,
            tv_nsec: 0,
        },
    ];
    // SAFETY: the path is NUL-terminated and `times` has two entries.
    if unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            path.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("setting the time of {}", dest.display()));
    }
    Ok(())
}

/// [`unpack_tree`] member by member, with ordinary calls.
fn extract_tree(input: impl Read, root: &Path, progress: &mut dyn FnMut(u64)) -> Result<()> {
    let count = std::cell::Cell::new(0);
    let mut archive = tar::Archive::new(Counted {
        inner: input,
        count: &count,
    });
    let mut check = TreeCheck::new();
    let mut modes = Vec::<(PathBuf, u32, u64)>::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let member = check.admit(&entry)?;
        let dest = root.join(&member.rel);
        let (mode, mtime) = (member.mode, member.mtime);
        match member.kind {
            tar::EntryType::Directory => {
                fs::create_dir(&dest)?;
                fs::set_permissions(&dest, fs::Permissions::from_mode(0o700))?;
                modes.push((dest, mode, mtime));
            }
            tar::EntryType::Regular => {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&dest)?;
                std::io::copy(&mut entry, &mut file)?;
                file.set_permissions(fs::Permissions::from_mode(mode))?;
                file.set_modified(tree_time(mtime))?;
            }
            tar::EntryType::Link => {
                let target = member.link.as_deref().expect("checked hardlink");
                fs::hard_link(root.join(target), &dest)?;
            }
            _ => {
                let target = member.link.as_deref().expect("checked symlink");
                std::os::unix::fs::symlink(target, &dest)?;
                set_link_time(&dest, mtime)?;
            }
        }
        progress(count.get());
    }
    // Directory times and modes after every child is in place.
    for (dest, mode, mtime) in modes.iter().rev() {
        File::open(dest)?.set_modified(tree_time(*mtime))?;
        fs::set_permissions(dest, fs::Permissions::from_mode(*mode))?;
    }
    progress(count.get());
    Ok(())
}

/// `_IOW('G', 1, struct ginux_archive_import)`: open an archive import stream
/// on an owned Ginux directory (Ginux `docs/archive-import.md`).
const GINUX_ARCHIVE_IMPORT: u32 = 0x4010_4701;
/// Clear set-user-ID, set-group-ID and sticky bits instead of refusing them.
const GINUX_ARCHIVE_CLEAR_SPECIAL_MODES: u32 = 2;
/// The most one `write` to an import stream applies.
const IMPORT_CHUNK: usize = 1 << 20;
/// The longest link target an import stream's GNU long-link record carries
/// (its 256-byte bound includes the terminating NUL). An individual
/// `symlinkat` accepts one byte more.
const IMPORT_MAX_LINK: usize = 255;

#[repr(C)]
struct ArchiveImportRequest {
    flags: u32,
    reserved: [u32; 3],
}

/// An archive import stream onto `root`, or `None` where none is offered:
/// Ginux answers `ENOTTY` when `[storage] archive_import` is off, on storage
/// it does not own, and in older versions, as Linux does for a directory.
fn open_import(root: &Path) -> Result<Option<File>> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::OpenOptionsExt;
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(root)
        .with_context(|| format!("opening {}", root.display()))?;
    let request = ArchiveImportRequest {
        flags: GINUX_ARCHIVE_CLEAR_SPECIAL_MODES,
        reserved: [0; 3],
    };
    // SAFETY: the request is a live, properly laid out struct the call only reads.
    let fd = unsafe {
        libc::ioctl(
            directory.as_raw_fd(),
            GINUX_ARCHIVE_IMPORT as _,
            &request as *const ArchiveImportRequest,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ENOTTY | libc::EINVAL) => Ok(None),
            _ => Err(error)
                .with_context(|| format!("requesting an archive import onto {}", root.display())),
        };
    }
    // SAFETY: a successful request returns a new descriptor this process owns.
    Ok(Some(unsafe { File::from_raw_fd(fd) }))
}

/// What a member must look like once the import has applied it, to name the
/// member a failed write stopped at.
enum Applied {
    Dir { mtime: u64 },
    File { mode: u32, size: u64, mtime: u64 },
    Link { target: PathBuf },
    Symlink { mtime: u64 },
}

/// A member sent to an import stream that may not be applied yet.
struct Sent {
    start: u64,
    end: u64,
    rel: PathBuf,
    applied: Applied,
}

/// An import stream with a write buffer of at most [`IMPORT_CHUNK`], so the
/// supervisor gets one write per MiB; it remembers the members of the bytes
/// not yet accepted.
struct ImportStream {
    file: File,
    buffer: Vec<u8>,
    /// Bytes the stream has accepted.
    sent: u64,
    /// Where the write that failed began.
    failed: Option<u64>,
    stopped: bool,
    pending: std::collections::VecDeque<Sent>,
}

impl ImportStream {
    fn new(file: File) -> Self {
        Self {
            file,
            buffer: Vec::with_capacity(IMPORT_CHUNK),
            sent: 0,
            failed: None,
            stopped: false,
            pending: Default::default(),
        }
    }

    /// Bytes handed to this writer so far.
    fn offset(&self) -> u64 {
        self.sent + self.buffer.len() as u64
    }

    fn send(&mut self) -> std::io::Result<()> {
        let mut done = 0;
        while done < self.buffer.len() {
            match self.file.write(&self.buffer[done..]) {
                Ok(0) => {
                    self.failed = Some(self.sent);
                    return Err(std::io::ErrorKind::WriteZero.into());
                }
                Ok(n) => {
                    done += n;
                    self.sent += n as u64;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.failed = Some(self.sent);
                    return Err(e);
                }
            }
        }
        self.buffer.clear();
        while self.pending.front().is_some_and(|m| m.end <= self.sent) {
            self.pending.pop_front();
        }
        Ok(())
    }
}

impl Write for ImportStream {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.stopped || self.failed.is_some() {
            return Err(std::io::Error::other("the archive import has stopped"));
        }
        if self.buffer.len() == IMPORT_CHUNK {
            self.send()?;
        }
        let n = data.len().min(IMPORT_CHUNK - self.buffer.len());
        self.buffer.extend_from_slice(&data[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.stopped || self.failed.is_some() {
            return Err(std::io::Error::other("the archive import has stopped"));
        }
        self.send()
    }
}

/// The error of an import that stopped: the stream's errno and, when a write
/// to it failed, the first member that write carried which is not in place.
/// `error` is the stream's own error, or one reading the input or encoding
/// a member before any write failed.
fn import_stopped(stream: &mut ImportStream, root: &Path, error: std::io::Error) -> anyhow::Error {
    stream.stopped = true;
    let Some(start) = stream.failed else {
        return anyhow::Error::new(error).context(format!(
            "archive import into {} stopped reading or encoding a member",
            root.display()
        ));
    };
    let in_place = |sent: &Sent| -> bool {
        let dest = root.join(&sent.rel);
        let Ok(md) = fs::symlink_metadata(&dest) else {
            return false;
        };
        match &sent.applied {
            Applied::Dir { mtime } => md.is_dir() && md.mtime() == *mtime as i64,
            Applied::File { mode, size, mtime } => {
                md.is_file()
                    && md.mode() & 0o7777 == *mode
                    && md.len() == *size
                    && md.mtime() == *mtime as i64
            }
            Applied::Link { target } => fs::symlink_metadata(root.join(target))
                .is_ok_and(|t| (t.dev(), t.ino()) == (md.dev(), md.ino())),
            Applied::Symlink { mtime } => {
                md.file_type().is_symlink() && md.mtime() == *mtime as i64
            }
        }
    };
    let candidates = stream
        .pending
        .iter()
        .filter(|sent| sent.end > start)
        .collect::<Vec<_>>();
    let at = match candidates.iter().find(|sent| !in_place(sent)) {
        Some(sent) => format!("at member {}", sent.rel.display()),
        None => match (candidates.first(), candidates.last()) {
            (Some(first), Some(last)) => format!(
                "at one of the members {} to {}",
                first.rel.display(),
                last.rel.display()
            ),
            _ => "at the end of the archive".to_owned(),
        },
    };
    anyhow::Error::new(error).context(format!(
        "archive import into {} stopped {at} (stream byte {start})",
        root.display()
    ))
}

/// A directory member held back until its contents are in the stream.
struct OpenDir {
    rel: PathBuf,
    mode: u32,
    mtime: u64,
}

/// What [`import_members`] leaves for ordinary calls after the stream.
#[derive(Default)]
struct AfterImport {
    /// Directories already in the stream, with their recorded times.
    closed: HashMap<PathBuf, u64>,
    /// Directories whose time changed after their member was applied.
    retime: HashSet<PathBuf>,
    /// Symbolic links too long for the stream: name, target, time.
    deferred: Vec<(PathBuf, PathBuf, u64)>,
    /// Recorded modes the stream did not give, children before directories.
    modes: Vec<(PathBuf, u32)>,
}

/// Write one member to the import stream; `applied` says what it leaves.
fn send_member(
    out: &mut tar::Builder<ImportStream>,
    root: &Path,
    rel: &Path,
    header: &mut tar::Header,
    link: Option<&Path>,
    data: &mut dyn Read,
    applied: Applied,
) -> Result<()> {
    let start = out.get_ref().offset();
    out.get_mut().pending.push_back(Sent {
        start,
        end: u64::MAX,
        rel: rel.to_path_buf(),
        applied,
    });
    header.set_uid(0);
    header.set_gid(0);
    let written = match link {
        Some(link) => out.append_link(header, rel, link),
        None => out.append_data(header, rel, data),
    };
    if let Err(error) = written {
        return Err(import_stopped(out.get_mut(), root, error));
    }
    let end = out.get_ref().offset();
    let stream = out.get_mut();
    if let Some(sent) = stream.pending.back_mut().filter(|s| s.start == start) {
        sent.end = end;
    }
    // A send during the append may already have delivered all of it.
    while stream.pending.front().is_some_and(|m| m.end <= stream.sent) {
        stream.pending.pop_front();
    }
    Ok(())
}

/// Write a held directory member, now that its contents are in the stream.
fn close_dir(
    out: &mut tar::Builder<ImportStream>,
    root: &Path,
    dir: OpenDir,
    after: &mut AfterImport,
) -> Result<()> {
    // The owner's rwx while the import runs, as `extract_tree` gives.
    let stream_mode = (dir.mode & 0o777) | 0o700;
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_mode(stream_mode);
    header.set_mtime(dir.mtime);
    header.set_size(0);
    let applied = Applied::Dir { mtime: dir.mtime };
    send_member(
        out,
        root,
        &dir.rel,
        &mut header,
        None,
        &mut std::io::empty(),
        applied,
    )?;
    if stream_mode != dir.mode {
        after.modes.push((root.join(&dir.rel), dir.mode));
    }
    after.closed.insert(dir.rel, dir.mtime);
    Ok(())
}

/// [`unpack_tree`] through an archive import `stream`.
///
/// The members are written again as a canonical tar stream: the same order,
/// except that each directory follows its contents, so creating them does not
/// change its time afterwards. The stream gives files and directories
/// permission bits only, directories with the owner's `rwx` while the import
/// runs, as [`extract_tree`] does. Afterwards, with ordinary calls: symbolic
/// links whose target is too long for the stream are made, the times of
/// directories that changed since their member are set again, and every
/// recorded mode the stream did not give is applied, children before their
/// directory. Any failure is a hard error; members already applied remain.
fn import_tree(
    input: impl Read,
    root: &Path,
    stream: File,
    progress: &mut dyn FnMut(u64),
) -> Result<()> {
    let count = std::cell::Cell::new(0);
    let archive = tar::Archive::new(Counted {
        inner: input,
        count: &count,
    });
    let mut out = tar::Builder::new(ImportStream::new(stream));
    let after = match import_members(archive, root, &mut out, &count, progress) {
        Ok(after) => after,
        Err(error) => {
            // Dropping the builder would end the archive; nothing more goes.
            out.get_mut().stopped = true;
            return Err(error);
        }
    };
    // The end-of-archive marker, then everything buffered, then the verdict.
    if let Err(error) = out.finish() {
        return Err(import_stopped(out.get_mut(), root, error));
    }
    let mut stream = out.into_inner()?;
    if let Err(error) = stream.flush() {
        return Err(import_stopped(&mut stream, root, error));
    }
    if let Err(error) = stream.file.sync_all() {
        stream.failed.get_or_insert(stream.sent);
        return Err(import_stopped(&mut stream, root, error));
    }
    drop(stream);

    let mut retime = after.retime;
    for (rel, target, mtime) in &after.deferred {
        let dest = root.join(rel);
        std::os::unix::fs::symlink(target, &dest)
            .with_context(|| format!("creating {}", dest.display()))?;
        set_link_time(&dest, *mtime)?;
        retime.insert(rel.parent().unwrap_or(Path::new("")).to_path_buf());
    }
    for rel in &retime {
        // The root's own time is not part of the tree.
        if let Some(mtime) = after.closed.get(rel) {
            File::open(root.join(rel))?.set_modified(tree_time(*mtime))?;
        }
    }
    for (dest, mode) in &after.modes {
        fs::set_permissions(dest, fs::Permissions::from_mode(*mode))
            .with_context(|| format!("setting the mode of {}", dest.display()))?;
    }
    progress(count.get());
    Ok(())
}

/// Write every member of `archive` to `out`, directories after their contents.
fn import_members<R: Read>(
    mut archive: tar::Archive<R>,
    root: &Path,
    out: &mut tar::Builder<ImportStream>,
    count: &std::cell::Cell<u64>,
    progress: &mut dyn FnMut(u64),
) -> Result<AfterImport> {
    let mut check = TreeCheck::new();
    let mut open = Vec::<OpenDir>::new();
    let mut after = AfterImport::default();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let member = check.admit(&entry)?;
        let parent = member.rel.parent().unwrap_or(Path::new("")).to_path_buf();
        // Every held directory this member is not inside is complete.
        while open.last().is_some_and(|dir| !parent.starts_with(&dir.rel)) {
            let dir = open.pop().expect("checked");
            close_dir(out, root, dir, &mut after)?;
        }
        // Only an archive out of `write_tree`'s order reopens a directory.
        if after.closed.contains_key(&parent) {
            after.retime.insert(parent.clone());
        }
        let (rel, mode, mtime) = (member.rel, member.mode, member.mtime);
        let mut header = tar::Header::new_gnu();
        header.set_mtime(mtime);
        header.set_size(0);
        let mut empty = std::io::empty();
        match member.kind {
            tar::EntryType::Directory => open.push(OpenDir { rel, mode, mtime }),
            tar::EntryType::Regular => {
                let size = entry.header().size()?;
                header.set_entry_type(tar::EntryType::Regular);
                header.set_mode(mode & 0o777);
                header.set_size(size);
                let applied = Applied::File {
                    mode: mode & 0o777,
                    size,
                    mtime,
                };
                send_member(out, root, &rel, &mut header, None, &mut entry, applied)?;
                if mode & !0o777 != 0 {
                    after.modes.push((root.join(&rel), mode));
                }
            }
            tar::EntryType::Link => {
                let target = member.link.expect("checked hardlink");
                header.set_entry_type(tar::EntryType::Link);
                header.set_mode(mode & 0o777);
                let applied = Applied::Link {
                    target: target.clone(),
                };
                send_member(
                    out,
                    root,
                    &rel,
                    &mut header,
                    Some(&target),
                    &mut empty,
                    applied,
                )?;
            }
            _ => {
                let target = member.link.expect("checked symlink");
                if target.as_os_str().len() > IMPORT_MAX_LINK {
                    after.deferred.push((rel, target, mtime));
                } else {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_mode(0o777);
                    let applied = Applied::Symlink { mtime };
                    send_member(
                        out,
                        root,
                        &rel,
                        &mut header,
                        Some(&target),
                        &mut empty,
                        applied,
                    )?;
                }
            }
        }
        progress(count.get());
    }
    while let Some(dir) = open.pop() {
        close_dir(out, root, dir, &mut after)?;
    }
    Ok(after)
}
