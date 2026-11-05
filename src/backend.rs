//! Mandatory Ginux backend selection (R04 selection, S0 policy, R06 launch).
//!
//! Every executable recipe operation (`install` / `remove` / `cleanup` / the
//! manual `is_*` checks) routes through the [`Backend`] value carried by
//! [`crate::RecipeEngine`]. Ginux is the only public backend: the default
//! selection launches the configured domain, and legacy host selectors
//! (`--backend host`, `RECIPE_BACKEND=host`) are rejected with an explicit
//! migration error — never honored, never a silent fallback.
//!
//! Sources, in precedence order:
//!
//! 1. An explicit caller choice ([`BackendKind`], wired to `--backend` on the CLI).
//! 2. The `RECIPE_BACKEND` environment variable (`ginux` accepted; `host` /
//!    `in-process` rejected as removed).
//! 3. The built-in default: `ginux`.
//!
//! The Ginux launcher path comes from `GINUX_BIN` (see
//! `docs/ginux-recipe-interfaces.md` §2). A missing or unexecutable launcher
//! is a `startup` failure. Public selection always requires Ginux, regardless
//! of worker environment variables. The private worker entry invokes the shared
//! lifecycle executor directly inside its domain; it does not select a backend.
//!
//! [`Backend::run`] launches the domain for real (R06): it prepares the R05
//! invocation inputs, spawns the launcher, pumps the R01 worker protocol,
//! and maps outcomes to the interface §1 categories (result file
//! authoritative). Host recipe execution is unavailable outside the internal
//! worker entry path.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

/// Requested execution backend kind, before launcher resolution.
///
/// Parses from `--backend` / `RECIPE_BACKEND`. `host` and `in-process` are
/// legacy selectors: they parse so [`Backend::select`] can reject them with
/// a migration error, but they never select host execution. Anything else
/// besides `ginux` is an explicit parse error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// Legacy host request. Rejected by [`Backend::select`] with a migration
    /// error; kept parseable so the error names the removal instead of
    /// reporting an unknown value.
    Host,
    /// Ginux domain launch (the only public backend; also the default when
    /// no explicit choice is given).
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
    /// Run recipe code inside a Ginux domain started from this launcher.
    Ginux {
        /// Resolved executable launcher (from `GINUX_BIN`), verified present
        /// and executable at selection time.
        launcher: PathBuf,
    },
}

impl Backend {
    /// Resolve the backend from an explicit caller choice (`--backend`),
    /// falling back to `RECIPE_BACKEND`, falling back to the `ginux` default.
    ///
    /// A legacy `host`/`in-process` request (flag or env) is a `startup`
    /// failure with migration guidance: public host execution was removed
    /// (S0) and is never honored.
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

        match requested.unwrap_or(BackendKind::Ginux) {
            BackendKind::Host => Err(host_removed_error()),
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
            Backend::Ginux { .. } => BackendKind::Ginux,
        }
    }

    /// Short backend name for logs and diagnostics.
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Ginux { .. } => "ginux",
        }
    }

    /// Launcher executable for the `ginux` backend, if selected.
    pub fn launcher(&self) -> Option<&Path> {
        match self {
            Backend::Ginux { launcher } => Some(launcher),
        }
    }

    /// Route one executable recipe operation through the selected backend.
    ///
    /// The legacy callback is retained until coordinator option transport is
    /// migrated; it is never invoked. Public recipe evaluation always launches
    /// Ginux. The private worker calls the shared lifecycle executor directly.
    ///
    /// Ginux prepares the invocation inputs for real (R05: discovery, ids,
    /// allowlisted env, staged files, domain config), launches the domain
    /// through the resolved launcher, and returns the worker's final context.
    /// `persist_ctx` is the caller's persistence preference; it travels in
    /// the worker request frame (checks ignore it) and R07 decides what the
    /// acked generations mean for durable state.
    pub fn run(
        &self,
        operation: &str,
        recipe: &Path,
        search_path: Option<&Path>,
        persist_ctx: bool,
        _host_op: impl FnOnce() -> Result<rhai::Map>,
    ) -> Result<rhai::Map> {
        match self {
            Backend::Ginux { launcher } => {
                let prepared =
                    crate::inputs::prepare_invocation(recipe, operation, search_path, &[], &[])
                        .map_err(|err| {
                            anyhow!(
                                "backend startup failure: ginux input preparation failed for \
                                 `{operation}` ({}): {err:#}",
                                recipe.display(),
                            )
                        })?;
                let report = crate::launch::run_domain(&prepared, launcher, persist_ctx)?;
                eprintln!(
                    "recipe: invocation {} finished in the Ginux domain: ok={} ({} checkpoint{} \
                     acked; launcher exit {}; result {})",
                    prepared.invocation_id,
                    report.result.ok,
                    report.checkpoints_acked,
                    if report.checkpoints_acked == 1 {
                        ""
                    } else {
                        "s"
                    },
                    report
                        .launcher_code
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "signaled".to_string()),
                    report.result_outcome,
                );
                if !report.result.ok {
                    let detail = report
                        .result
                        .failures
                        .first()
                        .map(|failure| {
                            format!(
                                "phase={} reason={}: {}",
                                failure.phase, failure.reason, failure.message
                            )
                        })
                        .unwrap_or_else(|| "worker reported failure without detail".to_string());
                    bail!(
                        "backend workload failure: `{operation}` failed in the Ginux domain \
                         (invocation {}): {detail}",
                        prepared.invocation_id,
                    );
                }
                ctx_summary_to_map(&report.result.ctx_summary).map_err(|err| {
                    anyhow!(
                        "backend protocol failure: `{operation}` succeeded in the Ginux domain \
                         (invocation {}) but its ctx summary is not a JSON object: {err:#}",
                        prepared.invocation_id,
                    )
                })
            }
        }
    }
}

