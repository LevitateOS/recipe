//! Package metadata collection inside the managed domain. Extraction grants
//! apply only to staging; package metadata stays in the target manifest.
use crate::target::{self, Entry, Manifest};
use anyhow::{Context, Result, bail, ensure};
use rhai::EvalAltResult;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Cursor, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

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

fn read_tar<R: Read>(reader: R, root: &Path, manifest: &mut Manifest, extract: bool) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    let mut hardlinks = Vec::new();
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
        let mut md = metadata(&mut entry)?;
        if md.kind == "hardlink" {
            md.link = Some(
                target::relative(Path::new(
                    md.link.as_deref().context("missing hardlink target")?,
                ))?
                .to_str()
                .context("non-UTF8 hardlink")?
                .into(),
            );
        }
        if extract {
            let dest = target::resolve_member(root, &rel)?;
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            if md.kind == "dir" {
                ensure!(
                    !fs::symlink_metadata(&dest).is_ok_and(|m| !m.is_dir()),
                    "package directory conflict: {name}"
                );
                fs::create_dir_all(&dest)?;
                fs::set_permissions(&dest, fs::Permissions::from_mode((md.mode & 0o777) | 0o700))?;
            } else {
                if fs::symlink_metadata(&dest).is_ok() {
                    fs::remove_file(&dest)?;
                }
                match md.kind.as_str() {
                    "file" => {
                        std::io::copy(&mut entry, &mut File::create(&dest)?)?;
                        fs::set_permissions(
                            &dest,
                            fs::Permissions::from_mode((md.mode & 0o777) | 0o600),
                        )?;
                    }
                    "symlink" => {
                        std::os::unix::fs::symlink(
                            md.link.as_ref().context("missing symlink target")?,
                            &dest,
                        )?;
                    }
                    "hardlink" => hardlinks.push((dest, md.link.clone().unwrap())),
                    _ => bail!("unsupported package inode"),
                }
            }
        }
        let resolved = target::resolve_member(root, &rel)?;
        let canonical = resolved
            .strip_prefix(root)?
            .to_str()
            .context("non-UTF8 package path")?
            .to_owned();
        if md.kind == "hardlink" {
            let resolved = target::resolve_member(root, Path::new(md.link.as_deref().unwrap()))?;
            md.link = Some(
                resolved
                    .strip_prefix(root)?
                    .to_str()
                    .context("non-UTF8 hardlink")?
                    .into(),
            );
        }
        manifest.entries.insert(canonical, md);
    }
    for (dest, link) in hardlinks {
        let source = target::resolve_member(root, Path::new(&link))?;
        ensure!(
            fs::symlink_metadata(&source)?.is_file(),
            "hardlink target is not a regular file"
        );
        fs::hard_link(source, dest)?;
    }
    Ok(())
}

pub fn extract_rpm(archive: &str, root: &str) -> Result<(), Box<EvalAltResult>> {
    recipe_result((|| {
        let root = Path::new(root);
        fs::create_dir_all(root)?;
        let mut manifest = target::load(root)?;
        let mut command = Command::new("rpm2archive");
        command.args(["-n", archive]);
        let result = crate::process::Run::new(command)
            .keep(crate::process::Keep::Limit(512 * 1024 * 1024))
            .run()?;
        ensure!(
            result.exit.success(),
            "rpm2archive failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        read_tar(Cursor::new(result.stdout), root, &mut manifest, true)?;
        // rpm2archive omits RPM file capabilities; query the verified package,
        // then encode Linux v2 file capabilities as raw target xattr bytes.
        let mut command = Command::new("rpm");
        // Authentication is the recipe's pinned SHA256/signature acquisition
        // contract. Reading metadata must not open the compiler image's RPM DB
        // to search for signing keys.
        command.args([
            "-qp",
            "--nosignature",
            "--qf",
            "[%{FILENAMES}\t%{FILECAPS}\n]",
            archive,
        ]);
        let result = crate::helpers::internal::cmd::checked_capture(command)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        for line in std::str::from_utf8(&result.stdout)?.lines() {
            let (path, caps) = line
                .split_once('\t')
                .context("invalid RPM capability record")?;
            if caps.is_empty() || caps == "(none)" {
                continue;
            }
            let path = target::relative(Path::new(path.trim_start_matches('/')))?;
            let resolved = target::resolve_member(root, &path)?;
            let name = resolved
                .strip_prefix(root)?
                .to_str()
                .context("non-UTF8 RPM path")?;
            if let Some(entry) = manifest.entries.get_mut(name) {
                entry
                    .xattrs
                    .insert("security.capability".into(), hex::encode(file_caps(caps)?));
            } else {
                bail!("RPM capability has no payload: {name}");
            }
        }
        target::save(root, &manifest)
    })())
}

pub fn extract_tar(archive: &str, root: &str) -> Result<(), Box<EvalAltResult>> {
    recipe_result((|| {
        let root = Path::new(root);
        fs::create_dir_all(root)?;
        let mut manifest = target::load(root)?;
        read_tar(File::open(archive)?, root, &mut manifest, true)?;
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
                read_tar(&mut gz, root, &mut manifest, false)
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
        target::set_mode(Path::new(root), Path::new(path), mode)?;
        let full = Path::new(root).join(target::relative(Path::new(path))?);
        let grant = if full.is_dir() { 0o700 } else { 0o600 };
        fs::set_permissions(full, fs::Permissions::from_mode((mode & 0o777) | grant))?;
        Ok(())
    })())
}

pub fn finalize(root: &str) -> Result<(), Box<EvalAltResult>> {
    recipe_result(target::finalize(Path::new(root)))
}
