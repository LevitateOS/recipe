//! The coordinator/worker records: the request, the result, and the live
//! diagnostic and progress lines.
//!
//! Recipe owns their shape; Ginux never reads them. Nothing travels from the
//! coordinator to the worker once it has started (its standard input is
//! `/dev/null`), so the exchange is three plain forms, none of them framed
//! or binary (owner decision D6, 2026-11-22,
//! `docs/ginux-public-lifecycle-migration.md`, section 3.1):
//!
//! - **Request in, a file.** The coordinator writes one [`WorkerRequest`] as
//!   the reserved input [`crate::domain::REQUEST_NAME`], staged with the
//!   other inputs (read-only, at [`crate::domain::REQUEST`]). The worker reads
//!   it through [`crate::domain::local`], so a host-mode worker finds it below
//!   its host root, which it learns from [`crate::domain::ENV_HOST_ROOT`]
//!   (the request cannot carry it: it is read from there).
//! - **Result out, a file.** The worker writes one [`WorkerResult`] to
//!   [`crate::domain::RESULT`], inside the `work` tree the coordinator reads
//!   back after the run (from the export archive of a Ginux run, from the run
//!   directory of a host run). It writes [`crate::domain::RESULT_STAGING`]
//!   first and renames it into place, so the result is whole or absent. The
//!   coordinator reads it as untrusted input, bounded by
//!   [`crate::domain::MAX_RESULT_BYTES`] and parsed strictly; it must name
//!   the run's invocation id, so a result a kept archive brought back from an
//!   earlier run never counts. It is never an output: outputs are
//!   `/out/NAME`, and the result lives in `/work`.
//! - **Diagnostics and progress, lines.** The worker writes one
//!   [`LineRecord`] per line on its standard output: a JSON object with the
//!   version and exactly one of `diagnostic` or `progress`, at most
//!   [`MAX_LINE_BYTES`] including the newline. The coordinator relays them
//!   live; a line that is not such a record (not JSON, another version,
//!   another shape, too long) is kept as plain log text, never an error.
//!   Standard error stays for the workload's and the runtime's own text.
//!
//! Records carry no host paths except a host-mode request's offline cache:
//! `invocation_id` values follow `[A-Za-z0-9_-]{1,64}`, and a result names
//! each declared output by the domain path the engine placed it at
//! ([`crate::domain::output_path`]); the coordinator validates them before
//! publication.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Version carried in every record (request, result, line). Anything else is
/// refused: a request or result of another version is invalid, a line of
/// another version is plain text.
// API 1 (docs/recipe-format.md): the operations (download, build, install,
// cleanup, latest), the engine-placed outputs, the dependencies' outputs and
// the result's step statuses of transport 11 require a matching coordinator
// and worker.
// 12: the request carries the recipe's name (`recipe_name`).
// 13: no checkpoint and ack frames, no saved state in the request or result.
// 14: no frames: the request and the result are files, diagnostics and
// progress are lines; the result names its invocation.
// 15: the network grant is its own request field for both runtimes
// (`network`); the offline cache of a Ginux run is a copy of the declared
// pins.
pub const RECORDS_VERSION: u32 = 15;

/// Longest line record, newline included. Derived from the record bounds
/// that predate lines: a diagnostic carries at most
/// [`MAX_DIAGNOSTIC_CHUNK`] bytes (at most four JSON characters each), a
/// progress record three texts of at most 256 bytes (at most six JSON
/// characters each, `\uXXXX`).
pub const MAX_LINE_BYTES: usize = 8192;

/// Keep a diagnostic line below [`MAX_LINE_BYTES`] even when every byte
/// requires four JSON characters.
pub const MAX_DIAGNOSTIC_CHUNK: usize = 768;
pub const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_DIAGNOSTIC_RECORDS: usize = 32768;

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

