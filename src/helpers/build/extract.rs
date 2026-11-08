//! Build phase helpers
//!
//! Archive extraction with explicit source and destination paths.
//! All functions take explicit inputs and return explicit outputs.
//!
//! ## Example
//!
//! ```rhai
//! fn build(ctx) {
//!     extract(ctx.archive_path, RUNTIME.paths.build);
//!     let src_dir = RUNTIME.paths.build + "/myapp-1.0";
//!     shell_in(src_dir, "./configure");
//!     shell_in(src_dir, "make -j" + RUNTIME.options.jobs);
//!     ctx.src_dir = src_dir;
//!     ctx
//! }
//! ```

use crate::core::output;

use rhai::EvalAltResult;
use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::{Component, Path, PathBuf};

// ============================================================================
// Native archive extraction (no external tools needed)
// ============================================================================

fn normalize_lexical(path: &Path) -> PathBuf {
    // Lexically normalize a path (no filesystem access). This is used to
    // validate link targets without following symlinks.
    let mut out = PathBuf::new();
    let mut has_root = false;

    for c in path.components() {
        match c {
            Component::Prefix(p) => {
                out.clear();
                out.push(p.as_os_str());
                has_root = true;
            }
            Component::RootDir => {
                out.push(Component::RootDir.as_os_str());
                has_root = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                let popped = out
                    .components()
                    .next_back()
                    .is_some_and(|last| matches!(last, Component::Normal(_)));
                if popped {
                    out.pop();
                } else if !has_root {
                    // Preserve leading ".." for relative paths.
                    out.push("..");
                }
            }
            Component::Normal(seg) => out.push(seg),
        }
    }

    out
}

fn ensure_no_symlink_components(dest: &Path, full_path: &Path) -> Result<(), Box<EvalAltResult>> {
    full_path.strip_prefix(dest).map_err(|_| {
        format!(
            "archive contains path outside destination: {}",
            full_path.display()
        )
    })?;

    // Include the destination and its ancestors: even a safe archive member
    // escapes if extraction creates its destination through an existing link.
    for path in full_path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
        match std::fs::symlink_metadata(path) {
            Ok(md) if md.file_type().is_symlink() => {
                return Err(format!(
                    "archive extraction blocked: symlink in path component: {}",
                    path.display()
                )
                .into());
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "cannot inspect archive destination {}: {}",
                    path.display(),
                    e
                )
                .into());
            }
        }
    }

    Ok(())
}

fn ensure_link_target_within_dest(
    dest: &Path,
    base: &Path,
    link_name: &Path,
) -> Result<(), Box<EvalAltResult>> {
    if link_name.is_absolute()
        || link_name
            .components()
            .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
    {
        return Err(format!(
            "tar contains unsafe link target (absolute): {}",
            link_name.display()
        )
        .into());
    }

    // Resolve relative to `base`, then ensure it stays within dest.
    let candidate = normalize_lexical(&base.join(link_name));
    let norm_dest = normalize_lexical(dest);
    if candidate.strip_prefix(&norm_dest).is_err() {
        return Err(format!(
            "tar contains unsafe link target (escapes dest): {} -> {}",
            base.display(),
            link_name.display()
        )
        .into());
    }

    Ok(())
}

