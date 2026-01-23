//! Install phase helpers
//!
//! Simple helpers for copying outputs to PREFIX. These helpers:
//! - Enforce FHS directory structure (bin, lib, share/man)
//! - Track installed files for clean removal
//! - Set appropriate permissions
//!
//! For anything more complex, recipes should use `run()` directly.

use crate::core::{output, record_installed_file, with_context};
use rhai::EvalAltResult;
use std::path::Path;
use std::process::Command;

/// Install files to PREFIX/bin with executable permissions (0o755).
///
/// Tracks installed files for clean removal.
///
/// # Example
/// ```rhai
/// install_bin("myapp");       // Installs to PREFIX/bin/myapp
/// install_bin("target/*");    // Glob patterns work
/// ```
pub fn install_bin(pattern: &str) -> Result<(), Box<EvalAltResult>> {
    do_install(pattern, "bin", Some(0o755))
}

/// Install files to PREFIX/lib with standard permissions (0o644).
///
/// Tracks installed files for clean removal.
///
/// # Example
/// ```rhai
/// install_lib("libfoo.so");
/// install_lib("*.a");
/// ```
pub fn install_lib(pattern: &str) -> Result<(), Box<EvalAltResult>> {
    do_install(pattern, "lib", Some(0o644))
}

/// Install man pages to PREFIX/share/man/man{N}/.
///
/// Determines section from file extension (e.g., `foo.1` → `man1/`).
/// Tracks installed files for clean removal.
///
/// # Example
/// ```rhai
/// install_man("doc/*.1");  // Section 1 man pages
/// install_man("foo.5");    // Section 5 config man page
/// ```
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

/// Install files to a custom subdirectory of PREFIX (without mode change).
///
/// Generic install function for advanced use cases. Use install_bin/install_lib
/// for standard locations.
///
/// # Arguments
/// * `pattern` - Glob pattern for files to install
/// * `subdir` - Subdirectory under PREFIX (e.g., "share/doc")
///
/// # Example
/// ```rhai
/// install_to_dir("docs/*", "share/doc/myapp");  // No mode change
/// ```
pub fn install_to_dir(pattern: &str, subdir: &str) -> Result<(), Box<EvalAltResult>> {
    install_to_dir_with_mode(pattern, subdir, None)
}

