//! Install phase - installs files to their destinations.

use crate::{DesktopSpec, InstallFile, Shell};

use super::context::Context;
use super::error::ExecuteError;
use super::util::{expand_vars, run_cmd, shell_quote};

/// Execute the install phase.
pub fn install(
    ctx: &Context,
    spec: &crate::InstallSpec,
) -> Result<(), ExecuteError> {
    for file in &spec.files {
        install_file(ctx, file)?;
    }
    Ok(())
}

fn install_file(ctx: &Context, file: &InstallFile) -> Result<(), ExecuteError> {
    let cmd = match file {
        InstallFile::ToBin { src, dest, mode } => {
            let src_path = ctx.build_dir.join(src);
            let dest_name = dest
                .as_ref()
                .map(|d| d.as_str())
                .unwrap_or_else(|| src.rsplit('/').next().unwrap_or(src));
            let dest_path = ctx.prefix.join("bin").join(dest_name);
            let mode = mode.unwrap_or(0o755);

            format!(
                "install -Dm{:o} {} {}",
                mode,
                shell_quote(src_path.display()),
                shell_quote(dest_path.display())
            )
        }
        InstallFile::ToLib { src, dest } => {
            let src_path = ctx.build_dir.join(src);
            let dest_name = dest
                .as_ref()
                .map(|d| d.as_str())
                .unwrap_or_else(|| src.rsplit('/').next().unwrap_or(src));
            let dest_path = ctx.prefix.join("lib").join(dest_name);

            format!(
                "install -Dm644 {} {}",
                shell_quote(src_path.display()),
                shell_quote(dest_path.display())
            )
        }
        InstallFile::ToConfig { src, dest, mode } => {
            let src_path = ctx.build_dir.join(src);
            let dest_path = std::path::PathBuf::from(dest);
            let mode = mode.unwrap_or(0o644);

            format!(
                "install -Dm{:o} {} {}",
                mode,
                shell_quote(src_path.display()),
                shell_quote(dest_path.display())
            )
        }
        InstallFile::ToMan { src } => {
            let src_path = ctx.build_dir.join(src);
            // Detect man section from filename (e.g., "rg.1" -> "man1")
            let section = src
                .rsplit('.')
                .next()
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(1);
            let dest_dir = ctx
                .prefix
                .join("share")
                .join("man")
                .join(format!("man{}", section));

            format!(
                "install -Dm644 {} {}/",
                shell_quote(src_path.display()),
                shell_quote(dest_dir.display())
            )
        }
        InstallFile::ToShare { src, dest } => {
            let src_path = ctx.build_dir.join(src);
            let dest_path = ctx.prefix.join("share").join(dest);

            format!(
                "install -Dm644 {} {}",
                shell_quote(src_path.display()),
                shell_quote(dest_path.display())
            )
        }
        InstallFile::Link { src, dest } => {
            let src_path = expand_vars(ctx, src);
            let dest_path = expand_vars(ctx, dest);

            format!("ln -sf {} {}", shell_quote(&src_path), shell_quote(&dest_path))
        }
        InstallFile::ToInclude { src, dest } => {
            let src_path = ctx.build_dir.join(src);
            let dest_subdir = dest.as_deref().unwrap_or("");
            let dest_path = ctx.prefix.join("include").join(dest_subdir);

            // Check if src is a directory or file pattern
            if src_path.is_dir() {
                format!(
                    "mkdir -p {} && cp -r {}/* {}/",
                    shell_quote(dest_path.display()),
                    shell_quote(src_path.display()),
                    shell_quote(dest_path.display())
                )
            } else {
                format!(
                    "install -Dm644 {} {}/",
                    shell_quote(src_path.display()),
                    shell_quote(dest_path.display())
                )
            }
        }
        InstallFile::ToPkgconfig { src } => {
            let src_path = ctx.build_dir.join(src);
            let dest_path = ctx.prefix.join("lib").join("pkgconfig");

            format!(
                "install -Dm644 {} {}/",
                shell_quote(src_path.display()),
                shell_quote(dest_path.display())
            )
        }
        InstallFile::ToCmake { src } => {
            let src_path = ctx.build_dir.join(src);
            let filename = src.rsplit('/').next().unwrap_or(src);
            let dest_path = ctx.prefix.join("lib").join("cmake").join(filename);

            format!(
                "install -Dm644 {} {}",
                shell_quote(src_path.display()),
                shell_quote(dest_path.display())
            )
        }
        InstallFile::ToSystemd { src } => {
            let src_path = ctx.build_dir.join(src);
            let dest_path = ctx.prefix.join("lib").join("systemd").join("system");

            format!(
                "install -Dm644 {} {}/",
                shell_quote(src_path.display()),
                shell_quote(dest_path.display())
            )
        }
        InstallFile::ToCompletions { src, shell } => {
            let src_path = ctx.build_dir.join(src);
            let dest_dir = match shell {
                Shell::Bash => ctx.prefix.join("share").join("bash-completion").join("completions"),
                Shell::Zsh => ctx.prefix.join("share").join("zsh").join("site-functions"),
                Shell::Fish => ctx.prefix.join("share").join("fish").join("vendor_completions.d"),
            };

            format!(
                "install -Dm644 {} {}/",
                shell_quote(src_path.display()),
                shell_quote(dest_dir.display())
            )
        }
    };

    run_cmd(ctx, &cmd)?;
    Ok(())
}

/// Install a desktop entry file.
///
/// Generates a .desktop file following the freedesktop.org specification
/// and installs it to $PREFIX/share/applications/{package-name}.desktop
pub fn install_desktop(
    ctx: &Context,
    package_name: &str,
    spec: &DesktopSpec,
    description: Option<&str>,
) -> Result<(), ExecuteError> {
    // Use comment from spec, or fall back to package description
    let comment = spec.comment.as_deref().or(description).unwrap_or("");

    // Ensure categories ends with semicolon if present
    let categories = spec.categories.as_ref().map(|c| {
        if c.ends_with(';') {
            c.clone()
        } else {
            format!("{};", c)
        }
    });

    // Build the .desktop file content
    let mut content = String::new();
    content.push_str("[Desktop Entry]\n");
    content.push_str("Type=Application\n");
    content.push_str(&format!("Name={}\n", spec.name));
    content.push_str(&format!("Exec={}\n", spec.exec));

    if let Some(ref icon) = spec.icon {
        content.push_str(&format!("Icon={}\n", icon));
    }

    if !comment.is_empty() {
        content.push_str(&format!("Comment={}\n", comment));
    }

    if let Some(ref cats) = categories {
        content.push_str(&format!("Categories={}\n", cats));
    }

    content.push_str(&format!("Terminal={}\n", if spec.terminal { "true" } else { "false" }));

    // Destination path
    let dest_dir = ctx.prefix.join("share").join("applications");
    let dest_file = dest_dir.join(format!("{}.desktop", package_name));

    if ctx.dry_run {
        println!("[dry-run] Would create {}", dest_file.display());
        println!("{}", content);
        return Ok(());
    }

    // Create the directory if needed
    std::fs::create_dir_all(&dest_dir)?;

    // Write the .desktop file
    std::fs::write(&dest_file, &content)?;

    // Set permissions (644)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest_file, std::fs::Permissions::from_mode(0o644))?;
    }

    if ctx.verbose {
        println!("Installed desktop entry: {}", dest_file.display());
    }

    Ok(())
}
