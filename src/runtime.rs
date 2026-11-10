//! Installed Ginux defaults, shared by the engine and OS runtime packaging.
//! Explicit environment selections take precedence; payload verification and
//! mandatory managed execution remain unchanged.
use anyhow::{Context, Result, bail};
use std::path::PathBuf;

pub const LAUNCHER: &str = "/usr/libexec/recipe/ginux";
pub const ROOTFS: &str = "/usr/share/recipe/runtime/rootfs.tar";
pub const ROOT_REFERENCE: &str = "/usr/share/recipe/runtime/root-reference";
const MAX_CONFIG_PATH: usize = 128;

/// Default immutable image reference: the pinned Rocky rootfs content,
/// named by digest rather than by host path (host `.artifacts` paths never
/// enter instance config). Override with `GINUX_ROOT`; the override must pass
/// the same reference validation. The launcher resolves the reference.
const DEFAULT_ROOT_REF: &str = "oci:rocky-10.2.20260525.0-amd64-rootfs@sha256:b93bfd0f6deea2a7a96216655cda3ebbd87a445231548ff491ee30aed20a36ab";

/// Image reference: the pin by default, `GINUX_ROOT` override when set.
/// Mirrors the instance parser's reference rule (no leading `/`, no `..`,
/// charset `[A-Za-z0-9_.:/@-]`), plus the §2 ≤ 128 B bound.
pub fn root_reference() -> Result<String> {
    let root = match std::env::var("GINUX_ROOT") {
        Ok(root) => root,
        Err(std::env::VarError::NotPresent) => {
            installed_reference()?.unwrap_or_else(|| DEFAULT_ROOT_REF.to_string())
        }
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("input preparation failure: GINUX_ROOT is not valid UTF-8")
        }
    };
    if root.is_empty() || root.len() > MAX_CONFIG_PATH {
        bail!(
            "input preparation failure: instance root {root:?} has {} B, want 1..={MAX_CONFIG_PATH} B (§2)",
            root.len(),
        );
    }
    validate_reference(&root)?;
    Ok(root)
}

/// Mirror of the instance parser's reference rule for `root`/`storage`/`devices`.
fn validate_reference(value: &str) -> Result<()> {
    if value.starts_with('/')
        || value.contains("..")
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/' | b'@')
        })
    {
        bail!(
            "input preparation failure: instance reference {value:?} is not a reference \
             (no leading `/`, no `..`, charset `[A-Za-z0-9_.:/@-]`; host paths never enter config)"
        );
    }
    Ok(())
}

pub fn root_digest(reference: &str) -> Result<&str> {
    let (image, digest) = reference.split_once("@sha256:").context(
        "domain.root must identify the assembled rootfs tar as oci:NAME@sha256:CONTENT_DIGEST",
    )?;
    if !image.starts_with("oci:")
        || image.len() <= 4
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!(
            "domain.root must identify the assembled rootfs tar as oci:NAME@sha256:CONTENT_DIGEST"
        );
    }
    Ok(digest)
}

fn installed_reference() -> Result<Option<String>> {
    match std::fs::read_to_string(ROOT_REFERENCE) {
        Ok(value) => Ok(Some(value.trim().to_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("cannot read installed Recipe runtime reference"),
    }
}

/// Select the root archive; the engine hashes the selected bytes before use.
pub fn rootfs_path(reference: &str) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("GINUX_ROOTFS") {
        if path.is_empty() {
            bail!("GINUX_ROOTFS is empty; select a verified assembled rootfs tar");
        }
        return Ok(PathBuf::from(path));
    }
    let digest = root_digest(reference)?;
    if installed_reference()?.as_deref() == Some(reference) {
        return Ok(PathBuf::from(ROOTFS));
    }
    Ok(dirs::cache_dir()
        .context("cannot locate the image cache; set GINUX_ROOTFS to an assembled rootfs tar")?
        .join("recipe/images")
        .join(format!("{digest}.tar")))
}
