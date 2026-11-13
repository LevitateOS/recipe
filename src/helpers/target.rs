//! Package metadata collection inside the managed domain. Extraction grants
//! apply only to staging; package metadata stays in the target manifest.
use super::rpm_package;
use crate::target::{self, Entry, Manifest};
use anyhow::{Context, Result, bail, ensure};
use rhai::EvalAltResult;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

fn recipe_result(result: Result<()>) -> Result<(), Box<EvalAltResult>> {
    result.map_err(|e| format!("{e:#}").into())
}

fn metadata<R: Read>(entry: &mut tar::Entry<'_, R>) -> Result<Entry> {
    let h = entry.header();
    let kind = h.entry_type();
    let mut result = Entry {
        kind: if kind.is_file() {
            "file"
        } else if kind.is_dir() {
            "dir"
        } else if kind.is_symlink() {
            "symlink"
        } else if kind.is_hard_link() {
            "hardlink"
        } else {
            bail!("unsupported package inode")
        }
        .into(),
        mode: h.mode()? & 0o7777,
        uid: h.uid()?.try_into()?,
        gid: h.gid()?.try_into()?,
        user: h.username()?.filter(|s| !s.is_empty()).map(str::to_owned),
        group: h.groupname()?.filter(|s| !s.is_empty()).map(str::to_owned),
        link: entry.link_name()?.map(|p| p.to_string_lossy().into_owned()),
        xattrs: BTreeMap::new(),
        sha256: None,
    };
    if let Some(pax) = entry.pax_extensions()? {
        for extension in pax {
            let extension = extension?;
            if let Some(name) = extension.key()?.strip_prefix("SCHILY.xattr.") {
                result
                    .xattrs
                    .insert(name.into(), hex::encode(extension.value_bytes()));
            } else if extension.key()?.starts_with("LIBARCHIVE.xattr.") {
                bail!("unsupported base64 package xattr encoding; refusing metadata loss");
            }
        }
    }
    Ok(result)
}

/// Apply one package member through `extraction` and, when `members` is
/// recording, note it as extraction applied it. Shared by the tar and RPM
/// readers so both validate and record identically.
#[allow(clippy::too_many_arguments)]
fn apply_member(
    extraction: &mut target::Extraction,
    manifest: &mut Manifest,
    rel: &Path,
    name: String,
    mut md: Entry,
    extract: bool,
    members: &mut Option<&mut Vec<target::Member>>,
    write: impl FnOnce(&Path, u32) -> Result<()>,
) -> Result<()> {
    target::Extraction::normalize(&mut md)?;
    let recorded = members.as_ref().map(|_| md.clone());
    let (canonical, _) = extraction.member(manifest, rel, md, extract, write)?;
    if let (Some(members), Some(entry)) = (members.as_deref_mut(), recorded) {
        let data = (entry.kind == "file").then(|| canonical.clone());
        members.push(target::Member {
            path: name,
            entry,
            data,
        });
    }
    Ok(())
}

/// Extract (or, without `extract`, only record) a tar stream's members into
/// `root` through [`target::Extraction`]. With `members`, also record each
/// member as extraction applied it ([`target::Member`]).
fn read_tar<R: Read>(
    reader: R,
    root: &Path,
    manifest: &mut Manifest,
    extract: bool,
    mut members: Option<&mut Vec<target::Member>>,
) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    let mut extraction = target::Extraction::new(root);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let rel = target::relative(&entry.path()?)?;
        let name = rel.to_str().context("non-UTF8 package path")?.to_owned();
        // APK signature/control entries are package metadata, not OS files.
        if name.is_empty()
            || name == ".PKGINFO"
            || name.starts_with(".SIGN.")
            || name == ".INSTALL"
            || name == ".trigger"
        {
            continue;
        }
        ensure!(
            name != target::MANIFEST,
            "package uses reserved metadata path"
        );
        let md = metadata(&mut entry)?;
        apply_member(
            &mut extraction,
            manifest,
            &rel,
            name,
            md,
            extract,
            &mut members,
            |dest, mode| {
                let mut file = File::create(dest)?;
                std::io::copy(&mut entry, &mut file)?;
                file.set_permissions(fs::Permissions::from_mode(mode))?;
                Ok(())
            },
        )?;
    }
    extraction.finish()
}

/// The numeric ids for package owner names, from this environment's
/// passwd/group files; names that do not resolve are 0, as RPM treats them.
/// The names themselves are recorded alongside.
struct Owners {
    users: BTreeMap<String, u32>,
    groups: BTreeMap<String, u32>,
}

