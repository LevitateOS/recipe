//! `recipe runtime`: show, fetch and stage the runtime Recipe runs recipes in.
//!
//! `stage` uses plain `std::fs` only, so the static binary can run it inside a
//! Ginux domain.

use anyhow::{Context, Result, bail, ensure};
use levitate_recipe::runtime;
use sha2::{Digest, Sha256};
use std::io::{IsTerminal, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::cli::{Cli, RuntimeCommands};

const PROFILE: &str = include_str!("../../../data/profile.sh");
const APPARMOR: &str = include_str!("../../../data/apparmor-ginux");
const OFFICIAL_SOURCE: &str = "files = \"https://raw.githubusercontent.com/LevitateOS/recipes/{rev}/{path}\"\nbranch = \"master\"\n";

pub(crate) fn execute(command: &RuntimeCommands, cli: &Cli) -> Result<()> {
    match command {
        RuntimeCommands::Status => status(cli),
        RuntimeCommands::Fetch => {
            runtime::set_fetch_events(cli.machine_events);
            let reference = runtime::root_reference()?;
            let path = runtime::ensure_rootfs(&reference)?;
            println!("{}", path.display());
            Ok(())
        }
        RuntimeCommands::Stage {
            root,
            prefix,
            launcher,
            reference,
            image,
            catalog,
            apparmor,
        } => stage(
            root,
            prefix,
            launcher,
            reference,
            image.as_deref(),
            catalog,
            *apparmor,
        ),
    }
}

fn executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

fn status(cli: &Cli) -> Result<()> {
    let reference = runtime::root_reference()?;
    let digest = runtime::root_digest(&reference)?.to_owned();
    let origin = runtime::reference_origin();
    let (launcher, launcher_from) = match std::env::var_os("GINUX_BIN") {
        Some(path) if !path.is_empty() => (PathBuf::from(path), "GINUX_BIN"),
        _ => (runtime::launcher(), "install prefix"),
    };
    let image = runtime::rootfs_path(&reference)?;
    let image_from = if std::env::var_os("GINUX_ROOTFS").is_some() {
        "GINUX_ROOTFS"
    } else if image == runtime::shipped_rootfs() {
        "installed with Recipe"
    } else {
        "download cache"
    };
    let sources = runtime::shipped_sources();
    let result = serde_json::json!({
        "schema_version": 1,
        "reference": reference,
        "reference_from": origin,
        "digest": digest,
        "prefix": runtime::prefix(),
        "launcher": {"path": launcher, "from": launcher_from,
            "exists": launcher.exists(), "executable": executable(&launcher)},
        "image": {"path": image, "from": image_from, "present": image.exists()},
        "shipped_sources": {"path": sources, "exists": sources.exists()},
    });
    let json = cli.json || !std::io::stdout().is_terminal();
    if let Some(path) = &cli.json_output {
        std::fs::write(path, serde_json::to_string(&result)?)
            .with_context(|| format!("cannot write {}", path.display()))?;
        if !json {
            println!("Runtime status saved to {}.", path.display());
        }
    } else if json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        println!("Runtime image: {reference}\n  selected by: {origin}");
        println!(
            "Launcher:      {} ({}; {})",
            launcher.display(),
            launcher_from,
            match (launcher.exists(), executable(&launcher)) {
                (false, _) => "missing",
                (true, false) => "present, not executable",
                (true, true) => "present, executable",
            }
        );
        println!(
            "Image file:    {} ({}; {})",
            image.display(),
            image_from,
            if image.exists() {
                "present"
            } else {
                "absent, downloaded on first use or with 'recipe runtime fetch'"
            }
        );
        println!(
            "Sources:       {}{}",
            sources.display(),
            if sources.exists() { "" } else { " (absent)" }
        );
    }
    Ok(())
}

/// Create `path` and any missing parents as 0755.
fn mkdirs(path: &Path) -> Result<()> {
    let mut missing: Vec<&Path> = path
        .ancestors()
        .take_while(|p| !p.as_os_str().is_empty() && !p.exists())
        .collect();
    missing.reverse();
    for directory in missing {
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(directory)
            .with_context(|| format!("cannot create {}", directory.display()))?;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Replace `destination` with `content` at `mode`.
fn put(destination: &Path, content: &[u8], mode: u32) -> Result<()> {
    mkdirs(
        destination
            .parent()
            .context("destination has no directory")?,
    )?;
    remove_if_present(destination)?;
    std::fs::write(destination, content)
        .with_context(|| format!("cannot write {}", destination.display()))?;
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(mode))?;
    eprintln!("wrote {}", destination.display());
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => bail!(
            "{} is a directory; cannot replace it with a file",
            path.display()
        ),
        Ok(_) => {
            std::fs::remove_file(path).with_context(|| format!("cannot replace {}", path.display()))
        }
        Err(_) => Ok(()),
    }
}

