//! Git acquisition at an explicit destination and an exact pinned commit.

use crate::core::output;
use crate::helpers::internal::{cmd, progress};
use rhai::EvalAltResult;
use std::path::Path;
use std::process::Command;

fn validate_origin(url: &str) -> Result<(), Box<EvalAltResult>> {
    if url.contains(['\0', '\n', '\r'])
        || !["https://", "http://", "file://"]
            .iter()
            .any(|prefix| url.starts_with(prefix))
    {
        return Err(
            "Git origin must use https://, http://, or an imported file:// repository".into(),
        );
    }
    Ok(())
}

fn git(dir: Option<&Path>, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    // These are HTTP low-speed bounds, not a whole-process timeout.
    command
        .args([
            "-c",
            "http.lowSpeedLimit=1000",
            "-c",
            "http.lowSpeedTime=600",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    // Inherited repository locators must not redirect this command's writes.
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_PARAMETERS",
        "GIT_CONFIG_COUNT",
        "GIT_TEMPLATE_DIR",
        "GIT_SSL_NO_VERIFY",
    ] {
        command.env_remove(name);
    }
    if let Some(dir) = dir {
        command
            .arg("-C")
            .arg(dir)
            .args(["--git-dir=.git", "--work-tree=."]);
    }
    command.args(args);
    command
}

fn query(dir: &Path, args: &[&str]) -> Result<String, Box<EvalAltResult>> {
    let output = cmd::checked_capture(git(Some(dir), args))?;
    String::from_utf8(output.stdout)
        .map(|text| text.trim_end_matches(['\r', '\n']).to_owned())
        .map_err(|error| format!("invalid Git output: {error}").into())
}

fn validate_checkout(dest: &Path, url: &str) -> Result<(), Box<EvalAltResult>> {
    // Do not accidentally discover a parent repository or traverse a linked .git.
    let metadata = std::fs::symlink_metadata(dest.join(".git")).map_err(|error| {
        format!(
            "Git destination {} is not a standalone clone: {error}",
            dest.display()
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "Git destination {} needs a regular .git directory",
            dest.display()
        )
        .into());
    }
    query(dest, &["rev-parse", "--verify", "HEAD"])
        .map_err(|error| format!("invalid Git checkout at {}: {error}", dest.display()))?;
    let actual = query(dest, &["config", "--get", "remote.origin.url"])
        .map_err(|error| format!("cannot read Git origin at {}: {error}", dest.display()))?;
    if actual != url {
        return Err(format!(
            "Git origin mismatch at {}; refusing to reuse or replace this checkout",
            dest.display()
        )
        .into());
    }
    Ok(())
}

fn select_commit(dest: &Path, commit: &str) -> Result<(), Box<EvalAltResult>> {
    if !query(dest, &["status", "--porcelain", "--untracked-files=all"])?.is_empty() {
        return Err(format!(
            "Git checkout {} has local changes; refusing to replace them",
            dest.display()
        )
        .into());
    }
    let current = query(dest, &["rev-parse", "--verify", "HEAD"])?;
    if current.eq_ignore_ascii_case(commit) {
        return Ok(());
    }
    let object = format!("{commit}^{{commit}}");
    let present = cmd::capture(git(Some(dest), &["cat-file", "-e", &object]))?;
    if !present.status.success() {
        cmd::run_checked(git(Some(dest), &["fetch", "--no-tags", "origin", commit]))?;
    }
    let resolved = query(
        dest,
        &["rev-parse", "--verify", "--end-of-options", &object],
    )?;
    if !resolved.eq_ignore_ascii_case(commit) {
        return Err("Git revision did not resolve to the requested commit".into());
    }
    cmd::run_checked(git(Some(dest), &["checkout", "--detach", commit]))
}

fn acquire(url: &str, dest: &Path, commit: &str) -> Result<String, Box<EvalAltResult>> {
    validate_origin(url)?;
    if dest.file_name().is_none() {
        return Err("Git destination must name a checkout directory".into());
    }
    match std::fs::symlink_metadata(dest) {
        Ok(metadata) => {
            if !metadata.is_dir() {
                return Err(format!(
                    "Git destination {} is not a regular directory",
                    dest.display()
                )
                .into());
            }
            validate_checkout(dest, url)?;
            select_commit(dest, commit)?;
            output::detail(&format!("git: reusing {}", dest.display()));
            return Ok(dest.to_string_lossy().into_owned());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(
                format!("cannot inspect Git destination {}: {error}", dest.display()).into(),
            );
        }
    }

    let parent = dest
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create Git destination parent: {error}"))?;
    let staging = tempfile::Builder::new()
        .prefix(".recipe-git.")
        .tempdir_in(parent)
        .map_err(|error| format!("cannot create Git staging directory: {error}"))?;
    let staged = staging.path().join("checkout");
    let mut command = git(None, &["clone", "--no-local", "--progress", "--template="]);
    command.arg("--").arg(url).arg(&staged);
    let pb = progress::create_spinner("cloning Git source");
    let _guard = progress::ProgressGuard::new(&pb);
    cmd::run_checked(command)?;
    validate_checkout(&staged, url)?;
    select_commit(&staged, commit)?;
    std::fs::rename(&staged, dest)
        .map_err(|error| format!("cannot publish Git checkout {}: {error}", dest.display()))?;
    output::detail(&format!("git: acquired {}", dest.display()));
    Ok(dest.to_string_lossy().into_owned())
}

/// Acquire an exact destination at a full SHA-1 or SHA-256 commit ID.
/// Cached checkouts must have the same origin and no local changes.
/// Selection uses detached HEAD and never resets or cleans a dirty checkout.
pub fn git_checkout(url: &str, dest: &str, commit: &str) -> Result<String, Box<EvalAltResult>> {
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(
            "git_checkout requires a full 40- or 64-character hexadecimal commit ID".into(),
        );
    }
    acquire(url, Path::new(dest), commit)
}