impl Owners {
    fn load() -> Self {
        let table = |path: &str| -> BTreeMap<String, u32> {
            fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .filter_map(|line| {
                    let mut f = line.split(':');
                    let name = f.next()?;
                    let id = f.nth(1)?.parse().ok()?;
                    Some((name.to_owned(), id))
                })
                .collect()
        };
        Owners {
            users: table("/etc/passwd"),
            groups: table("/etc/group"),
        }
    }
}

/// Extract an RPM payload (a cpio stream) into `root` through
/// [`target::Extraction`], with metadata taken from the package header.
/// Hardlink sets come out as RPM stores them in the payload (content on the
/// last member): the first member of a set is written with the content and the
/// others become hardlinks to it, which is how `rpm2archive` presented them.
fn read_rpm_payload<R: Read + 'static>(
    header: &rpm_package::Header,
    reader: R,
    root: &Path,
    manifest: &mut Manifest,
    mut members: Option<&mut Vec<target::Member>>,
) -> Result<()> {
    let owners = Owners::load();
    let by_path = rpm_package::index(header);
    let mut cpio = rpm_package::Cpio::new(rpm_package::payload(header, reader)?);
    let mut extraction = target::Extraction::new(root);
    let entry_for = |index: usize, kind: &str, link: Option<String>, mode: u32| -> Entry {
        let user = header.users.get(index).filter(|s| !s.is_empty()).cloned();
        let group = header.groups.get(index).filter(|s| !s.is_empty()).cloned();
        Entry {
            kind: kind.into(),
            mode: mode & 0o7777,
            uid: user
                .as_ref()
                .and_then(|u| owners.users.get(u))
                .copied()
                .unwrap_or(0),
            gid: group
                .as_ref()
                .and_then(|g| owners.groups.get(g))
                .copied()
                .unwrap_or(0),
            user,
            group,
            link,
            xattrs: BTreeMap::new(),
            sha256: None,
        }
    };
    // Hardlink sets still waiting for their last member, by (device, inode):
    // (relative path, name, header index, mode) per member seen so far.
    type Set = Vec<(PathBuf, String, usize, u32)>;
    let mut sets: BTreeMap<(u32, u32, u32), Set> = BTreeMap::new();
    while let Some(item) = cpio.next()? {
        let rel = target::relative(Path::new(item.name.trim_start_matches('/')))?;
        let name = rel.to_str().context("non-UTF8 package path")?.to_owned();
        if name.is_empty() {
            continue;
        }
        ensure!(
            name != target::MANIFEST,
            "package uses reserved metadata path"
        );
        let index = *by_path.get(name.as_str()).with_context(|| {
            format!(
                "RPM payload member {name} is not in the package header; \
                 the file is corrupt, fetch it again"
            )
        })?;
        match item.mode & 0o170000 {
            0o040000 => apply_member(
                &mut extraction,
                manifest,
                &rel,
                name,
                entry_for(index, "dir", None, item.mode),
                true,
                &mut members,
                |_, _| Ok(()),
            )?,
            0o120000 => {
                ensure!(item.size <= 4096, "package symlink target too long");
                let mut link = String::new();
                cpio.data()
                    .read_to_string(&mut link)
                    .context("non-UTF8 package symlink")?;
                apply_member(
                    &mut extraction,
                    manifest,
                    &rel,
                    name,
                    entry_for(index, "symlink", Some(link), item.mode),
                    true,
                    &mut members,
                    |_, _| Ok(()),
                )?;
            }
            0o100000 => {
                let mut set = vec![(rel, name, index, item.mode)];
                if item.nlink > 1 {
                    let key = (item.dev.0, item.dev.1, item.ino);
                    let waiting = sets.entry(key).or_default();
                    waiting.extend(set);
                    if item.size == 0 && (waiting.len() as u32) < item.nlink {
                        continue;
                    }
                    set = sets.remove(&key).unwrap_or_default();
                }
                let mut data = cpio.data();
                let (first_rel, first_name, first_index, first_mode) = set[0].clone();
                apply_member(
                    &mut extraction,
                    manifest,
                    &first_rel,
                    first_name.clone(),
                    entry_for(first_index, "file", None, first_mode),
                    true,
                    &mut members,
                    |dest, mode| {
                        let mut file = File::create(dest)?;
                        std::io::copy(&mut data, &mut file)?;
                        file.set_permissions(fs::Permissions::from_mode(mode))?;
                        Ok(())
                    },
                )?;
                for (rel, name, index, mode) in set.into_iter().skip(1) {
                    apply_member(
                        &mut extraction,
                        manifest,
                        &rel,
                        name,
                        entry_for(index, "hardlink", Some(first_name.clone()), mode),
                        true,
                        &mut members,
                        |_, _| Ok(()),
                    )?;
                }
            }
            _ => bail!("unsupported package inode"),
        }
    }
    ensure!(
        sets.is_empty(),
        "RPM payload ends inside a hardlink set; the file is truncated, fetch it again"
    );
    extraction.finish()
}

