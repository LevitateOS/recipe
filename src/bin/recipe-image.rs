//! Explicit bootstrap composer. Consumes pinned local artifacts; runs no Rhai.
use anyhow::{Context, Result, bail};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use levitate_recipe::{fsx, image};

/// Largest single pinned input or input-archive member.
const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// Largest sum of pinned inputs.
const MAX_INPUTS_TOTAL: u64 = 32 * 1024 * 1024 * 1024;

#[derive(Parser)]
#[command(about = "Compose a prepared Ginux compiler image from pinned local artifacts")]
struct Cli {
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    input_root: PathBuf,
    /// Fresh directory receiving rootfs.tar and provenance.json.
    #[arg(long)]
    output: PathBuf,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: String,
    sha256: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    base: Input,
    rust: Input,
    #[serde(default)]
    rust_additions: Vec<Input>,
    rust_components: Vec<String>,
    rpms: Vec<Input>,
}
struct Addition {
    header: tar::Header,
    data: Option<tempfile::TempPath>,
    link: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut manifest_file = fsx::open_regular(&cli.manifest)?;
    if manifest_file.metadata()?.len() > 65536 {
        bail!("manifest exceeds 64 KiB");
    }
    let mut bytes = Vec::new();
    manifest_file.read_to_end(&mut bytes)?;
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    if manifest.version != 1
        || manifest.rpms.len() > 64
        || manifest.rust_additions.len() > 16
        || manifest.rust_components.len() > 16
        || manifest.rust_components.is_empty()
    {
        bail!("unsupported or oversized image manifest");
    }
    let selected_components = manifest.rust_components.iter().collect::<HashSet<_>>();
    if selected_components.len() != manifest.rust_components.len() {
        bail!("duplicate selected Rust components");
    }
    let parent = cli
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    if cli.output.try_exists()? {
        bail!("output already exists: {}", cli.output.display());
    }
    let output = tempfile::Builder::new()
        .prefix(".recipe-image-")
        .tempdir_in(parent)?;
    let scratch = tempfile::Builder::new()
        .prefix("recipe-image-inputs-")
        .tempdir_in(parent)?;
    let input_root = cli.input_root.canonicalize()?;
    let mut total = 0;
    let base = snapshot(&input_root, &manifest.base, scratch.path(), &mut total)?;
    let mut additions = BTreeMap::new();
    let mut expanded = 0;
    for input in &manifest.rpms {
        let rpm = snapshot(&input_root, input, scratch.path(), &mut total)?;
        let mut archive = tempfile::NamedTempFile::new_in(scratch.path())?;
        // rpm2archive emits tar to stdout. Never extract RPM paths on the host.
        // The command reads the exact verified private copy, not the original.
        let mut child = Command::new("rpm2archive")
            .arg("-n")
            .arg(rpm.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context("rpm2archive is required for the explicit image producer")?;
        let copied = fsx::copy_hashed(
            &mut child.stdout.take().context("missing rpm archive stream")?,
            &mut archive,
            MAX_INPUT_BYTES,
        );
        if copied.is_err() {
            let _ = child.kill();
        }
        let status = child.wait()?;
        copied?;
        if !status.success() {
            bail!("rpm2archive failed for {}: {status}", input.path);
        }
        archive.as_file_mut().seek(SeekFrom::Start(0))?;
        collect(
            tar::Archive::new(archive.as_file_mut()),
            &mut additions,
            scratch.path(),
            &mut expanded,
            |path| Ok(path.starts_with("usr/").then(|| path.to_owned())),
        )?;
    }
    let mut components = HashSet::new();
    for input in std::iter::once(&manifest.rust).chain(&manifest.rust_additions) {
        let rust = snapshot(&input_root, input, scratch.path(), &mut total)?;
        let mut archive_components = HashSet::new();
        collect(
            tar::Archive::new(flate2::read::GzDecoder::new(rust.reopen()?)),
            &mut additions,
            scratch.path(),
            &mut expanded,
            |path| {
                let parts = path.split('/').collect::<Vec<_>>();
                if parts.len() < 3 || !selected_components.contains(&parts[1].to_owned()) {
                    return Ok(None);
                }
                if archive_components.insert(parts[1].to_owned())
                    && !components.insert(parts[1].to_owned())
                {
                    bail!("Rust component occurs in multiple inputs: {}", parts[1]);
                }
                if !matches!(parts[2], "bin" | "lib" | "libexec") {
                    return Ok(None);
                }
                Ok(Some(format!("usr/{}", parts[2..].join("/"))))
            },
        )?;
    }
    if manifest
        .rust_components
        .iter()
        .any(|name| !components.contains(name))
    {
        bail!("Rust archive is missing a selected component");
    }

    let image_path = output.path().join("rootfs.tar");
    let image = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&image_path)?;
    let mut writer = tar::Builder::new(image);
    // Every emitted member passes the consumer's rules in consumer order.
    let mut validator = image::Validator::default();
    let mut seen = HashSet::new();
    let mut emitted = HashSet::new();
    for entry in tar::Archive::new(base.reopen()?).entries()? {
        let mut entry = entry?;
        let path = safe_name(&entry.path()?)?;
        if path.is_empty() {
            continue;
        }
        if !seen.insert(path.clone()) {
            bail!("duplicate base image entry: {path}");
        }
        if additions.contains_key(&path) {
            continue;
        }
        let mut header = entry.header().clone();
        let link = entry.link_name()?.map(|link| link.into_owned());
        validator.check(
            Path::new(&path),
            header.entry_type(),
            header.size()?,
            header.mode()?,
            link.as_deref(),
        )?;
        image::normalize_header(&mut header);
        if let Some(link) = link {
            writer.append_link(&mut header, &path, link)?;
        } else {
            writer.append_data(&mut header, &path, &mut entry)?;
        }
        emitted.insert(path);
    }
    // Additions go in path order, except that a hardlink waits for a later
    // pass until its target has been written.
    let mut pending = additions.into_iter().collect::<Vec<_>>();
    while !pending.is_empty() {
        let mut deferred = Vec::new();
        let mut progressed = false;
        for (path, mut entry) in pending {
            let waits = entry.header.entry_type().is_hard_link()
                && entry
                    .link
                    .as_deref()
                    .is_some_and(|target| !emitted.contains(&*target.to_string_lossy()));
            if waits {
                deferred.push((path, entry));
                continue;
            }
            validator.check(
                Path::new(&path),
                entry.header.entry_type(),
                entry.header.size()?,
                entry.header.mode()?,
                entry.link.as_deref(),
            )?;
            image::normalize_header(&mut entry.header);
            if let Some(link) = entry.link {
                writer.append_link(&mut entry.header, &path, link)?;
            } else if let Some(pathname) = entry.data {
                let mut file = File::open(&pathname)?;
                writer.append_data(&mut entry.header, &path, &mut file)?;
            } else {
                writer.append_data(&mut entry.header, &path, std::io::empty())?;
            }
            emitted.insert(path);
            progressed = true;
        }
        if !progressed {
            bail!("image hardlink target is never written: {}", deferred[0].0);
        }
        pending = deferred;
    }
    writer.finish()?;
    writer.into_inner()?.sync_all()?;
    let mut image = File::open(&image_path)?;
    let (size, digest) = fsx::copy_hashed(&mut image, &mut std::io::sink(), image::MAX_ARCHIVE_BYTES)?;
    let record = serde_json::json!({"version":1,"image":"rootfs.tar","sha256":digest,"bytes":size,"inputs":manifest});
    let mut provenance = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.path().join("provenance.json"))?;
    serde_json::to_writer_pretty(&mut provenance, &record)?;
    provenance.write_all(b"\n")?;
    provenance.sync_all()?;
    File::open(output.path())?.sync_all()?;
    publish_directory(output.path(), &cli.output)?;
    let _ = output.keep();
    File::open(parent)?.sync_all()?;
    println!("{}", serde_json::to_string_pretty(&record)?);
    Ok(())
}

