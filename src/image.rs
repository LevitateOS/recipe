//! The root image contract shared by the image producer (`recipe-image`) and
//! the OS builder's own roots: the rules every layer member passes
//! ([`Validator`]) and the one-layer OCI image layout Ginux imports
//! ([`write_layout`]; `ginux import-image`, docs/ginux-interim-lifecycle-design.md
//! section 2.5).
//!
//! Every member of a layer runs through the same [`Validator`], so no
//! producer can publish an image a Ginux run then refuses.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

/// Largest image archive.
pub const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Largest sum of regular-file sizes in an image.
pub const MAX_UNPACKED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// Most members in an image.
pub const MAX_ENTRIES: usize = 500_000;
/// Longest member path or link target.
pub const MAX_PATH_BYTES: usize = 4096;

/// Permission bits an unpacked member keeps: the domain root is read-only.
pub fn installed_mode(mode: u32) -> u32 {
    mode & 0o555
}

/// The member every layer leaves out ([`Validator::check`]).
pub const HOSTS: &str = "etc/hosts";

/// The uncompressed tar of an image layout, beside the layout's own files.
pub const LAYOUT_TAR: &str = "image.oci.tar";

/// Normalize a header for a reproducible image: root ownership, epoch mtime.
pub fn normalize_header(header: &mut tar::Header) {
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_cksum();
}

/// Normalized relative member path: `.` components dropped, anything that is
/// not a plain name rejected. The empty path is the image root.
pub fn member_path(path: &Path) -> Result<PathBuf> {
    let mut relative = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => relative.push(name),
            _ => bail!("unsafe rootfs image path: {}", path.display()),
        }
    }
    if relative.as_os_str().len() > MAX_PATH_BYTES {
        bail!("rootfs image path exceeds {MAX_PATH_BYTES} bytes");
    }
    Ok(relative)
}

/// Checks members in archive order against the image rules.
#[derive(Default)]
pub struct Validator {
    /// Members seen so far → regular-file installed mode (None for non-files).
    members: HashMap<PathBuf, Option<u32>>,
    unpacked: u64,
}

impl Validator {
    /// Validate the next member. `path` must come from [`member_path`] and be
    /// non-empty; `link` is the target of a symlink or hardlink.
    pub fn check(
        &mut self,
        path: &Path,
        kind: tar::EntryType,
        size: u64,
        mode: u32,
        link: Option<&Path>,
    ) -> Result<()> {
        if self.members.len() >= MAX_ENTRIES {
            bail!("rootfs image entry limit exceeded");
        }
        if crate::domain::RESERVED_ROOTS
            .iter()
            .any(|name| path.starts_with(name))
        {
            bail!(
                "rootfs image uses reserved invocation path: {}",
                path.display()
            );
        }
        // Ginux's expander treats a `.wh.` name as an OCI whiteout.
        if path
            .components()
            .any(|part| part.as_os_str().as_encoded_bytes().starts_with(b".wh."))
        {
            bail!(
                "rootfs image member {} has a name starting `.wh.` (an OCI whiteout)",
                path.display()
            );
        }
        // Recipe injects `/etc/hosts` into every run, and the injector never
        // overwrites a file the image has.
        if path == Path::new(HOSTS) {
            bail!("rootfs image must not contain {HOSTS}: Recipe injects it into every run");
        }
        if self.members.contains_key(path) {
            bail!("duplicate rootfs image path: {}", path.display());
        }
        let file_mode = if kind.is_file() {
            self.unpacked = self
                .unpacked
                .checked_add(size)
                .filter(|total| *total <= MAX_UNPACKED_BYTES)
                .context("rootfs unpacked size limit exceeded")?;
            Some(installed_mode(mode))
        } else if kind.is_dir() {
            None
        } else if kind.is_symlink() {
            let target = link.context("rootfs symlink has no target")?;
            if target.as_os_str().is_empty() || target.as_os_str().len() > MAX_PATH_BYTES {
                bail!("invalid rootfs symlink target: {}", path.display());
            }
            // Absolute targets are meaningful inside the chroot; relative ones
            // must not climb above the image root.
            if !target.is_absolute() {
                let mut depth = path.components().count() - 1;
                for part in target.components() {
                    match part {
                        Component::Normal(_) => depth += 1,
                        Component::ParentDir if depth > 0 => depth -= 1,
                        Component::ParentDir => {
                            bail!("rootfs symlink escapes the image root: {}", path.display())
                        }
                        _ => {}
                    }
                }
            }
            None
        } else if kind.is_hard_link() {
            let target = member_path(link.context("rootfs hardlink has no target")?)?;
            match self.members.get(&target) {
                Some(Some(target_mode)) if *target_mode == installed_mode(mode) => {}
                Some(Some(_)) => bail!(
                    "rootfs hardlink target kind or mode disagrees: {}",
                    path.display()
                ),
                _ => bail!(
                    "rootfs hardlink target must be a prior file: {}",
                    path.display()
                ),
            }
            None
        } else {
            bail!("unsupported rootfs image entry type at {}", path.display());
        };
        self.members.insert(path.to_owned(), file_mode);
        Ok(())
    }
}