pub fn extract_rpm(archive: &str, root: &str) -> Result<(), Box<EvalAltResult>> {
    recipe_result(extract_rpm_recorded(archive, root, None))
}

/// `extract_rpm`, and the record of what it did (`target::Members`) written to
/// `members`, outside `root`, so the package can be composed into another
/// root later (`target::replay`).
pub fn extract_rpm_members(
    archive: &str,
    root: &str,
    members: &str,
) -> Result<(), Box<EvalAltResult>> {
    recipe_result(extract_rpm_recorded(
        archive,
        root,
        Some(Path::new(members)),
    ))
}

fn extract_rpm_recorded(archive: &str, root: &str, record: Option<&Path>) -> Result<()> {
    let root = Path::new(root);
    fs::create_dir_all(root)?;
    let mut manifest = target::load(root)?;
    let mut reader = BufReader::new(File::open(archive).with_context(|| {
        format!("cannot open RPM {archive}; check the path the recipe acquired it to")
    })?);
    let header = rpm_package::read_header(&mut reader)?;
    let mut members = record.map(|_| Vec::new());
    read_rpm_payload(&header, reader, root, &mut manifest, members.as_mut())?;
    // File capabilities live in the package header; encode Linux v2 file
    // capabilities as raw target xattr bytes.
    let mut capabilities = Vec::new();
    for (path, caps) in header.files.iter().zip(&header.caps) {
        if caps.is_empty() {
            continue;
        }
        capabilities.push(target::Capability {
            path: path.trim_start_matches('/').to_owned(),
            xattr: hex::encode(file_caps(caps)?),
        });
    }
    target::apply_capabilities(root, &mut manifest, &capabilities)?;
    target::save(root, &manifest)?;
    if let (Some(path), Some(members)) = (record, members) {
        target::save_members(
            path,
            &target::Members {
                version: target::MEMBERS_VERSION,
                dir_mode: created_dir_mode(path)?,
                members,
                capabilities,
            },
        )?;
    }
    Ok(())
}

/// The staging mode a directory this process creates gets: made and removed
/// beside `record`, so the root it describes stays untouched.
fn created_dir_mode(record: &Path) -> Result<u32> {
    let probe = record
        .parent()
        .context("member record has no directory")?
        .join(format!(".recipe-dir-mode-{}", std::process::id()));
    fs::create_dir(&probe)?;
    let mode = fs::symlink_metadata(&probe)?.mode() & 0o7777;
    fs::remove_dir(&probe)?;
    Ok(mode)
}

pub fn extract_tar(archive: &str, root: &str) -> Result<(), Box<EvalAltResult>> {
    recipe_result((|| {
        let root = Path::new(root);
        fs::create_dir_all(root)?;
        let mut manifest = target::load(root)?;
        read_tar(File::open(archive)?, root, &mut manifest, true, None)?;
        target::save(root, &manifest)
    })())
}

fn file_caps(text: &str) -> Result<Vec<u8>> {
    const NAMES: &[&str] = &[
        "chown",
        "dac_override",
        "dac_read_search",
        "fowner",
        "fsetid",
        "kill",
        "setgid",
        "setuid",
        "setpcap",
        "linux_immutable",
        "net_bind_service",
        "net_broadcast",
        "net_admin",
        "net_raw",
        "ipc_lock",
        "ipc_owner",
        "sys_module",
        "sys_rawio",
        "sys_chroot",
        "sys_ptrace",
        "sys_pacct",
        "sys_admin",
        "sys_boot",
        "sys_nice",
        "sys_resource",
        "sys_time",
        "sys_tty_config",
        "mknod",
        "lease",
        "audit_write",
        "audit_control",
        "setfcap",
        "mac_override",
        "mac_admin",
        "syslog",
        "wake_alarm",
        "block_suspend",
        "audit_read",
        "perfmon",
        "bpf",
        "checkpoint_restore",
    ];
    let (mut permitted, mut inheritable, mut effective) = (0u64, 0u64, 0u64);
    for clause in text.split_whitespace() {
        let (names, flags) = clause
            .split_once('=')
            .context("unsupported RPM capability syntax")?;
        let mut mask = 0;
        for name in names.split(',') {
            let index = NAMES
                .iter()
                .position(|n| Some(*n) == name.strip_prefix("cap_"))
                .context("unknown capability name")?;
            mask |= 1u64 << index;
        }
        ensure!(
            flags.chars().all(|c| "eip".contains(c)),
            "unknown capability flag"
        );
        if flags.contains('p') {
            permitted |= mask;
        }
        if flags.contains('i') {
            inheritable |= mask;
        }
        if flags.contains('e') {
            effective |= mask;
        }
    }
    ensure!(
        effective == 0 || effective == permitted | inheritable,
        "file capability effective flags disagree"
    );
    let mut bytes = Vec::new();
    for value in [
        0x02000000u32 | u32::from(effective != 0),
        permitted as u32,
        inheritable as u32,
        (permitted >> 32) as u32,
        (inheritable >> 32) as u32,
    ] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    Ok(bytes)
}

