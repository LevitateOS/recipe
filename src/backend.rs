//! Explicit backend selection (R04): one surface shared by CLI and library callers.
//!
//! Every executable recipe operation (`install` / `remove` / `cleanup` / the
//! manual `is_*` checks, including the R02 worker paths) routes through the
//! [`Backend`] value carried by [`crate::RecipeEngine`]. The default is host
//! execution; selecting `ginux` requires a usable launcher and fails
//! explicitly otherwise — never silently falling back to host.
//!
//! Sources, in precedence order:
//!
//! 1. An explicit caller choice ([`BackendKind`], wired to `--backend` on the CLI).
//! 2. The `RECIPE_BACKEND` environment variable (`host` / `in-process` / `ginux`).
//! 3. The built-in default: host.
//!
//! The Ginux launcher path comes from `GINUX_BIN` (see
//! `docs/ginux-recipe-interfaces.md` §2). A selected `ginux` backend with a
//! missing or unexecutable launcher is a `startup` failure. When
//! `RECIPE_WORKER=1` is set, selection is forced to in-process execution: the
//! worker can never recursively invoke the Ginux backend.
//!
//! What this does NOT do (owned by R06): actually launching Ginux. The
//! [`Backend::run`] Ginux arm runs R05 input preparation for real and then
//! returns an explicit error naming the operation, the prepared invocation,
//! and the resolved launcher; R06 replaces the error with real domain launch.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

/// Requested execution backend kind, before launcher resolution.
///
/// Parses from `--backend` / `RECIPE_BACKEND` (`host` and `in-process` both
/// mean host execution; anything else besides `ginux` is an explicit error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Host,
    Ginux,
}

impl BackendKind {
    /// Parse one `RECIPE_BACKEND` value. Empty means "unset" (no explicit choice).
    pub fn from_env_value(raw: &str) -> Result<Option<Self>> {
        let value = raw.trim();
        if value.is_empty() {
            return Ok(None);
        }
        value.parse::<BackendKind>().map(Some).map_err(|_| {
            anyhow::anyhow!(
                "invalid RECIPE_BACKEND {raw:?}: want \"host\" (or \"in-process\") or \"ginux\""
            )
        })
    }
}

impl std::str::FromStr for BackendKind {
    type Err = anyhow::Error;

    fn from_str(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "host" | "in-process" => Ok(BackendKind::Host),
            "ginux" => Ok(BackendKind::Ginux),
            other => bail!("invalid backend {raw:?}: want \"host\" or \"ginux\" (got {other:?})"),
        }
    }
}

/// Resolved execution backend: the single choice every executable recipe
/// operation routes through. This is the selection surface R06 launches from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// Run recipe code in-process on the host (the default).
    Host,
    /// Run recipe code inside a Ginux domain started from this launcher.
    Ginux {
        /// Resolved executable launcher (from `GINUX_BIN`), verified present
        /// and executable at selection time.
        launcher: PathBuf,
    },
}

impl Backend {
    /// Resolve the backend from an explicit caller choice (`--backend`),
    /// falling back to `RECIPE_BACKEND`, falling back to host.
    ///
    /// Under `RECIPE_WORKER=1` this always returns [`Backend::Host`]
    /// (forced in-process execution); an explicit non-host request is noted
    /// on stderr so the override is visible, never silent.
    pub fn select(explicit: Option<BackendKind>) -> Result<Self> {
        let requested = match explicit {
            Some(kind) => Some(kind),
            None => match std::env::var("RECIPE_BACKEND") {
                Ok(raw) => BackendKind::from_env_value(&raw)?,
                Err(std::env::VarError::NotPresent) => None,
                Err(std::env::VarError::NotUnicode(_)) => {
                    bail!("invalid RECIPE_BACKEND: value is not valid Unicode")
                }
            },
        };

        if std::env::var("RECIPE_WORKER").as_deref() == Ok("1") {
            if requested == Some(BackendKind::Ginux) {
                eprintln!(
                    "recipe: RECIPE_WORKER=1 forces in-process execution; \
                     ignoring requested ginux backend (no recursive Ginux launch)"
                );
            }
            return Ok(Backend::Host);
        }

        match requested.unwrap_or(BackendKind::Host) {
            BackendKind::Host => Ok(Backend::Host),
            BackendKind::Ginux => {
                let launcher = resolve_launcher()?;
                Ok(Backend::Ginux { launcher })
            }
        }
    }