const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.image.config.v1+json";
const LAYER_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar";
/// Largest `index.json` or manifest blob read when checking a layout.
const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;

// The layout's JSON documents. Field order is the byte order: serde_json
// writes struct fields in declaration order, with no whitespace, so the
// manifest digest (the image's identity) is stable for the same layer.
#[derive(Serialize)]
struct OciLayout {
    #[serde(rename = "imageLayoutVersion")]
    image_layout_version: &'static str,
}
#[derive(Serialize)]
struct Platform {
    architecture: &'static str,
    os: &'static str,
}
#[derive(Serialize)]
struct RootFs {
    #[serde(rename = "type")]
    kind: &'static str,
    diff_ids: Vec<String>,
}
#[derive(Serialize)]
struct Empty {}
#[derive(Serialize)]
struct Config {
    architecture: &'static str,
    os: &'static str,
    rootfs: RootFs,
    config: Empty,
}
#[derive(Serialize)]
struct Descriptor {
    #[serde(rename = "mediaType")]
    media_type: &'static str,
    digest: String,
    size: u64,
}
#[derive(Serialize)]
struct Manifest {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    #[serde(rename = "mediaType")]
    media_type: &'static str,
    config: Descriptor,
    layers: Vec<Descriptor>,
}
#[derive(Serialize)]
struct IndexEntry {
    #[serde(rename = "mediaType")]
    media_type: &'static str,
    digest: String,
    size: u64,
    platform: Platform,
}
#[derive(Serialize)]
struct Index {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    manifests: Vec<IndexEntry>,
}

/// What [`write_layout_record`] wrote.
#[derive(Debug, Clone)]
pub struct LayoutRecord {
    /// The manifest digest (hex): the image's identity, as
    /// `local/NAME@sha256:<manifest>`.
    pub manifest: String,
    /// The layer digest (hex), equal to its diff_id (it is uncompressed).
    pub layer: String,
    /// The layer's size in bytes.
    pub layer_bytes: u64,
}

/// Counts and hashes every byte read through it.
struct Hashing<R> {
    inner: R,
    hasher: Sha256,
    bytes: u64,
}

impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        if self.bytes > MAX_ARCHIVE_BYTES {
            return Err(std::io::Error::other(format!(
                "the layer is larger than {MAX_ARCHIVE_BYTES} bytes"
            )));
        }
        Ok(n)
    }
}

/// Check every member of the layer tar at `layer` against [`Validator`] and
/// return its SHA-256 (hex) and size, read in one pass.
pub fn check_layer(layer: &Path) -> Result<(String, u64)> {
    let file = crate::fsx::open_regular(layer)?;
    let mut archive = tar::Archive::new(Hashing {
        inner: std::io::BufReader::with_capacity(1 << 20, file),
        hasher: Sha256::new(),
        bytes: 0,
    });
    let mut validator = Validator::default();
    for entry in archive
        .entries()
        .with_context(|| format!("reading the layer {}", layer.display()))?
    {
        let entry = entry.with_context(|| format!("reading the layer {}", layer.display()))?;
        let path = member_path(&entry.path()?)?;
        if path.as_os_str().is_empty() {
            // The archive's own top directory (`./`).
            continue;
        }
        let header = entry.header();
        let link = entry.link_name()?.map(|link| link.into_owned());
        validator
            .check(
                &path,
                header.entry_type(),
                header.size()?,
                header.mode()?,
                link.as_deref(),
            )
            .with_context(|| format!("the layer {} breaks an image rule", layer.display()))?;
    }
    // The end-of-archive blocks and padding belong to the layer's bytes.
    let mut rest = archive.into_inner();
    std::io::copy(&mut rest, &mut std::io::sink())?;
    Ok((hex::encode(rest.hasher.finalize()), rest.bytes))
}