pub fn capture_apks(root: &str, paths: rhai::Array) -> Result<(), Box<EvalAltResult>> {
    recipe_result((|| {
        let root = Path::new(root);
        let database = ["lib/apk/db/installed", "usr/lib/apk/db/installed"]
            .iter()
            .map(|p| root.join(p))
            .find(|p| p.is_file())
            .context("APK installed database missing")?;
        let text = fs::read_to_string(database)?;
        let mut archives = BTreeMap::new();
        for directory in paths {
            for item in walkdir::WalkDir::new(directory.to_string()).follow_links(false) {
                let item = item?;
                if item.file_type().is_file() && item.path().extension().is_some_and(|e| e == "apk")
                {
                    archives.insert(
                        item.file_name().to_string_lossy().into_owned(),
                        item.path().to_owned(),
                    );
                }
            }
        }
        let mut manifest = Manifest::default();
        for record in text.split("\n\n").filter(|s| !s.trim().is_empty()) {
            let field = |key: &str| {
                record
                    .lines()
                    .find_map(|l| l.strip_prefix(key))
                    .context("invalid APK installed record")
            };
            let name = format!("{}-{}.apk", field("P:")?, field("V:")?);
            let archive = archives.get(&name).with_context(|| format!("no pinned package archive for installed {name}; metadata cannot be reconstructed"))?;
            let mut source = BufReader::new(File::open(archive)?);
            while !source.fill_buf()?.is_empty() {
                let mut gz = flate2::bufread::GzDecoder::new(source);
                read_tar(&mut gz, root, &mut manifest, false, None)
                    .with_context(|| format!("reading metadata from {name}"))?;
                std::io::copy(&mut gz, &mut std::io::sink())?;
                source = gz.into_inner();
            }
        }
        target::save(root, &manifest)
    })())
}

pub fn mode(root: &str, path: &str, mode: i64) -> Result<(), Box<EvalAltResult>> {
    recipe_result((|| {
        let mode: u32 = mode.try_into()?;
        let full = target::set_mode(Path::new(root), Path::new(path), mode)?;
        let grant = if full.is_dir() { 0o700 } else { 0o600 };
        fs::set_permissions(full, fs::Permissions::from_mode((mode & 0o777) | grant))?;
        Ok(())
    })())
}

pub fn finalize(root: &str) -> Result<(), Box<EvalAltResult>> {
    recipe_result(target::finalize(Path::new(root)))
}

pub fn remove(root: &str, path: &str) -> Result<(), Box<EvalAltResult>> {
    recipe_result(target::remove(Path::new(root), Path::new(path)))
}

pub fn unpack_tree(archive: &str, root: &str) -> Result<(), Box<EvalAltResult>> {
    recipe_result((|| {
        let root = Path::new(root);
        fs::create_dir_all(root)?;
        let file = File::open(archive).with_context(|| format!("opening {archive}"))?;
        let total = file.metadata()?.len();
        target::unpack_tree(BufReader::new(file), root, &mut |done| {
            crate::core::output::progress_report(done, total, "bytes of the tree")
        })
    })())
}

pub fn pack_tree(root: &str, archive: &str) -> Result<(), Box<EvalAltResult>> {
    recipe_result((|| {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(archive)
            .with_context(|| format!("creating {archive}"))?;
        let mut out = std::io::BufWriter::new(file);
        target::write_tree(Path::new(root), &mut out, true)?;
        out.into_inner()
            .map_err(|e| anyhow::anyhow!("writing {archive}: {e}"))?
            .sync_all()?;
        Ok(())
    })())
}