/// Rejection for every public host-execution path (S0): Ginux is required
/// for every operation that evaluates recipe code. Callers holding a legacy
/// host selector must drop it (or set it to `ginux`); with no usable
/// launcher in `GINUX_BIN` this fails explicitly instead of running on the
/// host. Refusing host execution.
fn host_removed_error() -> anyhow::Error {
    anyhow!(
        "backend startup failure: public host execution was removed (S0): \
         Ginux is required for every operation that evaluates recipe code. \
         Remove `--backend host` / `RECIPE_BACKEND=host` (or set them to `ginux`); \
         with no usable launcher in `GINUX_BIN` this fails explicitly instead of \
         running on the host. Refusing host execution."
    )
}

/// Convert a worker `ctx_summary` JSON object into the caller-facing context
/// map. Scalars round-trip exactly; nested values become the matching Rhai
/// types. A non-object summary is a protocol failure, never silent loss.
fn ctx_summary_to_map(value: &serde_json::Value) -> Result<rhai::Map> {
    match value {
        serde_json::Value::Object(fields) => {
            let mut map = rhai::Map::new();
            for (name, item) in fields {
                map.insert(name.clone().into(), json_to_dynamic(item)?);
            }
            Ok(map)
        }
        other => anyhow::bail!("ctx summary is {}, want a JSON object", json_kind(other)),
    }
}

/// Convert one JSON value to the matching Rhai dynamic value.
fn json_to_dynamic(value: &serde_json::Value) -> Result<rhai::Dynamic> {
    match value {
        serde_json::Value::Null => Ok(rhai::Dynamic::UNIT),
        serde_json::Value::Bool(flag) => Ok(rhai::Dynamic::from_bool(*flag)),
        serde_json::Value::Number(number) => {
            if let Some(signed) = number.as_i64() {
                Ok(rhai::Dynamic::from_int(signed))
            } else if let Some(unsigned) = number.as_u64() {
                i64::try_from(unsigned)
                    .map(rhai::Dynamic::from_int)
                    .map_err(|_| {
                        anyhow!("ctx summary number {unsigned} exceeds i64 (not representable)")
                    })
            } else if let Some(float) = number.as_f64() {
                Ok(rhai::Dynamic::from_float(float))
            } else {
                anyhow::bail!("ctx summary number {number} is not representable")
            }
        }
        serde_json::Value::String(text) => Ok(rhai::Dynamic::from(text.clone())),
        serde_json::Value::Array(items) => {
            let mut array = rhai::Array::with_capacity(items.len());
            for item in items {
                array.push(json_to_dynamic(item)?);
            }
            Ok(rhai::Dynamic::from_array(array))
        }
        serde_json::Value::Object(fields) => {
            let mut map = rhai::Map::new();
            for (name, item) in fields {
                map.insert(name.clone().into(), json_to_dynamic(item)?);
            }
            Ok(rhai::Dynamic::from_map(map))
        }
    }
}

/// JSON value kind for diagnostics.
fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
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
