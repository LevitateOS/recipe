//! Install phase helpers
//!
//! Copy outputs to PREFIX: install_bin, install_lib, install_man, rpm_install

use crate::engine::context::{record_installed_file, with_context};
use crate::engine::output;
use rhai::EvalAltResult;
use std::path::Path;
use std::process::Command;

/// Validate that a path is within the given prefix directory.
/// This prevents path traversal attacks via symlinks or relative paths.
fn validate_path_within_prefix(path: &Path, prefix: &Path) -> Result<(), String> {
    // Canonicalize both paths to resolve symlinks and .. components
    let canonical_prefix = prefix.canonicalize()
        .map_err(|e| format!("Failed to canonicalize prefix '{}': {}", prefix.display(), e))?;

    // For the target path, we need to handle the case where it doesn't exist yet
    // We canonicalize the parent directory and append the filename
    let canonical_path = if path.exists() {
        path.canonicalize()
            .map_err(|e| format!("Failed to canonicalize path '{}': {}", path.display(), e))?
    } else {
        // Path doesn't exist yet - canonicalize parent and append filename
        let parent = path.parent().ok_or_else(|| "Path has no parent".to_string())?;
        let filename = path.file_name().ok_or_else(|| "Path has no filename".to_string())?;
        let canonical_parent = parent.canonicalize()
            .map_err(|e| format!("Failed to canonicalize parent '{}': {}", parent.display(), e))?;
        canonical_parent.join(filename)
    };

    // Check if the canonical path starts with the canonical prefix
    if !canonical_path.starts_with(&canonical_prefix) {
        return Err(format!(
            "Path traversal detected: '{}' is outside prefix '{}'",
            path.display(),
            prefix.display()
        ));
    }

    Ok(())
}

/// Install files to PREFIX/bin with executable permissions
pub fn install_bin(pattern: &str) -> Result<(), Box<EvalAltResult>> {
    install_to_dir(pattern, "bin", Some(0o755))
}

/// Install files to PREFIX/lib with standard permissions
pub fn install_lib(pattern: &str) -> Result<(), Box<EvalAltResult>> {
    install_to_dir(pattern, "lib", Some(0o644))
}