/// The operation one invocation runs; its wire form is the name.
///
/// The worker runs the same steps for `build` and `install` (the download
/// step, then the step that builds the outputs); the coordinator keeps the
/// result for both and publishes it only for `install` (docs/recipe-format.md,
/// sections 6.1 and 6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// The download step only.
    Download,
    /// The download step where it is missing, then the build step; the
    /// result is kept, not published.
    Build,
    /// Every missing step; the outputs are published.
    Install,
    /// `cleanup("manual")` (section 3.7), after Recipe deleted the material.
    Cleanup,
    /// `latest()` (sections 2.9 and 3.5): the newest upstream version.
    Latest,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Build => "build",
            Self::Install => "install",
            Self::Cleanup => "cleanup",
            Self::Latest => "latest",
        }
    }

    /// Build and install make the outputs, run the dependencies first, and
    /// leave a result Recipe keeps.
    pub fn builds(self) -> bool {
        matches!(self, Self::Build | Self::Install)
    }
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Most dependencies (`deps` and `build_deps`) one request names.
pub const MAX_DEPENDENCIES: usize = 64;
/// Most outputs a recipe declares (`NEEDS.outputs`).
pub const MAX_OUTPUTS: usize = 64;

/// The request file: start one invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerRequest {
    /// [`RECORDS_VERSION`].
    pub version: u32,
    /// `[A-Za-z0-9_-]{1,64}`, e.g. `<recipe-name>-<unix-millis>-<pid>`.
    pub invocation_id: String,
    /// The recipe's name, its file name without `.rhai`, as the coordinator
    /// read it from the real file (`crate::needs::Declaration::name`). The
    /// recipe is staged as [`crate::domain::MAIN_RECIPE`]; the worker reads
    /// its declarations under this name, never under the staged file name.
    pub recipe_name: String,
    /// Engine operation.
    pub operation: Operation,
    /// Explicit invocation options; never inferred from coordinator environment.
    pub options: InvocationOptions,
    /// Each dependency of the recipe (`deps`, `build_deps`) by its name, with
    /// the output names it declares. The worker builds `DEPS` from it: a
    /// dependency with one output is that output's staged path, one with
    /// several a map by output name (docs/recipe-format.md, decision 16). An
    /// output the coordinator did not stage (it did not run the dependency,
    /// or a runtime dependency's `system` output in a system change) is `()`.
    pub dependencies: std::collections::BTreeMap<String, Vec<String>>,
    /// Present exactly for a host-mode worker (`crate::execution`). Its
    /// directories are below [`crate::domain::ENV_HOST_ROOT`]. Absent, and
    /// not serialized, for a Ginux worker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<HostRun>,
    /// The network rule the worker installs for Recipe's own downloads
    /// (`crate::helpers::acquire::net`), in either runtime. A host-mode
    /// worker without it reaches no endpoint at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkGrant>,
    /// The offline cache directory the worker reads (`crate::offline_cache`),
    /// read-only: a host path in a host run,
    /// [`crate::domain::OFFLINE_CACHE`] in a Ginux run, where it holds the
    /// cached files of the recipe's declared pins
    /// (`crate::inputs::inject_archive`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<String>,
}

/// Marks a host-mode worker's request. A host run's other rules (its
/// directories, its network) come from the environment and [`NetworkGrant`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRun {}

/// What Recipe's own HTTP client may reach: each declared `HOST:PORT` to the
/// addresses the coordinator pinned for it, and the run's network byte cap
/// (`crate::needs::Needs::network_budget`). In a Ginux run the domain admits
/// any address on a granted port (the shared network); this keeps the
/// worker's own downloads to the pins and the cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkGrant {
    pub pins: std::collections::BTreeMap<String, Vec<String>>,
    pub max_bytes: u64,
}