/// Install files to a custom subdirectory of PREFIX with optional mode.
///
/// Generic install function for advanced use cases. Use install_bin/install_lib
/// for standard locations.
///
/// # Arguments
/// * `pattern` - Glob pattern for files to install
/// * `subdir` - Subdirectory under PREFIX (e.g., "share/doc")
/// * `mode` - Unix permissions (e.g., 0o755)
///
/// # Example
/// ```rhai
/// install_to_dir("scripts/*", "libexec", 0o755);  // Executable
/// ```
pub fn install_to_dir_with_mode(pattern: &str, subdir: &str, mode: Option<u32>) -> Result<(), Box<EvalAltResult>> {
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

/// Install files to a custom subdirectory of PREFIX with mode (Rhai i64 wrapper).
///
/// This is an overload for Rhai's i64 integer type.
pub fn install_to_dir_i64(pattern: &str, subdir: &str, mode: i64) -> Result<(), Box<EvalAltResult>> {
    install_to_dir_with_mode(pattern, subdir, Some(mode as u32))
}

/// Extract RPM contents to PREFIX
///
/// Finds all .rpm files in the build directory and extracts them to PREFIX.
/// Tracks all extracted files for clean removal.
///
/// Requires `rpm2cpio` and `cpio` to be available in PATH.
///
/// # Example
/// ```rhai
/// fn install() {
///     rpm_install();  // Extracts all RPMs in build_dir to PREFIX
/// }
/// ```
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

/// Validate that a path is within the PREFIX directory.
/// Prevents path traversal attacks via symlinks or .. components.
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

/// Internal helper - install files to a subdirectory of PREFIX.
/// Not exposed to recipes; use install_bin(), install_lib(), or run() instead.
fn do_install(pattern: &str, subdir: &str, mode: Option<u32>) -> Result<(), Box<EvalAltResult>> {
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

        let mut installed = Vec::new();
        for src in matches {
            let filename = src.file_name().ok_or("invalid filename")?;
            let dest = dest_dir.join(filename);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{clear_context, get_installed_files, init_context};
    use leviso_cheat_test::{cheat_aware, cheat_reviewed};
    use tempfile::TempDir;

    fn setup_context() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let prefix = dir.path().join("prefix");
        let build_dir = dir.path().join("build");
        std::fs::create_dir_all(&prefix).unwrap();
        std::fs::create_dir_all(&build_dir).unwrap();
        init_context(prefix.clone(), build_dir.clone());
        (dir, prefix, build_dir)
    }

    // ==================== install_bin tests ====================

    #[cheat_aware(
        protects = "User's compiled binaries are installed to bin directory",
        severity = "HIGH",
        ease = "MEDIUM",
        cheats = [
            "Skip the copy operation entirely",
            "Copy to wrong directory",
            "Report success without checking destination"
        ],
        consequence = "User runs 'recipe install myapp' but binary is not in PATH - command not found"
    )]
    #[test]
    fn test_install_bin() {
        let (_dir, prefix, build_dir) = setup_context();
        std::fs::write(build_dir.join("mybin"), "binary").unwrap();

        let result = install_bin("mybin");
        assert!(result.is_ok());
        assert!(prefix.join("bin/mybin").exists());

        clear_context();
    }

    #[cheat_reviewed("Error handling - no context returns error")]
    #[test]
    fn test_install_bin_no_context() {
        clear_context();
        let result = install_bin("*.txt");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No execution context"));
    }

    #[cheat_reviewed("Error handling - nonexistent files return error")]
    #[test]
    fn test_install_bin_no_matching_files() {
        let (_dir, _prefix, _build_dir) = setup_context();
        let result = install_bin("nonexistent*");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no files match"));
        clear_context();
    }

    #[cheat_aware(
        protects = "User's installed files are tracked for removal",
        severity = "HIGH",
        ease = "EASY",
        cheats = [
            "Copy file but don't record it",
            "Record wrong path",
            "Record before copy (might fail)"
        ],
        consequence = "User runs 'recipe remove myapp' but files remain on disk"
    )]
    #[test]
    fn test_install_bin_copies_file() {
        let (_dir, prefix, build_dir) = setup_context();
        std::fs::write(build_dir.join("test-binary"), "binary content").unwrap();

        let result = install_bin("test-binary");
        assert!(result.is_ok());
        assert!(prefix.join("bin/test-binary").exists());

        let files = get_installed_files();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("test-binary"));

        clear_context();
    }

    #[cheat_aware(
        protects = "User can install multiple binaries with glob pattern",
        severity = "MEDIUM",
        ease = "MEDIUM",
        cheats = [
            "Only install first match",
            "Skip glob expansion and require exact name",
            "Install all files instead of just matching"
        ],
        consequence = "User runs 'install_bin(\"cmd*\")' expecting 5 tools but only gets 1"
    )]
    #[test]
    fn test_install_bin_with_glob_pattern() {
        let (_dir, prefix, build_dir) = setup_context();

        std::fs::write(build_dir.join("cmd1"), "content1").unwrap();
        std::fs::write(build_dir.join("cmd2"), "content2").unwrap();
        std::fs::write(build_dir.join("other.txt"), "other").unwrap();

        let result = install_bin("cmd*");
        assert!(result.is_ok());

        assert!(prefix.join("bin/cmd1").exists());
        assert!(prefix.join("bin/cmd2").exists());
        assert!(!prefix.join("bin/other.txt").exists());

        let files = get_installed_files();
        assert_eq!(files.len(), 2);

        clear_context();
    }

    #[cheat_aware(
        protects = "User's binaries are executable after installation",
        severity = "CRITICAL",
        ease = "EASY",
        cheats = [
            "Copy file but skip chmod",
            "Set wrong permissions (644 instead of 755)",
            "Check mode but not actually set it"
        ],
        consequence = "User runs './myapp' but gets 'Permission denied'"
    )]
    #[test]
    #[cfg(unix)]
    fn test_install_bin_sets_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let (_dir, prefix, build_dir) = setup_context();
        std::fs::write(build_dir.join("script"), "#!/bin/bash").unwrap();

        let result = install_bin("script");
        assert!(result.is_ok());

        let dest = prefix.join("bin/script");
        let perms = std::fs::metadata(&dest).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o755);

        clear_context();
    }

    // ==================== install_lib tests ====================

    #[cheat_aware(
        protects = "User's libraries are installed to lib directory",
        severity = "HIGH",
        ease = "MEDIUM",
        cheats = [
            "Install to bin instead of lib",
            "Skip library installation entirely",
            "Install but with wrong permissions"
        ],
        consequence = "User's application fails to start: 'libfoo.so: cannot open shared object file'"
    )]
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

    #[cheat_aware(
        protects = "User can read man pages for installed commands",
        severity = "MEDIUM",
        ease = "MEDIUM",
        cheats = [
            "Install to wrong man section",
            "Skip man page installation",
            "Install but don't follow FHS structure"
        ],
        consequence = "User runs 'man myapp' but gets 'No manual entry for myapp'"
    )]
    #[test]
    fn test_install_man_section_1() {
        let (_dir, prefix, build_dir) = setup_context();
        std::fs::write(build_dir.join("myapp.1"), "man page content").unwrap();

        let result = install_man("myapp.1");
        assert!(result.is_ok());
        assert!(prefix.join("share/man/man1/myapp.1").exists());

        clear_context();
    }

    #[cheat_reviewed("Man section detection - section 5 config pages")]
    #[test]
    fn test_install_man_section_5() {
        let (_dir, prefix, build_dir) = setup_context();
        std::fs::write(build_dir.join("config.5"), "config man page").unwrap();

        let result = install_man("config.5");
        assert!(result.is_ok());
        assert!(prefix.join("share/man/man5/config.5").exists());

        clear_context();
    }

    #[cheat_reviewed("Man section fallback - unknown extension defaults to man1")]
    #[test]
    fn test_install_man_no_section_defaults_to_1() {
        let (_dir, prefix, build_dir) = setup_context();
        std::fs::write(build_dir.join("weird.man"), "man content").unwrap();

        let result = install_man("weird.man");
        assert!(result.is_ok());
        assert!(prefix.join("share/man/man1/weird.man").exists());

        clear_context();
    }

    #[cheat_reviewed("Glob pattern support for man pages")]
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

    // ==================== Edge cases ====================

    #[cheat_aware(
        protects = "User's package upgrades replace old version",
        severity = "HIGH",
        ease = "EASY",
        cheats = [
            "Fail silently when file exists",
            "Skip overwrite and keep old version",
            "Append instead of replace"
        ],
        consequence = "User upgrades package but keeps running old buggy version"
    )]
    #[test]
    fn test_install_overwrites_existing_file() {
        let (_dir, prefix, build_dir) = setup_context();

        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::write(prefix.join("bin/file"), "old content").unwrap();
        std::fs::write(build_dir.join("file"), "new content").unwrap();

        let result = install_bin("file");
        assert!(result.is_ok());

        let content = std::fs::read_to_string(prefix.join("bin/file")).unwrap();
        assert_eq!(content, "new content");

        clear_context();
    }

    #[cheat_aware(
        protects = "User's installed files are byte-for-byte identical to source",
        severity = "HIGH",
        ease = "MEDIUM",
        cheats = [
            "Truncate file during copy",
            "Transform content during copy",
            "Write placeholder instead of actual content"
        ],
        consequence = "User's binary is corrupt and crashes on launch"
    )]
    #[test]
    fn test_install_preserves_file_content() {
        let (_dir, prefix, build_dir) = setup_context();

        let original = "This is the original content\nWith multiple lines\n";
        std::fs::write(build_dir.join("file"), original).unwrap();

        let result = install_lib("file");
        assert!(result.is_ok());

        let copied = std::fs::read_to_string(prefix.join("lib/file")).unwrap();
        assert_eq!(copied, original);

        clear_context();
    }

    // ==================== install_to_dir tests ====================

    #[cheat_reviewed("Nested directory creation - creates parent dirs")]
    #[test]
    fn test_install_creates_nested_directories() {
        let (_dir, prefix, build_dir) = setup_context();
        std::fs::write(build_dir.join("file"), "content").unwrap();

        // Install to deeply nested path
        let result = install_to_dir_with_mode("file", "a/b/c/d", None);
        assert!(result.is_ok());
        assert!(prefix.join("a/b/c/d/file").exists());

        clear_context();
    }

    // ==================== rpm_install tests ====================

    #[cheat_reviewed("Error handling - no RPMs returns error")]
    #[test]
    fn test_rpm_install_no_rpms() {
        let (_dir, _prefix, _build_dir) = setup_context();
        // No RPM files in build_dir
        let result = rpm_install();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no RPM files"));
        clear_context();
    }
}