/// Write the one-layer OCI image layout of the uncompressed layer tar
/// `layer` into the fresh directory `out` (created here; it must not exist)
/// and return the manifest digest (hex). See [`write_layout_record`].
pub fn write_layout(layer: &Path, out: &Path) -> Result<String> {
    Ok(write_layout_record(layer, out)?.manifest)
}

/// [`write_layout`], returning the layer's digest and size too.
///
/// `out` receives `oci-layout`, `index.json`, `blobs/sha256/<layer>` (a hard
/// link to `layer` where both are on one filesystem, else a copy),
/// `blobs/sha256/<config>`, `blobs/sha256/<manifest>`, and [`LAYOUT_TAR`]:
/// an uncompressed tar of exactly those layout members (`oci-layout`,
/// `index.json`, `blobs/`, `blobs/sha256/`, the blobs by name), with
/// normalized headers. [`LAYOUT_TAR`] is what `ginux import-image` takes;
/// `out` itself holds files Ginux does not accept in a layout directory.
/// The JSON documents are byte-stable (fixed key order, no whitespace, no
/// trailing newline), so the same layer always has the same manifest digest.
/// Every layer member passes [`Validator`] first.
pub fn write_layout_record(layer: &Path, out: &Path) -> Result<LayoutRecord> {
    let (layer_digest, layer_bytes) = check_layer(layer)?;
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::create_dir(out).with_context(|| {
        format!(
            "cannot create the fresh layout directory {} (it must not exist)",
            out.display()
        )
    })?;
    let blobs = out.join("blobs/sha256");
    std::fs::create_dir_all(&blobs)?;
    let layer_blob = blobs.join(&layer_digest);
    if std::fs::hard_link(layer, &layer_blob).is_err() {
        let mut source = crate::fsx::open_regular(layer)?;
        let mut target = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&layer_blob)
            .with_context(|| format!("cannot create {}", layer_blob.display()))?;
        let (bytes, digest) = crate::fsx::copy_hashed(&mut source, &mut target, MAX_ARCHIVE_BYTES)?;
        if digest != layer_digest || bytes != layer_bytes {
            bail!("the layer {} changed while it was copied", layer.display());
        }
        target.sync_all()?;
    }

    let config = serde_json::to_vec(&Config {
        architecture: "amd64",
        os: "linux",
        rootfs: RootFs {
            kind: "layers",
            diff_ids: vec![format!("sha256:{layer_digest}")],
        },
        config: Empty {},
    })?;
    let config_digest = hex::encode(Sha256::digest(&config));
    let manifest = serde_json::to_vec(&Manifest {
        schema_version: 2,
        media_type: MANIFEST_MEDIA_TYPE,
        config: Descriptor {
            media_type: CONFIG_MEDIA_TYPE,
            digest: format!("sha256:{config_digest}"),
            size: config.len() as u64,
        },
        layers: vec![Descriptor {
            media_type: LAYER_MEDIA_TYPE,
            digest: format!("sha256:{layer_digest}"),
            size: layer_bytes,
        }],
    })?;
    let manifest_digest = hex::encode(Sha256::digest(&manifest));
    let index = serde_json::to_vec(&Index {
        schema_version: 2,
        manifests: vec![IndexEntry {
            media_type: MANIFEST_MEDIA_TYPE,
            digest: format!("sha256:{manifest_digest}"),
            size: manifest.len() as u64,
            platform: Platform {
                architecture: "amd64",
                os: "linux",
            },
        }],
    })?;
    let oci_layout = serde_json::to_vec(&OciLayout {
        image_layout_version: "1.0.0",
    })?;
    let write_new = |path: &Path, bytes: &[u8]| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("cannot create {}", path.display()))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    };
    write_new(&blobs.join(&config_digest), &config)?;
    write_new(&blobs.join(&manifest_digest), &manifest)?;
    write_new(&out.join("index.json"), &index)?;
    write_new(&out.join("oci-layout"), &oci_layout)?;

    // The tar of the layout, members in the order Ginux's own CLI writes them.
    let mut names = vec![
        layer_digest.clone(),
        config_digest.clone(),
        manifest_digest.clone(),
    ];
    names.sort();
    names.dedup();
    let tar_path = out.join(LAYOUT_TAR);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tar_path)
        .with_context(|| format!("cannot create {}", tar_path.display()))?;
    let mut builder = tar::Builder::new(std::io::BufWriter::with_capacity(1 << 20, file));
    let header = |kind: tar::EntryType, mode: u32, size: u64| {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_mode(mode);
        header.set_size(size);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header
    };
    builder.append_data(
        &mut header(tar::EntryType::Regular, 0o644, oci_layout.len() as u64),
        "oci-layout",
        oci_layout.as_slice(),
    )?;
    builder.append_data(
        &mut header(tar::EntryType::Regular, 0o644, index.len() as u64),
        "index.json",
        index.as_slice(),
    )?;
    for directory in ["blobs/", "blobs/sha256/"] {
        builder.append_data(
            &mut header(tar::EntryType::Directory, 0o755, 0),
            directory,
            std::io::empty(),
        )?;
    }
    for name in &names {
        let path = blobs.join(name);
        let file = crate::fsx::open_regular(&path)?;
        let size = file.metadata()?.len();
        builder.append_data(
            &mut header(tar::EntryType::Regular, 0o644, size),
            format!("blobs/sha256/{name}"),
            file.take(size),
        )?;
    }
    let writer = builder.into_inner()?;
    let file = writer.into_inner().map_err(|error| error.into_error())?;
    file.sync_all()?;
    std::fs::File::open(&blobs)?.sync_all()?;
    std::fs::File::open(out)?.sync_all()?;
    Ok(LayoutRecord {
        manifest: manifest_digest,
        layer: layer_digest,
        layer_bytes,
    })
}

