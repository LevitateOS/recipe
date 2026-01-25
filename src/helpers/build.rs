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
//!     shell_in(src_dir, "./configure --prefix=" + PREFIX);
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
use std::path::Path;
use std::time::Duration;

// ============================================================================
// Native archive extraction (no external tools needed)
// ============================================================================

/// Extract a tar archive with optional decompression
fn extract_tar<R: Read>(reader: R, dest: &Path) -> Result<(), Box<EvalAltResult>> {
    let mut archive = tar::Archive::new(reader);

    // Unpack with security checks
    for entry in archive.entries().map_err(|e| format!("tar read error: {}", e))? {
        let mut entry = entry.map_err(|e| format!("tar entry error: {}", e))?;

        // Get the path and check for path traversal (clone to avoid borrow issues)
        let path = entry
            .path()
            .map_err(|e| format!("tar path error: {}", e))?
            .into_owned();

        // Security: reject paths that could escape the destination
        if path.is_absolute() || path.components().any(|c| c == std::path::Component::ParentDir) {
            return Err(format!("tar contains unsafe path: {}", path.display()).into());
        }

        let full_path = dest.join(&path);

        // Create parent directories
        if let Some(parent) = full_path.parent() {
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

    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("zip read error: {}", e))?;

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
        builder.append_data(&mut header, "test.txt", &content[..]).unwrap();

        // Properly finish and close the archive
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        // Extract it
        std::fs::create_dir_all(&extract_dir).unwrap();
        extract_tar_gz(&archive_path, &extract_dir).unwrap();

        // Verify
        let extracted_file = extract_dir.join("test.txt");
        assert!(extracted_file.exists());
        assert_eq!(std::fs::read_to_string(extracted_file).unwrap(), "Hello, World!");
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
        assert_eq!(std::fs::read_to_string(extracted_file).unwrap(), "Hello from zip!");
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
        builder.append_data(&mut header, "foo/bar/baz.txt", &content[..]).unwrap();

        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        // Extract
        std::fs::create_dir_all(&extract_dir).unwrap();
        extract_tar_gz(&archive_path, &extract_dir).unwrap();

        // Verify nested structure was created
        let extracted_file = extract_dir.join("foo/bar/baz.txt");
        assert!(extracted_file.exists());
        assert_eq!(std::fs::read_to_string(extracted_file).unwrap(), "nested content");
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
        assert_eq!(std::fs::read_to_string(extracted_file).unwrap(), "nested zip content");
    }
}