/// Install man pages to PREFIX/share/man/man{N}/
pub fn install_man(pattern: &str) -> Result<(), Box<EvalAltResult>> {
    let installed_paths = with_context(|ctx| {
        let full_pattern = ctx.current_dir.join(pattern);
        let matches: Vec<_> = glob::glob(&full_pattern.to_string_lossy())
            .map_err(|e| format!("invalid pattern: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        if matches.is_empty() {
            return Err(format!("no files match pattern: {}", pattern).into());
        }

        let mut installed = Vec::new();
        for src in matches {
            let filename = src.file_name().ok_or("invalid filename")?;
            let filename_str = filename.to_string_lossy();

            // Determine man section from extension (e.g., rg.1 -> man1)
            let section = filename_str
                .rsplit('.')
                .next()
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(1);

            let man_dir = ctx.prefix.join(format!("share/man/man{}", section));
            std::fs::create_dir_all(&man_dir).map_err(|e| format!("cannot create dir: {}", e))?;

            let dest = man_dir.join(filename);

            // Validate destination path is within PREFIX
            validate_path_within_prefix(&dest, &ctx.prefix)?;

            output::detail(&format!("install {} -> {}", src.display(), dest.display()));
            std::fs::copy(&src, &dest).map_err(|e| format!("install failed: {}", e))?;
            installed.push(dest);
        }

        Ok(installed)
    })?;

    // Record installed files in context
    for path in installed_paths {
        record_installed_file(path);
    }

    Ok(())
}

/// Install files to a specific subdirectory of PREFIX
pub fn install_to_dir(pattern: &str, subdir: &str, mode: Option<u32>) -> Result<(), Box<EvalAltResult>> {
    let installed_paths = with_context(|ctx| {
        let full_pattern = ctx.current_dir.join(pattern);
        let matches: Vec<_> = glob::glob(&full_pattern.to_string_lossy())
            .map_err(|e| format!("invalid pattern: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        if matches.is_empty() {
            return Err(format!("no files match pattern: {}", pattern).into());
        }

        let dest_dir = ctx.prefix.join(subdir);
        std::fs::create_dir_all(&dest_dir).map_err(|e| format!("cannot create dir: {}", e))?;

        // Validate that dest_dir is within PREFIX (prevents path traversal via subdir)
        validate_path_within_prefix(&dest_dir, &ctx.prefix)?;

        let mut installed = Vec::new();
        for src in matches {
            let filename = src.file_name().ok_or("invalid filename")?;
            let dest = dest_dir.join(filename);

            // Validate each destination path (catches symlink-based traversal)
            validate_path_within_prefix(&dest, &ctx.prefix)?;

            output::detail(&format!("install {} -> {}", src.display(), dest.display()));
            std::fs::copy(&src, &dest).map_err(|e| format!("install failed: {}", e))?;

            #[cfg(unix)]
            if let Some(m) = mode {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(m))
                    .map_err(|e| format!("chmod failed: {}", e))?;
            }

            installed.push(dest);
        }

        Ok(installed)
    })?;

    // Record installed files in context
    for path in installed_paths {
        record_installed_file(path);
    }

    Ok(())
}

/// Extract RPM contents to PREFIX
pub fn rpm_install() -> Result<(), Box<EvalAltResult>> {
    let installed_paths = with_context(|ctx| {
        // Find RPM files in build_dir
        let pattern = ctx.build_dir.join("*.rpm");
        let matches: Vec<_> = glob::glob(&pattern.to_string_lossy())
            .map_err(|e| format!("invalid pattern: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        if matches.is_empty() {
            return Err("no RPM files found in build directory".to_string().into());
        }

        let mut installed = Vec::new();
        for rpm in &matches {
            output::detail(&format!("rpm_install {}", rpm.display()));

            // Extract RPM contents to prefix using rpm2cpio, capturing file list
            let output = Command::new("sh")
                .args([
                    "-c",
                    &format!(
                        "rpm2cpio '{}' | cpio -idmv -D '{}' 2>&1",
                        rpm.display(),
                        ctx.prefix.display()
                    ),
                ])
                .current_dir(&ctx.build_dir)
                .output()
                .map_err(|e| format!("rpm2cpio failed: {}", e))?;

            if !output.status.success() {
                return Err(format!("rpm_install failed for {}", rpm.display()).into());
            }

            // Parse cpio verbose output for installed files
            let output_str = String::from_utf8_lossy(&output.stdout);
            for line in output_str.lines() {
                let line = line.trim();
                if !line.is_empty() && !line.starts_with('.') {
                    // cpio outputs relative paths, make them absolute
                    let file_path = ctx.prefix.join(line.trim_start_matches("./"));
                    // Track both files and symlinks (symlinks are important, e.g., /bin/sh)
                    if file_path.is_file() || file_path.is_symlink() {
                        // Validate each installed file is within PREFIX
                        // Note: cpio already extracts to prefix, but RPM could contain
                        // paths with .. or symlinks that escape
                        if let Err(e) = validate_path_within_prefix(&file_path, &ctx.prefix) {
                            output::warning(&e);
                            continue;
                        }
                        installed.push(file_path);
                    }
                }
            }
        }

        Ok(installed)
    })?;

    // Record installed files in context
    for path in installed_paths {
        record_installed_file(path);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::context::{clear_context, get_installed_files, init_context_with_recipe};
    use tempfile::TempDir;

    fn setup_context() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let prefix = dir.path().join("prefix");
        let build_dir = dir.path().join("build");
        std::fs::create_dir_all(&prefix).unwrap();
        std::fs::create_dir_all(&build_dir).unwrap();
        init_context_with_recipe(prefix.clone(), build_dir.clone(), None);
        (dir, prefix, build_dir)
    }

    // ==================== install_to_dir tests ====================

    #[test]
    fn test_install_to_dir_no_context() {
        clear_context();
        let result = install_to_dir("*.txt", "bin", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No execution context"));
    }

    #[test]
    fn test_install_to_dir_no_matching_files() {
        let (_dir, _prefix, _build_dir) = setup_context();
        let result = install_to_dir("nonexistent*.txt", "bin", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no files match"));
        clear_context();
    }

    #[test]
    fn test_install_to_dir_copies_file() {
        let (_dir, prefix, build_dir) = setup_context();

        // Create a file in build_dir
        std::fs::write(build_dir.join("test-binary"), "binary content").unwrap();

        let result = install_to_dir("test-binary", "bin", None);
        assert!(result.is_ok());

        // File should exist in prefix/bin
        assert!(prefix.join("bin/test-binary").exists());

        // Should be recorded in installed files
        let files = get_installed_files();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("test-binary"));

        clear_context();
    }

    #[test]
    fn test_install_to_dir_with_glob_pattern() {
        let (_dir, prefix, build_dir) = setup_context();

        // Create multiple files
        std::fs::write(build_dir.join("file1.txt"), "content1").unwrap();
        std::fs::write(build_dir.join("file2.txt"), "content2").unwrap();
        std::fs::write(build_dir.join("other.dat"), "other").unwrap();

        let result = install_to_dir("*.txt", "data", None);
        assert!(result.is_ok());

        // Both .txt files should be installed
        assert!(prefix.join("data/file1.txt").exists());
        assert!(prefix.join("data/file2.txt").exists());
        assert!(!prefix.join("data/other.dat").exists());

        let files = get_installed_files();
        assert_eq!(files.len(), 2);

        clear_context();
    }

    #[test]
    #[cfg(unix)]
    fn test_install_to_dir_sets_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, prefix, build_dir) = setup_context();
        std::fs::write(build_dir.join("script"), "#!/bin/bash").unwrap();

        let result = install_to_dir("script", "bin", Some(0o755));
        assert!(result.is_ok());

        let dest = prefix.join("bin/script");
        let perms = std::fs::metadata(&dest).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o755);

        clear_context();
    }

    // ==================== install_bin tests ====================

    #[test]
    fn test_install_bin() {
        let (_dir, prefix, build_dir) = setup_context();
        std::fs::write(build_dir.join("mybin"), "binary").unwrap();

        let result = install_bin("mybin");
        assert!(result.is_ok());
        assert!(prefix.join("bin/mybin").exists());

        clear_context();
    }

    // ==================== install_lib tests ====================

    #[test]
    fn test_install_lib() {
        let (_dir, prefix, build_dir) = setup_context();
        std::fs::write(build_dir.join("libtest.so"), "library").unwrap();

        let result = install_lib("libtest.so");
        assert!(result.is_ok());
        assert!(prefix.join("lib/libtest.so").exists());

        clear_context();
    }

    // ==================== install_man tests ====================

    #[test]
    fn test_install_man_section_1() {
        let (_dir, prefix, build_dir) = setup_context();
        std::fs::write(build_dir.join("myapp.1"), "man page content").unwrap();

        let result = install_man("myapp.1");
        assert!(result.is_ok());
        assert!(prefix.join("share/man/man1/myapp.1").exists());

        clear_context();
    }

    #[test]
    fn test_install_man_section_5() {
        let (_dir, prefix, build_dir) = setup_context();
        std::fs::write(build_dir.join("config.5"), "config man page").unwrap();

        let result = install_man("config.5");
        assert!(result.is_ok());
        assert!(prefix.join("share/man/man5/config.5").exists());

        clear_context();
    }

    #[test]
    fn test_install_man_no_section_defaults_to_1() {
        let (_dir, prefix, build_dir) = setup_context();
        // File without numeric extension
        std::fs::write(build_dir.join("weird.man"), "man content").unwrap();

        let result = install_man("weird.man");
        assert!(result.is_ok());
        // Should default to man1
        assert!(prefix.join("share/man/man1/weird.man").exists());

        clear_context();
    }

    #[test]
    fn test_install_man_multiple_files() {
        let (_dir, prefix, build_dir) = setup_context();
        std::fs::write(build_dir.join("cmd.1"), "cmd man").unwrap();
        std::fs::write(build_dir.join("other.1"), "other man").unwrap();

        let result = install_man("*.1");
        assert!(result.is_ok());
        assert!(prefix.join("share/man/man1/cmd.1").exists());
        assert!(prefix.join("share/man/man1/other.1").exists());

        clear_context();
    }

    // ==================== rpm_install tests ====================

    #[test]
    fn test_rpm_install_no_rpms() {
        let (_dir, _prefix, _build_dir) = setup_context();
        // No RPM files in build_dir
        let result = rpm_install();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no RPM files"));
        clear_context();
    }

    // Note: Full rpm_install testing requires rpm2cpio which may not be available
    // in all test environments. Additional integration tests could be added
    // in a separate test file that's conditionally compiled.

    // ==================== Edge cases ====================

    #[test]
    fn test_install_creates_nested_directories() {
        let (_dir, prefix, build_dir) = setup_context();
        std::fs::write(build_dir.join("file"), "content").unwrap();

        // Install to deeply nested path
        let result = install_to_dir("file", "a/b/c/d", None);
        assert!(result.is_ok());
        assert!(prefix.join("a/b/c/d/file").exists());

        clear_context();
    }

    #[test]
    fn test_install_overwrites_existing_file() {
        let (_dir, prefix, build_dir) = setup_context();

        // Create existing file in prefix
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::write(prefix.join("bin/file"), "old content").unwrap();

        // Create new file in build_dir
        std::fs::write(build_dir.join("file"), "new content").unwrap();

        let result = install_to_dir("file", "bin", None);
        assert!(result.is_ok());

        // File should have new content
        let content = std::fs::read_to_string(prefix.join("bin/file")).unwrap();
        assert_eq!(content, "new content");

        clear_context();
    }

    #[test]
    fn test_install_preserves_file_content() {
        let (_dir, prefix, build_dir) = setup_context();

        let original = "This is the original content\nWith multiple lines\n";
        std::fs::write(build_dir.join("file"), original).unwrap();

        let result = install_to_dir("file", "data", None);
        assert!(result.is_ok());

        let copied = std::fs::read_to_string(prefix.join("data/file")).unwrap();
        assert_eq!(copied, original);

        clear_context();
    }
}