fn snapshot(
    root: &Path,
    input: &Input,
    scratch: &Path,
    total: &mut u64,
) -> Result<tempfile::NamedTempFile> {
    let name = safe_name(Path::new(&input.path))?;
    if name.is_empty()
        || input.sha256.len() != 64
        || !input.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        bail!("invalid input identity");
    }
    let path = root.join(name);
    if !path.canonicalize()?.starts_with(root) {
        bail!("input escapes selected root");
    }
    let mut source = fsx::open_regular(&path)?;
    let mut copy = tempfile::NamedTempFile::new_in(scratch)?;
    let (bytes, digest) = fsx::copy_hashed(&mut source, &mut copy, MAX_INPUT_BYTES)?;
    *total = total
        .checked_add(bytes)
        .filter(|size| *size <= MAX_INPUTS_TOTAL)
        .context("input total exceeds byte bound")?;
    if digest != input.sha256 {
        bail!("input digest mismatch: {}", input.path);
    }
    copy.as_file_mut().seek(SeekFrom::Start(0))?;
    Ok(copy)
}
fn safe_name(path: &Path) -> Result<String> {
    Ok(image::member_path(path)?
        .to_str()
        .context("non-UTF-8 archive path")?
        .to_owned())
}
fn collect<R: Read>(
    mut archive: tar::Archive<R>,
    output: &mut BTreeMap<String, Addition>,
    scratch: &Path,
    total: &mut u64,
    mut select: impl FnMut(&str) -> Result<Option<String>>,
) -> Result<()> {
    let mut seen = HashSet::new();
    let mut selected = HashSet::new();
    let mut hardlinks = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let original = safe_name(&entry.path()?)?;
        if !seen.insert(original.clone()) || seen.len() > image::MAX_ENTRIES {
            bail!("duplicate or excessive input archive entries");
        }
        let Some(path) = select(&original)? else {
            continue;
        };
        if entry.size() > MAX_INPUT_BYTES {
            bail!("image member exceeds byte bound: {path}");
        }
        *total = total
            .checked_add(entry.size())
            .filter(|size| *size <= image::MAX_UNPACKED_BYTES)
            .context("expanded additions exceed byte bound")?;
        let mut header = entry.header().clone();
        selected.insert(path.clone());
        let link = if header.entry_type().is_hard_link() {
            let target = safe_name(&entry.link_name()?.context("missing hardlink target")?)?;
            let target =
                select(&target)?.context("hardlink target is outside selected component")?;
            hardlinks.push((path.clone(), target.clone()));
            Some(PathBuf::from(target))
        } else {
            entry.link_name()?.map(|path| path.into_owned())
        };
        if let Some(previous) = output.get(&path) {
            if previous.header.entry_type().is_dir() && header.entry_type().is_dir() {
                continue;
            }
            bail!("conflicting image additions: {path}");
        }
        let data = if header.entry_type().is_file() {
            let mut file = tempfile::NamedTempFile::new_in(scratch)?;
            let size = fsx::copy_hashed(&mut entry, &mut file, MAX_INPUT_BYTES)?.0;
            if size != header.size()? {
                bail!("truncated image addition: {path}");
            }
            Some(file.into_temp_path())
        } else {
            header.set_size(0);
            None
        };
        output.insert(path, Addition { header, data, link });
        if output.len() > image::MAX_ENTRIES {
            bail!("too many image additions");
        }
    }
    for (path, target) in hardlinks {
        if !selected.contains(&target)
            || !output
                .get(&target)
                .is_some_and(|entry| entry.data.is_some())
        {
            bail!("hardlink must name a regular file in the same selected archive: {path}");
        }
    }
    Ok(())
}
fn publish_directory(source: &Path, destination: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error())
            .context("cannot publish fresh image directory");
    }
    Ok(())
}
