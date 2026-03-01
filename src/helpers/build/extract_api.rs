//! Build phase helpers
//!
//! Pure functions for extracting archives.
//! All functions take explicit inputs and return explicit outputs.
//!
//! ## Example
//!
//! ```rhai
//! fn build(ctx) {
//!     extract(ctx.archive_path, BUILD_DIR);
//!     let src_dir = BUILD_DIR + "/myapp-1.0";
//!     shell_in(src_dir, "./configure");
//!     shell_in(src_dir, "make -j" + NPROC);
//!     ctx.src_dir = src_dir;
//!     ctx
//! }
//! ```

use crate::core::output;
use indicatif::{ProgressBar, ProgressStyle};
use rhai::EvalAltResult;
use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

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
    let rel = full_path.strip_prefix(dest).map_err(|_| {
        format!(
            "tar contains path outside destination: {}",
            full_path.display()
        )
    })?;

    // Reject if any existing path component (including leaf) is a symlink.
    let mut cur = dest.to_path_buf();
    for comp in rel.components() {
        cur.push(comp);
        if let Ok(md) = std::fs::symlink_metadata(&cur)
            && md.file_type().is_symlink()
        {
            return Err(format!(
                "tar extraction blocked: symlink in path component: {}",
                cur.display()
            )
            .into());
        }
    }

    Ok(())
}

fn ensure_link_target_within_dest(
    dest: &Path,
    link_parent: &Path,
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

    // Resolve relative to the link's parent, then ensure it stays within dest.
    let candidate = normalize_lexical(&link_parent.join(link_name));
    let norm_dest = normalize_lexical(dest);
    if candidate.strip_prefix(&norm_dest).is_err() {
        return Err(format!(
            "tar contains unsafe link target (escapes dest): {} -> {}",
            link_parent.display(),
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

        // Get the path and check for path traversal (clone to avoid borrow issues)
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
                let link_parent = full_path.parent().unwrap_or(dest);
                ensure_link_target_within_dest(dest, link_parent, &link_name)?;
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

        // Create parent directories
        if let Some(parent) = full_path.parent() {
            if parent.starts_with(dest) {
                ensure_no_symlink_components(dest, parent)?;
            }
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create directory {}: {}", parent.display(), e))?;
        }

        // Unpack the entry
        entry
            .unpack(&full_path)
            .map_err(|e| format!("unpack error for {}: {}", path.display(), e))?;
    }

    Ok(())
}

/// Extract a tar.gz archive
pub(crate) fn extract_tar_gz(archive_path: &Path, dest: &Path) -> Result<(), Box<EvalAltResult>> {
    let file = File::open(archive_path)
        .map_err(|e| format!("cannot open {}: {}", archive_path.display(), e))?;
    let reader = BufReader::new(file);
    let decoder = flate2::read::GzDecoder::new(reader);
    extract_tar(decoder, dest)
}

/// Extract a tar.xz archive
fn extract_tar_xz(archive_path: &Path, dest: &Path) -> Result<(), Box<EvalAltResult>> {
    let file = File::open(archive_path)
        .map_err(|e| format!("cannot open {}: {}", archive_path.display(), e))?;
    let reader = BufReader::new(file);
    let decoder = xz2::read::XzDecoder::new(reader);
    extract_tar(decoder, dest)
}

/// Extract a tar.bz2 archive
fn extract_tar_bz2(archive_path: &Path, dest: &Path) -> Result<(), Box<EvalAltResult>> {
    let file = File::open(archive_path)
        .map_err(|e| format!("cannot open {}: {}", archive_path.display(), e))?;
    let reader = BufReader::new(file);
    let decoder = bzip2::read::BzDecoder::new(reader);
    extract_tar(decoder, dest)
}

/// Extract a tar.zst archive
fn extract_tar_zst(archive_path: &Path, dest: &Path) -> Result<(), Box<EvalAltResult>> {
    let file = File::open(archive_path)
        .map_err(|e| format!("cannot open {}: {}", archive_path.display(), e))?;
    let reader = BufReader::new(file);
    let decoder =
        zstd::stream::read::Decoder::new(reader).map_err(|e| format!("zstd init error: {}", e))?;
    extract_tar(decoder, dest)
}

/// Extract a plain tar archive (no compression)
fn extract_tar_plain(archive_path: &Path, dest: &Path) -> Result<(), Box<EvalAltResult>> {
    let file = File::open(archive_path)
        .map_err(|e| format!("cannot open {}: {}", archive_path.display(), e))?;
    let reader = BufReader::new(file);
    extract_tar(reader, dest)
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
pub(crate) fn extract_zip(archive_path: &Path, dest: &Path) -> Result<(), Box<EvalAltResult>> {
    let file = File::open(archive_path)
        .map_err(|e| format!("cannot open {}: {}", archive_path.display(), e))?;

    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("zip read error: {}", e))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("zip entry error: {}", e))?;

        // Get the path from the zip entry
        let outpath = match file.enclosed_name() {
            Some(path) => dest.join(path),
            None => {
                // Skip entries with unsafe paths
                continue;
            }
        };

        if file.is_dir() {
            std::fs::create_dir_all(&outpath)
                .map_err(|e| format!("cannot create directory {}: {}", outpath.display(), e))?;
        } else {
            // Create parent directories
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create directory {}: {}", parent.display(), e))?;
            }

            // Extract the file
            let mut outfile = std::fs::File::create(&outpath)
                .map_err(|e| format!("cannot create {}: {}", outpath.display(), e))?;
            std::io::copy(&mut file, &mut outfile)
                .map_err(|e| format!("write error for {}: {}", outpath.display(), e))?;

            // Set permissions on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = file.unix_mode() {
                    std::fs::set_permissions(&outpath, std::fs::Permissions::from_mode(mode)).ok();
                }
            }
        }
    }

    Ok(())
}

