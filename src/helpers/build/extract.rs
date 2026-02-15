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
use std::io::{BufReader, Read};
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
fn extract_tar_gz(archive_path: &Path, dest: &Path) -> Result<(), Box<EvalAltResult>> {
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

/// Extract a zip archive
fn extract_zip(archive_path: &Path, dest: &Path) -> Result<(), Box<EvalAltResult>> {
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
fn detect_format(archive: &str) -> Option<&'static str> {
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
        // Alpine APK packages are gzipped tarballs
        Some("tar.gz")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_detect_format() {
        assert_eq!(detect_format("foo.tar.gz"), Some("tar.gz"));
        assert_eq!(detect_format("foo.tgz"), Some("tar.gz"));
        assert_eq!(detect_format("foo.tar.xz"), Some("tar.xz"));
        assert_eq!(detect_format("foo.txz"), Some("tar.xz"));
        assert_eq!(detect_format("foo.tar.bz2"), Some("tar.bz2"));
        assert_eq!(detect_format("foo.tbz2"), Some("tar.bz2"));
        assert_eq!(detect_format("foo.tar.zst"), Some("tar.zst"));
        assert_eq!(detect_format("foo.tzst"), Some("tar.zst"));
        assert_eq!(detect_format("foo.zip"), Some("zip"));
        assert_eq!(detect_format("foo.tar"), Some("tar"));
        assert_eq!(detect_format("foo.apk"), Some("tar.gz"));
        assert_eq!(detect_format("foo.unknown"), None);
    }

    #[test]
    fn test_extract_tar_gz() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("test.tar.gz");
        let extract_dir = temp_dir.path().join("extracted");

        // Create a simple tar.gz archive
        let file = File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        // Add a file to the archive using append_data which handles headers correctly
        let content = b"Hello, World!";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "test.txt", &content[..])
            .unwrap();

        // Properly finish and close the archive
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        // Extract it
        std::fs::create_dir_all(&extract_dir).unwrap();
        extract_tar_gz(&archive_path, &extract_dir).unwrap();

        // Verify
        let extracted_file = extract_dir.join("test.txt");
        assert!(extracted_file.exists());
        assert_eq!(
            std::fs::read_to_string(extracted_file).unwrap(),
            "Hello, World!"
        );
    }

    #[test]
    fn test_extract_zip() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("test.zip");
        let extract_dir = temp_dir.path().join("extracted");

        // Create a simple zip archive
        let file = File::create(&archive_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("test.txt", options).unwrap();
        zip.write_all(b"Hello from zip!").unwrap();
        zip.finish().unwrap();

        // Extract it
        std::fs::create_dir_all(&extract_dir).unwrap();
        extract_zip(&archive_path, &extract_dir).unwrap();

        // Verify
        let extracted_file = extract_dir.join("test.txt");
        assert!(extracted_file.exists());
        assert_eq!(
            std::fs::read_to_string(extracted_file).unwrap(),
            "Hello from zip!"
        );
    }

    #[test]
    fn test_extract_tar_with_nested_dirs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("nested.tar.gz");
        let extract_dir = temp_dir.path().join("extracted");

        // Create a tar.gz with nested directory structure
        let file = File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        // Add files in nested directories
        let content = b"nested content";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "foo/bar/baz.txt", &content[..])
            .unwrap();

        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        // Extract
        std::fs::create_dir_all(&extract_dir).unwrap();
        extract_tar_gz(&archive_path, &extract_dir).unwrap();

        // Verify nested structure was created
        let extracted_file = extract_dir.join("foo/bar/baz.txt");
        assert!(extracted_file.exists());
        assert_eq!(
            std::fs::read_to_string(extracted_file).unwrap(),
            "nested content"
        );
    }

    #[test]
    fn test_extract_tar_blocks_symlink_escape() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("escape.tar.gz");
        let extract_dir = temp_dir.path().join("extracted");

        let file = File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        // Create a symlink "a" -> "/" then attempt to write "a/evil.txt".
        let mut link_header = tar::Header::new_gnu();
        link_header.set_entry_type(tar::EntryType::Symlink);
        link_header.set_size(0);
        link_header.set_mode(0o777);
        link_header.set_cksum();
        link_header.set_link_name("/").unwrap();
        builder
            .append_data(&mut link_header, "a", std::io::empty())
            .unwrap();

        let content = b"pwned";
        let mut file_header = tar::Header::new_gnu();
        file_header.set_size(content.len() as u64);
        file_header.set_mode(0o644);
        file_header.set_cksum();
        builder
            .append_data(&mut file_header, "a/evil.txt", &content[..])
            .unwrap();

        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        std::fs::create_dir_all(&extract_dir).unwrap();
        let err = extract_tar_gz(&archive_path, &extract_dir).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unsafe link target") || msg.contains("symlink"),
            "expected link/symlink safety error, got: {msg}"
        );
        assert!(!extract_dir.join("a/evil.txt").exists());
    }

    #[test]
    fn test_extract_tar_blocks_hardlink_outside_dest() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("hardlink.tar.gz");
        let extract_dir = temp_dir.path().join("extracted");

        let file = File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Link);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        header.set_link_name("/etc/passwd").unwrap();
        builder
            .append_data(&mut header, "hl", std::io::empty())
            .unwrap();

        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        std::fs::create_dir_all(&extract_dir).unwrap();
        let err = extract_tar_gz(&archive_path, &extract_dir).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unsafe link target"),
            "expected unsafe link target error, got: {msg}"
        );
    }

    #[test]
    fn test_extract_zip_with_nested_dirs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("nested.zip");
        let extract_dir = temp_dir.path().join("extracted");

        // Create a zip with nested directory structure
        let file = File::create(&archive_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();

        // Add directory entry
        zip.add_directory("foo/bar/", options).unwrap();
        zip.start_file("foo/bar/baz.txt", options).unwrap();
        zip.write_all(b"nested zip content").unwrap();
        zip.finish().unwrap();

        // Extract
        std::fs::create_dir_all(&extract_dir).unwrap();
        extract_zip(&archive_path, &extract_dir).unwrap();

        // Verify
        let extracted_file = extract_dir.join("foo/bar/baz.txt");
        assert!(extracted_file.exists());
        assert_eq!(
            std::fs::read_to_string(extracted_file).unwrap(),
            "nested zip content"
        );
    }
}