fn copy_file(source: &Path, destination: &Path, mode: u32) -> Result<()> {
    mkdirs(
        destination
            .parent()
            .context("destination has no directory")?,
    )?;
    remove_if_present(destination)?;
    std::fs::copy(source, destination).with_context(|| {
        format!(
            "cannot copy {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(mode))?;
    eprintln!("wrote {}", destination.display());
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Copy the files of `source` into `destination`, keeping links; directories
/// 0755, files 0644 unless executable. Git's own `.git` entries are left out.
fn copy_tree(source: &Path, destination: &Path) -> Result<usize> {
    mkdirs(destination)?;
    let mut count = 0;
    for entry in
        std::fs::read_dir(source).with_context(|| format!("cannot read {}", source.display()))?
    {
        let entry = entry?;
        if entry.file_name().as_bytes() == b".git" {
            continue;
        }
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&from)?;
        if meta.is_dir() {
            count += copy_tree(&from, &to)?;
        } else if meta.file_type().is_symlink() {
            remove_if_present(&to)?;
            std::os::unix::fs::symlink(std::fs::read_link(&from)?, &to)
                .with_context(|| format!("cannot link {}", to.display()))?;
            count += 1;
        } else if meta.is_file() {
            let mode = if meta.mode() & 0o111 != 0 {
                0o755
            } else {
                0o644
            };
            remove_if_present(&to)?;
            std::fs::copy(&from, &to)
                .with_context(|| format!("cannot copy {} to {}", from.display(), to.display()))?;
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(mode))?;
            count += 1;
        }
    }
    Ok(count)
}

fn stage(
    root: &Path,
    prefix: &Path,
    launcher: &Path,
    reference: &str,
    image: Option<&Path>,
    catalog: &Path,
    apparmor: bool,
) -> Result<()> {
    runtime::validate_reference(reference)?;
    let digest = runtime::root_digest(reference)?;
    ensure!(
        launcher.is_file(),
        "the launcher {} is not a file; pass --launcher with the Ginux launcher to ship",
        launcher.display()
    );
    ensure!(
        catalog.join("index.json").is_file(),
        "{} has no index.json; run 'recipe index' in that catalog and commit it",
        catalog.display()
    );
    let prefix_text = prefix.to_str().context("--prefix is not valid UTF-8")?;
    ensure!(
        prefix_text.starts_with('/')
            && prefix_text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-')),
        "--prefix {prefix_text:?} must be an absolute path of letters, digits and `/._-`"
    );
    if let Some(image) = image {
        let actual = sha256_file(image)?;
        ensure!(
            actual == digest,
            "the image {} has SHA-256 {actual}, not {digest} from the reference; pass the matching tar or leave out --image",
            image.display()
        );
    }

    let base = root.join(prefix_text.trim_start_matches('/'));
    copy_file(launcher, &base.join("libexec/recipe/ginux"), 0o755)?;
    put(
        &base.join("share/recipe/runtime/root-reference"),
        format!("{reference}\n").as_bytes(),
        0o644,
    )?;
    if let Some(image) = image {
        copy_file(image, &base.join("share/recipe/runtime/rootfs.tar"), 0o644)?;
    }
    let sources = base.join("share/recipe/sources");
    let files = copy_tree(catalog, &sources.join("levitate"))?;
    eprintln!(
        "wrote {} ({files} files)",
        sources.join("levitate").display()
    );
    put(
        &sources.join("levitate.toml"),
        OFFICIAL_SOURCE.as_bytes(),
        0o644,
    )?;

    let mut profile = PROFILE.to_owned();
    if prefix_text.trim_end_matches('/') != "/usr" {
        let bin = format!("{}/bin", prefix_text.trim_end_matches('/'));
        profile = profile.replacen(
            "export PATH\n",
            &format!(
                "case \":$PATH:\" in *:{bin}:*) ;; *) PATH=\"{bin}:$PATH\" ;; esac\nexport PATH\n"
            ),
            1,
        );
    }
    put(
        &root.join("etc/profile.d/recipe.sh"),
        profile.as_bytes(),
        0o644,
    )?;
    if apparmor {
        let launcher = format!("{}/libexec/recipe/ginux", prefix_text.trim_end_matches('/'));
        put(
            &root.join("etc/apparmor.d/recipe-ginux"),
            APPARMOR.replace("@LAUNCHER@", &launcher).as_bytes(),
            0o644,
        )?;
    }
    Ok(())
}