// ============================================================================
// Public API - Pure functions
// ============================================================================

/// Detect archive format from filename extension
pub fn detect_format(archive: &str) -> Option<&'static str> {
    let path = archive.to_lowercase();
    if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
        Some("tar.gz")
    } else if path.ends_with(".tar.xz") || path.ends_with(".txz") {
        Some("tar.xz")
    } else if path.ends_with(".tar.bz2") || path.ends_with(".tbz2") {
        Some("tar.bz2")
    } else if path.ends_with(".tar.zst") || path.ends_with(".tzst") {
        Some("tar.zst")
    } else if path.ends_with(".zip") {
        Some("zip")
    } else if path.ends_with(".tar") {
        Some("tar")
    } else if path.ends_with(".apk") {
        Some("apk")
    } else {
        None
    }
}

/// Extract an archive to a specific destination.
///
/// Auto-detects format from filename extension.
/// Supports: tar.gz, tar.xz, tar.bz2, tar.zst, zip, tar, apk.
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

    // Create destination directory if needed
    std::fs::create_dir_all(dest_path)
        .map_err(|e| format!("cannot create destination directory: {}", e))?;

    let filename = archive_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "archive".to_string());

    // Create spinner for extraction
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("     {spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"),
    );
    pb.set_message(format!("extracting {}", filename));
    pb.enable_steady_tick(Duration::from_millis(80));

    let result = match format.to_lowercase().as_str() {
        "tar.gz" | "tgz" => extract_tar_gz(archive_path, dest_path),
        "tar.xz" | "txz" => extract_tar_xz(archive_path, dest_path),
        "tar.bz2" | "tbz2" => extract_tar_bz2(archive_path, dest_path),
        "tar.zst" | "tzst" => extract_tar_zst(archive_path, dest_path),
        "tar" => extract_tar_plain(archive_path, dest_path),
        "zip" => extract_zip(archive_path, dest_path),
        "apk" => extract_apk(archive_path, dest_path),
        _ => {
            pb.finish_and_clear();
            return Err(format!("unknown archive format: {}", format).into());
        }
    };

    pb.finish_and_clear();

    result?;
    output::detail(&format!("extracted {} to {}", filename, dest));
    Ok(())
}