/// The manifest digest (hex) of the image layout at `path`: a layout
/// directory, or an uncompressed tar of one ([`LAYOUT_TAR`]). Read from
/// `index.json` (exactly one manifest) and checked against the manifest
/// blob's own SHA-256. Layers are not read: `ginux import-image` verifies
/// every blob when it imports the layout.
pub fn layout_manifest_digest(path: &Path) -> Result<String> {
    let metadata =
        std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    let mut small = HashMap::<String, Vec<u8>>::new();
    let index = if metadata.is_dir() {
        read_bounded(&path.join("index.json"))?
    } else {
        let file = crate::fsx::open_regular(path)?;
        let mut archive = tar::Archive::new(std::io::BufReader::new(file));
        let mut index = None;
        for entry in archive.entries()? {
            let mut entry = entry?;
            if !entry.header().entry_type().is_file() || entry.size() > MAX_METADATA_BYTES {
                continue;
            }
            let name = entry.path()?.to_string_lossy().into_owned();
            let name = name.strip_prefix("./").unwrap_or(&name).to_owned();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            if name == "index.json" {
                index = Some(bytes);
            } else if let Some(hex) = name.strip_prefix("blobs/sha256/") {
                small.insert(hex.to_owned(), bytes);
            }
        }
        index.with_context(|| format!("{} has no index.json", path.display()))?
    };
    let index: serde_json::Value = serde_json::from_slice(&index)
        .with_context(|| format!("{}: index.json is not JSON", path.display()))?;
    let manifests = index["manifests"]
        .as_array()
        .with_context(|| format!("{}: index.json has no manifests", path.display()))?;
    let [entry] = manifests.as_slice() else {
        bail!(
            "{}: index.json names {} manifests, want exactly one",
            path.display(),
            manifests.len()
        );
    };
    if entry["mediaType"] != MANIFEST_MEDIA_TYPE {
        bail!(
            "{}: index.json does not name an OCI image manifest",
            path.display()
        );
    }
    let hex = entry["digest"]
        .as_str()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .with_context(|| {
            format!(
                "{}: index.json has no sha256 manifest digest",
                path.display()
            )
        })?
        .to_owned();
    let manifest = if metadata.is_dir() {
        read_bounded(&path.join("blobs/sha256").join(&hex))?
    } else {
        small
            .remove(&hex)
            .with_context(|| format!("{} has no manifest blob {hex}", path.display()))?
    };
    let actual = hex::encode(Sha256::digest(&manifest));
    if actual != hex {
        bail!(
            "{}: the manifest blob {hex} has SHA-256 {actual}",
            path.display()
        );
    }
    Ok(hex)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>> {
    let file = crate::fsx::open_regular(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_METADATA_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        bail!(
            "{} is larger than {MAX_METADATA_BYTES} bytes",
            path.display()
        );
    }
    Ok(bytes)
}