fn check_network(network: &NetworkGrant) -> Result<(), RecordError> {
    if network.pins.len() > crate::domain::MAX_NETWORK_ENDPOINTS
        || network
            .pins
            .values()
            .any(|pins| pins.is_empty() || pins.len() > crate::domain::MAX_NETWORK_ENDPOINTS)
        || network.max_bytes == 0
    {
        return Err(RecordError::invalid("invalid network grant"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub struct InvocationOptions {
    pub defines: Vec<(String, String)>,
    pub execution: crate::ExecutionOptions,
}

impl InvocationOptions {
    pub fn validate(&self) -> Result<(), RecordError> {
        self.execution
            .validate()
            .map_err(|error| RecordError::invalid(error.to_string()))?;
        if self.defines.len() > 64 {
            return Err(RecordError::invalid(
                "invocation options exceed their bounds",
            ));
        }
        let mut names = std::collections::HashSet::new();
        for (name, value) in &self.defines {
            if name.is_empty()
                || name.len() > 128
                || value.len() > 65536
                || !name.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_alphabetic() || i > 0 && b.is_ascii_digit()
                })
                || !names.insert(name)
            {
                return Err(RecordError::invalid(format!(
                    "invalid or duplicate define {name:?}"
                )));
            }
        }
        Ok(())
    }
}

impl WorkerRequest {
    fn check(&self) -> Result<(), RecordError> {
        if self.version != RECORDS_VERSION {
            return Err(RecordError::invalid(format!(
                "request version {}, want {RECORDS_VERSION}",
                self.version
            )));
        }
        self.options.validate()?;
        if self.dependencies.len() > MAX_DEPENDENCIES
            || self.dependencies.iter().any(|(name, outputs)| {
                crate::recipes::dependency_file(name).is_err()
                    || outputs.len() > MAX_OUTPUTS
                    || outputs
                        .iter()
                        .any(|output| !crate::domain::valid_name(output))
            })
        {
            return Err(RecordError::invalid("invalid dependency outputs"));
        }
        if let Some(network) = &self.network {
            check_network(network)?;
        }
        if self.cache.as_ref().is_some_and(|cache| {
            !crate::domain::valid_host_path(cache)
                || self.host.is_none() && cache != crate::domain::OFFLINE_CACHE
        }) {
            return Err(RecordError::invalid("invalid offline cache directory"));
        }
        if self.recipe_name.is_empty()
            || self.recipe_name.len() > crate::domain::MAX_INPUT_NAME
            || self.recipe_name.contains('/')
            || self.recipe_name.chars().any(char::is_control)
        {
            return Err(RecordError::invalid(format!(
                "request recipe_name {:?} must be a file name of 1..={} B without `/` or control characters",
                self.recipe_name,
                crate::domain::MAX_INPUT_NAME
            )));
        }
        if !crate::domain::valid_name(&self.invocation_id) {
            return Err(RecordError::invalid(format!(
                "request invocation_id {:?} must match [A-Za-z0-9_-]{{1,{}}}",
                self.invocation_id,
                crate::domain::MAX_NAME
            )));
        }
        Ok(())
    }

    /// The request file's bytes, checked and bounded by
    /// [`crate::domain::MAX_REQUEST_BYTES`].
    pub fn encode(&self) -> Result<Vec<u8>, RecordError> {
        self.check()?;
        encode_bounded(self, crate::domain::MAX_REQUEST_BYTES)
    }

    /// Parse a request file's bytes strictly.
    pub fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        let request: Self = decode_bounded(bytes, crate::domain::MAX_REQUEST_BYTES, "request")?;
        request.check()?;
        Ok(request)
    }
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

/// What failed ([`FailureInfo::kind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// A step failed (`download` or `build`, which includes `verify`).
    Phase,
    /// The operation failed outside a step: compiling or setting up the
    /// recipe, a dependency, top-level code, `cleanup` or `latest`.
    Recipe,
    /// The worker could not run the request it was given.
    Request,
}

/// Why an invocation failed ([`WorkerResult::failure`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureInfo {
    pub kind: FailureKind,
    /// The failed phase; present exactly for [`FailureKind::Phase`].
    pub phase: Option<crate::lifecycle::Phase>,
    pub message: String,
}

