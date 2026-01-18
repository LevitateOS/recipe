//! Install phase helpers
//!
//! Copy outputs to PREFIX: install_bin, install_lib, install_man, rpm_install

use crate::engine::context::with_context;
use rhai::EvalAltResult;
use std::process::Command;

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
    with_context(|ctx| {
        let full_pattern = ctx.current_dir.join(pattern);
        let matches: Vec<_> = glob::glob(&full_pattern.to_string_lossy())
            .map_err(|e| format!("invalid pattern: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        if matches.is_empty() {
            return Err(format!("no files match pattern: {}", pattern).into());
        }

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
            println!("     install {} -> {}", src.display(), dest.display());
            std::fs::copy(&src, &dest).map_err(|e| format!("install failed: {}", e))?;
        }

        Ok(())
    })
}

/// Install files to a specific subdirectory of PREFIX
pub fn install_to_dir(pattern: &str, subdir: &str, mode: Option<u32>) -> Result<(), Box<EvalAltResult>> {
    with_context(|ctx| {
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

        for src in matches {
            let filename = src.file_name().ok_or("invalid filename")?;
            let dest = dest_dir.join(filename);
            println!("     install {} -> {}", src.display(), dest.display());
            std::fs::copy(&src, &dest).map_err(|e| format!("install failed: {}", e))?;

            #[cfg(unix)]
            if let Some(m) = mode {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(m))
                    .map_err(|e| format!("chmod failed: {}", e))?;
            }
        }

        Ok(())
    })
}

/// Extract RPM contents to PREFIX
pub fn rpm_install() -> Result<(), Box<EvalAltResult>> {
    with_context(|ctx| {
        // Find RPM files in build_dir
        let pattern = ctx.build_dir.join("*.rpm");
        let matches: Vec<_> = glob::glob(&pattern.to_string_lossy())
            .map_err(|e| format!("invalid pattern: {}", e))?
            .filter_map(|r| r.ok())
            .collect();

        if matches.is_empty() {
            return Err("no RPM files found in build directory".to_string().into());
        }

        for rpm in matches {
            println!("     rpm_install {}", rpm.display());

            // Extract RPM contents to prefix using rpm2cpio
            let status = Command::new("sh")
                .args([
                    "-c",
                    &format!(
                        "rpm2cpio '{}' | cpio -idmv -D '{}'",
                        rpm.display(),
                        ctx.prefix.display()
                    ),
                ])
                .current_dir(&ctx.build_dir)
                .status()
                .map_err(|e| format!("rpm2cpio failed: {}", e))?;

            if !status.success() {
                return Err(format!("rpm_install failed for {}", rpm.display()).into());
            }
        }

        Ok(())
    })
}