/// Extract a tar archive with optional decompression
fn extract_tar<R: Read>(reader: R, dest: &Path) -> Result<(), Box<EvalAltResult>> {
    let mut archive = tar::Archive::new(reader);

    // Unpack with security checks
    for entry in archive
        .entries()
        .map_err(|e| format!("tar read error: {}", e))?
    {
        let mut entry = entry.map_err(|e| format!("tar entry error: {}", e))?;

        let path = entry
            .path()
            .map_err(|e| format!("tar path error: {}", e))?
            .into_owned();

        // Security: reject paths that could escape the destination
        if path.is_absolute()
            || path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            return Err(format!("tar contains unsafe path: {}", path.display()).into());
        }

        // Some archives contain a "." entry; treat it as a no-op.
        if path.as_os_str().is_empty() || path == Path::new(".") {
            continue;
        }

        let full_path = dest.join(&path);

        // Block tar "symlink swap" / link-based extraction escapes:
        // if any existing component is a symlink, writing through it could
        // escape `dest` even when `path` is syntactically safe.
        ensure_no_symlink_components(dest, &full_path)?;

        // Validate link targets before extraction.
        let entry_type = entry.header().entry_type();
        if entry_type == tar::EntryType::Symlink || entry_type == tar::EntryType::Link {
            let link_name = entry
                .link_name()
                .map_err(|e| format!("tar link_name error: {}", e))?;
            if let Some(link_name) = link_name {
                // Symlink targets resolve from the link's directory; hardlink
                // targets name another archive member, relative to the root.
                let base = if entry_type == tar::EntryType::Link {
                    dest
                } else {
                    full_path.parent().unwrap_or(dest)
                };
                ensure_link_target_within_dest(dest, base, &link_name)?;
                if entry_type == tar::EntryType::Link {
                    ensure_no_symlink_components(dest, &dest.join(&link_name))?;
                }
            } else {
                return Err(format!(
                    "tar contains {} without link target: {}",
                    if entry_type == tar::EntryType::Symlink {
                        "symlink"
                    } else {
                        "hardlink"
                    },
                    path.display()
                )
                .into());
            }
        }

        // `full_path`'s ancestors were checked above.
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create directory {}: {}", parent.display(), e))?;
        }

        // Hardlinks must be created relative to the destination root, which
        // only `unpack_in` does; `unpack` would resolve them from the cwd.
        if entry_type == tar::EntryType::Link {
            let unpacked = entry
                .unpack_in(dest)
                .map_err(|e| format!("unpack error for {}: {}", path.display(), e))?;
            if !unpacked {
                return Err(format!("tar contains unsafe path: {}", path.display()).into());
            }
        } else {
            entry
                .unpack(&full_path)
                .map_err(|e| format!("unpack error for {}: {}", path.display(), e))?;
        }
    }

    Ok(())
}

/// Supported archive formats: canonical name, accepted aliases and suffixes.
const FORMATS: [(&str, &[&str], &[&str]); 7] = [
    ("tar.gz", &["tgz"], &[".tar.gz", ".tgz"]),
    ("tar.xz", &["txz"], &[".tar.xz", ".txz"]),
    ("tar.bz2", &["tbz2"], &[".tar.bz2", ".tbz2"]),
    ("tar.zst", &["tzst"], &[".tar.zst", ".tzst"]),
    ("zip", &[], &[".zip"]),
    ("tar", &[], &[".tar"]),
    ("apk", &[], &[".apk"]),
];

/// Extract a tar archive, decompressed as `format` names.
fn extract_tar_file(
    archive_path: &Path,
    dest: &Path,
    format: &str,
) -> Result<(), Box<EvalAltResult>> {
    let file = File::open(archive_path)
        .map_err(|e| format!("cannot open {}: {}", archive_path.display(), e))?;
    let reader = BufReader::new(file);
    match format {
        "tar.gz" => extract_tar(flate2::read::GzDecoder::new(reader), dest),
        "tar.xz" => extract_tar(xz2::read::XzDecoder::new(reader), dest),
        "tar.bz2" => extract_tar(bzip2::read::BzDecoder::new(reader), dest),
        "tar.zst" => extract_tar(
            zstd::stream::read::Decoder::new(reader)
                .map_err(|e| format!("zstd init error: {}", e))?,
            dest,
        ),
        _ => extract_tar(reader, dest),
    }
}

/// Extract an Alpine APK package.
///
/// APK packages are not a single tar.gz stream; they are concatenated gzip
/// members (signature/control/data tar streams). We must iterate each gzip
/// member and unpack every tar payload.
fn extract_apk(archive_path: &Path, dest: &Path) -> Result<(), Box<EvalAltResult>> {
    let file = File::open(archive_path)
        .map_err(|e| format!("cannot open {}: {}", archive_path.display(), e))?;
    let mut source = BufReader::new(file);

    let mut extracted_members = 0usize;
    while !source
        .fill_buf()
        .map_err(|e| format!("cannot read {}: {}", archive_path.display(), e))?
        .is_empty()
    {
        let mut gz = flate2::bufread::GzDecoder::new(source);
        let mut tar_bytes = Vec::new();
        gz.read_to_end(&mut tar_bytes)
            .map_err(|e| format!("apk gzip stream decode failed: {}", e))?;
        source = gz.into_inner();

        if tar_bytes.is_empty() {
            break;
        }

        extract_tar(Cursor::new(tar_bytes), dest)?;
        extracted_members += 1;
    }

    if extracted_members == 0 {
        return Err(format!(
            "apk extraction failed for '{}': no gzip members extracted",
            archive_path.display()
        )
        .into());
    }

    Ok(())
}