impl FailureInfo {
    /// The reason text: a failed phase's cleanup reason, otherwise the kind.
    pub fn reason(&self) -> &'static str {
        match (self.kind, self.phase) {
            (FailureKind::Phase, Some(phase)) => {
                crate::lifecycle::CleanupReason::Failed(phase).as_str()
            }
            (FailureKind::Recipe, _) => "recipe",
            (FailureKind::Request, _) => "request",
            (FailureKind::Phase, None) => "phase",
        }
    }
}

impl fmt::Display for FailureInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let phase = self.phase.map_or("-", crate::lifecycle::Phase::name);
        write!(
            f,
            "phase={phase} reason={}: {}",
            self.reason(),
            self.message
        )
    }
}

/// The result file: the worker's final record. Written once, atomically,
/// then the worker exits 0. An exit without it is a `protocol` failure
/// (or what the run's outcome says it is).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerResult {
    /// [`RECORDS_VERSION`].
    pub version: u32,
    /// The request's invocation id: a result from another run is not this
    /// run's result.
    pub invocation_id: String,
    pub ok: bool,
    /// Present exactly when `ok` is false.
    pub failure: Option<FailureInfo>,
    /// A successful build or install: every declared output to the domain
    /// path the engine placed it at ([`crate::domain::output_path`]). Empty
    /// otherwise.
    pub outputs: std::collections::BTreeMap<String, String>,
    /// The steps the worker worked on, with their status (`download`, and
    /// `build` or `install` for the step that builds the outputs). The
    /// coordinator adds the steps it does itself.
    pub steps: Vec<crate::core::events::StepReport>,
    /// A successful `latest`: the version `latest()` returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    /// The worker's account of each declared endpoint for Recipe's own
    /// downloads; present when the request carried a network rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_usage:
        Option<std::collections::BTreeMap<String, crate::helpers::acquire::net::EndpointUse>>,
}

impl WorkerResult {
    fn check(&self) -> Result<(), RecordError> {
        if self.version != RECORDS_VERSION {
            return Err(RecordError::invalid(format!(
                "result version {}, want {RECORDS_VERSION}",
                self.version
            )));
        }
        if !crate::domain::valid_name(&self.invocation_id) {
            return Err(RecordError::invalid("invalid result invocation id"));
        }
        if self.ok == self.failure.is_some() {
            return Err(RecordError::invalid(
                "a result carries a failure exactly when it is not ok",
            ));
        }
        if !self.ok && (!self.outputs.is_empty() || self.latest.is_some()) {
            return Err(RecordError::invalid(
                "a failed result names no outputs and no version",
            ));
        }
        if self.outputs.len() > MAX_OUTPUTS
            || self.outputs.iter().any(|(name, path)| {
                !crate::domain::valid_name(name) || *path != crate::domain::output_path(name)
            })
        {
            return Err(RecordError::invalid(
                "a result names each output at its engine-placed path",
            ));
        }
        if self.steps.len() > 4
            || self.steps.iter().any(|step| {
                !["download", "build", "install", "cleanup"].contains(&step.step.as_str())
            })
        {
            return Err(RecordError::invalid(
                "a result names only the steps download, build, install and cleanup",
            ));
        }
        if self.latest.as_ref().is_some_and(|version| {
            version.is_empty() || version.len() > 256 || version.chars().any(char::is_control)
        }) {
            return Err(RecordError::invalid(
                "latest() returns a version of 1 to 256 characters",
            ));
        }
        if let Some(failure) = &self.failure
            && failure.phase.is_some() != (failure.kind == FailureKind::Phase)
        {
            return Err(RecordError::invalid(
                "a failure names a phase exactly when a phase failed",
            ));
        }
        Ok(())
    }

    /// The result file's bytes, checked and bounded by
    /// [`crate::domain::MAX_RESULT_BYTES`].
    pub fn encode(&self) -> Result<Vec<u8>, RecordError> {
        self.check()?;
        encode_bounded(self, crate::domain::MAX_RESULT_BYTES)
    }

