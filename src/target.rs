//! Intended target metadata is data, never a request for build-host privileges.
//!
//! Package producers keep writable/readable staging inodes and carry this manifest
//! through publication and product composition. The image writer consumes the
//! manifest rather than the coordinator's UID/GID, umask or xattrs.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
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
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: 1,
            entries: BTreeMap::new(),
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
            if i + 1 < parts.len()
                && fs::symlink_metadata(root.join(&relative))
                    .is_ok_and(|m| m.file_type().is_symlink())
            {
                let link = fs::read_link(root.join(&relative))?;
                relative.pop();
                path = relative
                    .join(link)
                    .join(parts[i + 1..].iter().collect::<PathBuf>());
                replaced = true;
                break;
            }
        }
        if !replaced {
            return Ok(root.join(relative));
        }
    }
    bail!("target symlink loop")
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
    ensure!(manifest.version == 1, "unsupported target metadata version");
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

fn hash(path: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    std::io::copy(&mut File::open(path)?, &mut digest)?;
    Ok(hex::encode(digest.finalize()))
}

fn current(path: &Path) -> Result<Entry> {
    let md = fs::symlink_metadata(path)?;
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
    let mut manifest = load(root)?;
    let accounts = |file: &str| -> Result<BTreeMap<String, u32>> {
        let mut ids = BTreeMap::from([("root".into(), 0)]);
        if let Ok(text) = fs::read_to_string(root.join(file)) {
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
    let mut inodes = HashMap::<(u64, u64), String>::new();
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
        let observed = current(item.path())?;
        // Installation clients may leave unreadable or non-writable inodes.
        // Grant access only to staging, after observing the authored metadata.
        // Target permissions remain the independent package/recipe entry.
        if observed.kind != "symlink" {
            let grant = if observed.kind == "dir" { 0o700 } else { 0o600 };
            let staged = fs::metadata(item.path())?.mode() & 0o777;
            fs::set_permissions(item.path(), fs::Permissions::from_mode(staged | grant))?;
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
            entry.sha256 = Some(hash(item.path())?);
            let md = fs::symlink_metadata(item.path())?;
            if let Some(first) = inodes.get(&(md.dev(), md.ino())) {
                entry.kind = "hardlink".into();
                entry.link = Some(first.clone());
            } else {
                entry.kind = "file".into();
                entry.link = None;
                inodes.insert((md.dev(), md.ino()), rel.clone());
            }
        }
        entries.insert(rel, entry);
    }
    save(
        root,
        &Manifest {
            entries,
            ..Manifest::default()
        },
    )
}

/// Record a deliberate recipe mode change, independently of staging grants.
pub fn set_mode(root: &Path, path: &Path, mode: u32) -> Result<()> {
    ensure!(mode <= 0o7777, "invalid target mode");
    let path = relative(path)?;
    let resolved = resolve_member(root, &path)?;
    let rel = resolved.strip_prefix(root)?;
    let mut manifest = load(root)?;
    let name = rel.to_str().context("non-UTF8 target path")?.to_owned();
    let mut entry = manifest
        .entries
        .remove(&name)
        .unwrap_or(current(&root.join(&rel))?);
    entry.mode = mode;
    manifest.entries.insert(name, entry);
    save(root, &manifest)
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