/// Extract a zip archive
fn extract_zip(archive_path: &Path, dest: &Path) -> Result<(), Box<EvalAltResult>> {
    let file = File::open(archive_path)
        .map_err(|e| format!("cannot open {}: {}", archive_path.display(), e))?;

    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("zip read error: {}", e))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("zip entry error: {}", e))?;

        let path = file
            .enclosed_name()
            .ok_or_else(|| format!("zip contains unsafe path: {}", file.name()))?;
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(format!("zip contains unsafe path: {}", file.name()).into());
        }
        let outpath = dest.join(path);
        ensure_no_symlink_components(dest, &outpath)?;

        // Zip stores a symlink as a member whose mode says so and whose data
        // is the target; extract it as a link, like tar, not as a text file.
        let is_symlink = file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000);
        if file.is_dir() {
            std::fs::create_dir_all(&outpath)
                .map_err(|e| format!("cannot create directory {}: {}", outpath.display(), e))?;
        } else if is_symlink {
            let mut target = String::new();
            file.read_to_string(&mut target).map_err(|e| {
                format!("zip symlink target error for {}: {}", outpath.display(), e)
            })?;
            let target = PathBuf::from(target);
            ensure_link_target_within_dest(dest, outpath.parent().unwrap_or(dest), &target)?;
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create directory {}: {}", parent.display(), e))?;
            }
            std::os::unix::fs::symlink(&target, &outpath)
                .map_err(|e| format!("cannot create symlink {}: {}", outpath.display(), e))?;
        } else {
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create directory {}: {}", parent.display(), e))?;
            }

            let mut outfile = std::fs::File::create(&outpath)
                .map_err(|e| format!("cannot create {}: {}", outpath.display(), e))?;
            std::io::copy(&mut file, &mut outfile)
                .map_err(|e| format!("write error for {}: {}", outpath.display(), e))?;

            {
                use std::os::unix::fs::PermissionsExt;
                // Same rule as tar extraction: permission bits only, no setuid/setgid/sticky.
                if let Some(mode) = file.unix_mode() {
                    std::fs::set_permissions(
                        &outpath,
                        std::fs::Permissions::from_mode(mode & 0o777),
                    )
                    .map_err(|e| {
                        format!("cannot set permissions for {}: {}", outpath.display(), e)
                    })?;
                }
            }
        }
    }

    Ok(())
}

// ============================================================================
// Public API
// ============================================================================

/// Detect archive format from filename extension
fn detect_format(archive: &str) -> Option<&'static str> {
    let path = archive.to_lowercase();
    FORMATS
        .iter()
        .find(|(_, _, suffixes)| suffixes.iter().any(|suffix| path.ends_with(suffix)))
        .map(|(name, _, _)| *name)
}

/// Extract an archive to a specific destination.
///
/// Auto-detects format from filename extension.
/// Supports every format in [`FORMATS`].
///
/// Uses native Rust libraries - no external tools required.
///
/// # Example
/// ```rhai
/// extract("/tmp/foo-1.0.tar.gz", "/tmp/build");
/// ```
pub fn extract(archive: &str, dest: &str) -> Result<(), Box<EvalAltResult>> {
    let format = detect_format(archive)
        .ok_or_else(|| format!("cannot detect archive format: {}", archive))?;

    extract_with_format(archive, dest, format)
}

/// Extract an archive to a specific destination with explicit format.
///
/// Uses native Rust libraries - no external tools required.
///
/// # Example
/// ```rhai
/// extract_with_format("/tmp/archive", "/tmp/build", "tar.gz");
/// ```
pub fn extract_with_format(
    archive: &str,
    dest: &str,
    format: &str,
) -> Result<(), Box<EvalAltResult>> {
    let archive_path = Path::new(archive);
    let dest_path = Path::new(dest);

    ensure_no_symlink_components(dest_path, dest_path)?;
    // Create destination directory if needed, after validating existing ancestors.
    std::fs::create_dir_all(dest_path)
        .map_err(|e| format!("cannot create destination directory: {}", e))?;

    let filename = archive_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "archive".to_string());

    let progress =
        crate::helpers::internal::progress::Spinner::new(&format!("extracting {filename}"));

    let requested = format.to_lowercase();
    let canonical = FORMATS
        .iter()
        .find(|(name, aliases, _)| *name == requested || aliases.contains(&requested.as_str()))
        .map(|(name, _, _)| *name)
        .ok_or_else(|| format!("unknown archive format: {}", format))?;
    let result = match canonical {
        "zip" => extract_zip(archive_path, dest_path),
        "apk" => extract_apk(archive_path, dest_path),
        tar => extract_tar_file(archive_path, dest_path, tar),
    };

    drop(progress);

    result?;
    output::detail(&format!("extracted {} to {}", filename, dest));
    Ok(())
}