    /// Parse a result file's bytes strictly: the shape, every field rule, and
    /// the invocation it must belong to.
    pub fn decode(bytes: &[u8], invocation_id: &str) -> Result<Self, RecordError> {
        let result: Self = decode_bounded(bytes, crate::domain::MAX_RESULT_BYTES, "result")?;
        result.check()?;
        if result.invocation_id != invocation_id {
            return Err(RecordError::invalid(format!(
                "the result belongs to invocation {:?}, not {invocation_id:?}",
                result.invocation_id
            )));
        }
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Lines
// ---------------------------------------------------------------------------

/// Human bytes only: no lifecycle, output-path or result authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStream {
    Stdout,
    Stderr,
    Recipe,
}

impl From<crate::diagnostics::Stream> for DiagnosticStream {
    fn from(stream: crate::diagnostics::Stream) -> Self {
        match stream {
            crate::diagnostics::Stream::Recipe => Self::Recipe,
            crate::diagnostics::Stream::Stdout => Self::Stdout,
            crate::diagnostics::Stream::Stderr => Self::Stderr,
        }
    }
}

/// The invocation's diagnostic budget. The worker uses it to chunk output and
/// to emit the single truncation marker; the coordinator uses the same rules
/// to verify the records it reads.
#[derive(Debug, Default)]
pub struct DiagnosticBudget {
    bytes: usize,
    records: usize,
    truncated: bool,
}

impl DiagnosticBudget {
    /// Worker: the next record for `bytes` (consuming what it carries), or
    /// the truncation marker once the budget is spent. `None` after truncation.
    pub fn next(
        &mut self,
        stream: DiagnosticStream,
        bytes: &mut &[u8],
    ) -> Option<DiagnosticRecord> {
        if self.truncated {
            return None;
        }
        let room = MAX_DIAGNOSTIC_BYTES - self.bytes;
        if room == 0 || self.records == MAX_DIAGNOSTIC_RECORDS {
            self.truncated = true;
            return Some(DiagnosticRecord {
                stream,
                data: Vec::new(),
                truncated: true,
            });
        }
        let take = room.min(MAX_DIAGNOSTIC_CHUNK).min(bytes.len());
        let data = bytes[..take].to_vec();
        *bytes = &bytes[take..];
        self.bytes += take;
        self.records += 1;
        Some(DiagnosticRecord {
            stream,
            data,
            truncated: false,
        })
    }

    /// Coordinator: account for a received record under the same rules.
    pub fn accept(&mut self, record: &DiagnosticRecord) -> Result<(), &'static str> {
        if self.truncated {
            return Err("diagnostic after truncation");
        }
        if record.truncated {
            if self.bytes != MAX_DIAGNOSTIC_BYTES && self.records != MAX_DIAGNOSTIC_RECORDS {
                return Err("premature diagnostic truncation");
            }
            self.truncated = true;
            return Ok(());
        }
        self.bytes += record.data.len();
        self.records += 1;
        if self.bytes > MAX_DIAGNOSTIC_BYTES || self.records > MAX_DIAGNOSTIC_RECORDS {
            return Err("diagnostic budget exceeded");
        }
        Ok(())
    }
}

/// Bytes the worker or its programs wrote for a human. `data` is the raw
/// bytes (a JSON array of numbers), so output that is not UTF-8 reaches
/// `worker.log` unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticRecord {
    pub stream: DiagnosticStream,
    pub data: Vec<u8>,
    /// Exactly one empty marker after the invocation byte/record budget is full.
    pub truncated: bool,
}

impl DiagnosticRecord {
    fn check(&self) -> bool {
        self.data.len() <= MAX_DIAGNOSTIC_CHUNK && self.data.is_empty() == self.truncated
    }
}

/// How far the running recipe says it is, for the coordinator's
/// `--progress-file`. Carries no authority and is outside the diagnostic
/// budget; the worker writes at most a few per second.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressRecord {
    pub recipe: String,
    /// The lifecycle step running (`download`, `build`, `install`, ...).
    pub phase: String,
    /// What is being counted (`packages`); may be empty.
    pub what: String,
    pub done: u64,
    /// 0 when unknown.
    pub total: u64,
}

