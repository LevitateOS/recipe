//! The required Ginux launcher for executable recipe operations.
use anyhow::{Context, Result, anyhow, bail};
use std::path::{Path, PathBuf};

/// Verified executable used to start the Ginux domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GinuxLauncher {
    path: PathBuf,
}

impl GinuxLauncher {
    /// Resolve and validate the required GINUX_BIN executable.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            path: resolve_launcher()?,
        })
    }

    /// The executable selected for this invocation.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Route one executable recipe operation through Ginux.
    ///
    /// Prepare immutable inputs, execute in Ginux, and return the final context
    /// only after successful completion and validated publication. Persistence
    /// retains separate state/files pairs; there is no host execution callback.
    pub fn run(
        &self,
        operation: crate::worker::protocol::Operation,
        cleanup_reason: Option<&str>,
        recipe: &Path,
        search_path: Option<&Path>,
        persist_state: bool,
        options: crate::worker::protocol::InvocationOptions,
        inputs: &[crate::inputs::ExtraInput],
        destination: &Path,
        state_dir: Option<&Path>,
    ) -> Result<rhai::Map> {
        options
            .validate_for(operation)
            .context(crate::failure::Category::Startup.error("invalid invocation options"))?;
        let prepared = crate::inputs::prepare_invocation(
            recipe,
            search_path,
            inputs,
            &options.execution,
        )
        .map_err(|err| {
            anyhow::Error::new(crate::failure::Category::Startup.error(format!("ginux input preparation failed for \
                             `{operation}` ({}): {err:#}",
                recipe.display(),
            )))
        })?;
        let report = crate::launch::run_domain(
            &prepared,
            operation,
            cleanup_reason,
            &self.path,
            persist_state,
            &options,
            destination,
            state_dir,
            recipe,
        )?;
        crate::diagnostics::diag!(
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
            // The protocol guarantees a failure record on every non-ok result.
            let failure = report
                .result
                .failure
                .clone()
                .expect("protocol: a failed result carries its failure");
            return Err(anyhow::Error::new(WorkloadFailure {
                fatal: failure.fatal,
                phase: failure.phase,
                reason: failure.reason,
                message: failure.message,
            }).context(crate::failure::Category::Workload.error(format!("`{operation}` failed in the Ginux domain (invocation {})", prepared.invocation_id))));
        }
        ctx_summary_to_map(&report.result.ctx_summary).map_err(|err| {
            anyhow::Error::new(crate::failure::Category::Protocol.error(format!("`{operation}` succeeded in the Ginux domain \
                     (invocation {}) but its ctx summary is not a JSON object: {err:#}",
                prepared.invocation_id,
            )))
        })
    }
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
        crate::failure::Category::Startup.error("ginux backend selected but GINUX_BIN is not set \
         (no launcher to start; refusing host fallback)")
    })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!(crate::failure::Category::Startup.error("ginux backend selected but GINUX_BIN is empty \
             (no launcher to start; refusing host fallback)"));
    }
    let path = PathBuf::from(trimmed);
    let meta = std::fs::metadata(&path).with_context(|| {
        crate::failure::Category::Startup.error(format!("ginux launcher {} is missing or inaccessible \
             (refusing host fallback)",
            path.display()
        ))
    })?;
    if !meta.is_file() {
        bail!(crate::failure::Category::Startup.error(format!("ginux launcher {} is not a file \
             (refusing host fallback)",
            path.display()
        )));
    }
    if !is_executable(&path, &meta) {
        bail!(crate::failure::Category::Startup.error(format!("ginux launcher {} is not executable \
             (refusing host fallback)",
            path.display()
        )));
    }
    Ok(path)
}

/// True when the current user could exec this file (owner/group/other exec bit).
fn is_executable(_path: &Path, meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[derive(Debug)]
pub(crate) struct WorkloadFailure {
    pub fatal: bool,
    pub phase: String,
    pub reason: String,
    pub message: String,
}
impl std::fmt::Display for WorkloadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "phase={} reason={}: {}",
            self.phase, self.reason, self.message
        )
    }
}
impl std::error::Error for WorkloadFailure {}