    /// Resolve the backend from the environment alone (library callers).
    pub fn from_env() -> Result<Self> {
        Self::select(None)
    }

    /// The requested kind behind this resolved backend.
    pub fn kind(&self) -> BackendKind {
        match self {
            Backend::Host => BackendKind::Host,
            Backend::Ginux { .. } => BackendKind::Ginux,
        }
    }

    /// Short backend name for logs and diagnostics.
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Host => "host",
            Backend::Ginux { .. } => "ginux",
        }
    }

    /// Launcher executable for the `ginux` backend, if selected.
    pub fn launcher(&self) -> Option<&Path> {
        match self {
            Backend::Host => None,
            Backend::Ginux { launcher } => Some(launcher),
        }
    }

    /// Route one executable recipe operation through the selected backend.
    ///
    /// Host runs `host_op` in-process. Ginux prepares the invocation inputs
    /// for real (R05: discovery, ids, allowlisted env, staged files, domain
    /// config) and then fails explicitly because domain launch is not wired
    /// yet (R06 owns it): the error names the operation, the prepared
    /// invocation, and the resolved launcher, and host execution is never
    /// used as a silent fallback.
    pub fn run<T>(
        &self,
        operation: &str,
        recipe: &Path,
        search_path: Option<&Path>,
        host_op: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        match self {
            Backend::Host => host_op(),
            Backend::Ginux { launcher } => {
                let prepared = crate::inputs::prepare_invocation(recipe, operation, search_path, &[], &[])
                    .map_err(|err| {
                        anyhow!(
                            "backend startup failure: ginux input preparation failed for \
                             `{operation}` ({}): {err:#}",
                            recipe.display(),
                        )
                    })?;
                bail!(
                    "backend startup failure: cannot run `{operation}` through ginux: \
                     domain launch is not wired yet (R06 owns it); refusing to run on \
                     the host instead (no silent host fallback). Prepared invocation {} \
                     ({} input{} staged under {}) for launcher {}.",
                    prepared.invocation_id,
                    prepared.inputs.len(),
                    if prepared.inputs.len() == 1 { "" } else { "s" },
                    prepared.staging_dir().display(),
                    launcher.display(),
                )
            }
        }
    }
}

/// Resolve the Ginux launcher from `GINUX_BIN` (interfaces §2).
///
/// A missing, empty, nonexistent, or unexecutable value is an explicit
/// `startup` failure — never a silent fallback to host execution.
fn resolve_launcher() -> Result<PathBuf> {
    let raw = std::env::var("GINUX_BIN").with_context(|| {
        "backend startup failure: ginux backend selected but GINUX_BIN is not set \
         (no launcher to start; refusing host fallback)"
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!(
            "backend startup failure: ginux backend selected but GINUX_BIN is empty \
             (no launcher to start; refusing host fallback)"
        );
    }
    let path = PathBuf::from(trimmed);
    let meta = std::fs::metadata(&path).with_context(|| {
        format!(
            "backend startup failure: ginux launcher {} is missing or inaccessible \
             (refusing host fallback)",
            path.display()
        )
    })?;
    if !meta.is_file() {
        bail!(
            "backend startup failure: ginux launcher {} is not a file \
             (refusing host fallback)",
            path.display()
        );
    }
    if !is_executable(&path, &meta) {
        bail!(
            "backend startup failure: ginux launcher {} is not executable \
             (refusing host fallback)",
            path.display()
        );
    }
    Ok(path)
}

/// True when the current user could exec this file (owner/group/other exec bit).
#[cfg(unix)]
fn is_executable(_path: &Path, meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

/// Non-Unix fallback: existence as a file is the only check available.
#[cfg(not(unix))]
fn is_executable(_path: &Path, _meta: &std::fs::Metadata) -> bool {
    true
}