const MAX_PROGRESS_TEXT: usize = 256;

impl ProgressRecord {
    fn check(&self) -> bool {
        self.recipe.len() <= MAX_PROGRESS_TEXT
            && self.phase.len() <= MAX_PROGRESS_TEXT
            && self.what.len() <= MAX_PROGRESS_TEXT
    }
}

/// One line of the worker's standard output:
/// `{"version":14,"diagnostic":{...}}` or `{"version":14,"progress":{...}}`,
/// exactly one of the two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineRecord {
    /// [`RECORDS_VERSION`].
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<DiagnosticRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<ProgressRecord>,
}

impl LineRecord {
    pub fn diagnostic(record: DiagnosticRecord) -> Self {
        Self {
            version: RECORDS_VERSION,
            diagnostic: Some(record),
            progress: None,
        }
    }

    pub fn progress(record: ProgressRecord) -> Self {
        Self {
            version: RECORDS_VERSION,
            diagnostic: None,
            progress: Some(record),
        }
    }

    fn check(&self) -> bool {
        self.version == RECORDS_VERSION
            && match (&self.diagnostic, &self.progress) {
                (Some(diagnostic), None) => diagnostic.check(),
                (None, Some(progress)) => progress.check(),
                _ => false,
            }
    }

    /// The line's bytes, newline included.
    pub fn encode(&self) -> Result<Vec<u8>, RecordError> {
        if !self.check() {
            return Err(RecordError::invalid("invalid line record"));
        }
        let mut line = serde_json::to_vec(self)
            .map_err(|error| RecordError::invalid(format!("cannot serialize a line: {error}")))?;
        line.push(b'\n');
        if line.len() > MAX_LINE_BYTES {
            return Err(RecordError::invalid(format!(
                "a line record exceeds {MAX_LINE_BYTES} bytes"
            )));
        }
        Ok(line)
    }

    /// The record one line (without its newline) holds, or None when it is
    /// plain text.
    pub fn parse(line: &[u8]) -> Option<Self> {
        if line.len() >= MAX_LINE_BYTES || line.first() != Some(&b'{') {
            return None;
        }
        let record: Self = serde_json::from_slice(line).ok()?;
        record.check().then_some(record)
    }
}

// ---------------------------------------------------------------------------
// Errors and helpers
// ---------------------------------------------------------------------------

/// A record that breaks its shape or a field rule.
#[derive(Debug)]
pub struct RecordError(String);

impl RecordError {
    fn invalid(detail: impl Into<String>) -> Self {
        Self(detail.into())
    }
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "worker record invalid: {}", self.0)
    }
}

impl std::error::Error for RecordError {}

fn encode_bounded(record: &impl Serialize, max: u64) -> Result<Vec<u8>, RecordError> {
    let bytes = serde_json::to_vec(record)
        .map_err(|error| RecordError::invalid(format!("cannot serialize a record: {error}")))?;
    if bytes.len() as u64 > max {
        return Err(RecordError::invalid(format!(
            "the record has {} bytes, more than {max}",
            bytes.len()
        )));
    }
    Ok(bytes)
}

fn decode_bounded<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    max: u64,
    what: &str,
) -> Result<T, RecordError> {
    if bytes.len() as u64 > max {
        return Err(RecordError::invalid(format!(
            "the {what} has more than {max} bytes"
        )));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|error| RecordError::invalid(format!("the {what} is not UTF-8: {error}")))?;
    serde_json::from_str(text)
        .map_err(|error| RecordError::invalid(format!("the {what} is malformed: {error}")))
}

/// Read at most `max` bytes of `reader`; more is an error, never a silent
/// truncation.
pub fn read_bounded(reader: impl std::io::Read, max: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    reader.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(std::io::Error::other(format!(
            "the record exceeds its bound of {max} bytes"
        )));
    }
    Ok(bytes)
}
