//! Worker protocol v6: per-recipe state, invocation options and diagnostics.
//!
//! Implements `docs/ginux-recipe-interfaces.md` §4 on the Recipe side. Ginux
//! treats these frames as opaque bytes; Recipe owns their shape.
//!
//! ## Wire format
//!
//! Every frame is:
//!
//! ```text
//! [u32 BE total-length][u32 BE version=6][u32 BE seq][u16 BE op][payload]
//! ```
//!
//! - `total-length` is the full frame size **including** its own 4 bytes, so
//!   `payload.len() == total-length - HEADER_LEN` with
//!   `HEADER_LEN == 14` (4 + 4 + 4 + 2).
//! - `version` is always [`PROTOCOL_VERSION`] (6). Anything else is a
//!   `protocol` failure: the receiver aborts, never retries on the same
//!   invocation.
//! - `seq` numbers each direction independently starting at 1. The receiver
//!   expects exactly the next sequence number; any gap or replay is a
//!   `protocol` failure.
//! - `op` selects the payload schema: 1 = request, 2 = checkpoint, 3 = ack,
//!   4 = failure, 5 = result, 6 = diagnostic.
//! - `payload` is JSON, UTF-8, and together with the header the frame is at
//!   most [`MAX_FRAME_LEN`] (4 MiB) total.
//!
//! ## Payload rules
//!
//! Payloads carry no host paths: `invocation_id` values follow the
//! `[A-Za-z0-9_-]{1,64}` shape. Checkpoints name a canonical `/inputs` recipe
//! and its JSON object `state`; requests carry matching per-recipe saved state.
//! The coordinator owns all authoritative host destinations. Final contexts
//! are JSON objects, with named outputs validated separately before publication.
//!
//! ## Use
//!
//! - Sending side: keep one [`FrameSender`] per direction. It assigns seq
//!   numbers starting at 1. Call [`FrameSender::send`] with a
//!   [`FramePayload`] and a `Write` (the protocol pipe).
//! - Receiving side: keep one [`FrameReceiver`] per direction. It enforces
//!   seq continuity starting at 1. Call [`FrameReceiver::recv`] with a `Read`.
//! - Single-shot (datagram) use: [`encode_message`] / [`decode_message`]
//!   with an explicit `expected_seq`.
//! - Direction check: [`Op::direction`] plus [`Frame::check_direction`] let
//!   the host side accept only worker→host ops and vice versa.
//!
//! Every rejection is an explicit [`ProtocolError`]. Reads use exact-length
//! I/O: short or truncated traffic is an error, never silent truncation.

use std::fmt;
use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

/// Protocol version carried in every frame header.
pub const PROTOCOL_VERSION: u32 = 6;

/// Maximum total frame size in bytes (4 MiB), header included.
pub const MAX_FRAME_LEN: usize = 4 * 1024 * 1024;

/// Header size in bytes: u32 len + u32 version + u32 seq + u16 op.
pub const HEADER_LEN: usize = 14;

/// First sequence number in each direction.
pub const FIRST_SEQ: u32 = 1;

/// Keep a diagnostic frame below the configured console IO bound even when
/// every byte requires four JSON characters. The full transport remains 4 MiB.
pub const MAX_DIAGNOSTIC_CHUNK: usize = 768;
pub const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_DIAGNOSTIC_FRAMES: usize = 32768;
const TRANSPORT_IO_CHUNK: usize = 4096;

// ---------------------------------------------------------------------------
// Ops
// ---------------------------------------------------------------------------

/// Frame operation codes: 1=request, 2=checkpoint, 3=ack, 4=failure,
/// 5=result, 6=diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Op {
    Request = 1,
    Checkpoint = 2,
    Ack = 3,
    Failure = 4,
    Result = 5,
    Diagnostic = 6,
}

impl Op {
    pub fn name(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Checkpoint => "checkpoint",
            Self::Ack => "ack",
            Self::Failure => "failure",
            Self::Result => "result",
            Self::Diagnostic => "diagnostic",
        }
    }

    /// Decode an op code; unknown codes are a protocol failure.
    pub fn from_u16(v: u16) -> Result<Self, ProtocolError> {
        match v {
            1 => Ok(Op::Request),
            2 => Ok(Op::Checkpoint),
            3 => Ok(Op::Ack),
            4 => Ok(Op::Failure),
            5 => Ok(Op::Result),
            6 => Ok(Op::Diagnostic),
            other => Err(ProtocolError::UnknownOp(other)),
        }
    }

    /// Which direction of the pipe this op travels.
    pub fn direction(self) -> Direction {
        match self {
            Op::Request | Op::Ack => Direction::HostToWorker,
            Op::Checkpoint | Op::Failure | Op::Result | Op::Diagnostic => Direction::WorkerToHost,
        }
    }
}

/// One direction of the bidirectional protocol pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Host → worker: request, ack.
    HostToWorker,
    /// Worker → host: diagnostic, checkpoint, failure, result.
    WorkerToHost,
}

// ---------------------------------------------------------------------------
// Payloads (plain serializable types, §4 field-for-field)
// ---------------------------------------------------------------------------

/// The engine operation one invocation runs. Serializes as the check or
/// lifecycle name (`install`, `is_built`, …), which is also its wire form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Install,
    Remove,
    Cleanup,
    IsInstalled,
    IsBuilt,
    IsAcquired,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Remove => "remove",
            Self::Cleanup => "cleanup",
            Self::IsInstalled => "is_installed",
            Self::IsBuilt => "is_built",
            Self::IsAcquired => "is_acquired",
        }
    }

    /// Only install runs phases, so only install accepts force options.
    pub fn accepts_force(self) -> bool {
        self == Self::Install
    }
}

impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A lifecycle phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Acquire,
    Build,
    Install,
}

/// Machine-readable reason carried by cleanup hooks, checkpoints and failures.
///
/// The phase variants are also the `reason` argument recipes receive in
/// `cleanup(ctx, reason)`, so their text is part of the recipe API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Succeeded(Phase),
    Failed(Phase),
    /// A user-requested cleanup.
    Manual,
    Fatal,
    Workload,
    Startup,
    Protocol,
}

impl Reason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded(Phase::Acquire) => "auto.acquire.success",
            Self::Succeeded(Phase::Build) => "auto.build.success",
            Self::Succeeded(Phase::Install) => "auto.install.success",
            Self::Failed(Phase::Acquire) => "auto.acquire.failure",
            Self::Failed(Phase::Build) => "auto.build.failure",
            Self::Failed(Phase::Install) => "auto.install.failure",
            Self::Manual => "manual",
            Self::Fatal => "fatal",
            Self::Workload => "workload",
            Self::Startup => "startup",
            Self::Protocol => "protocol",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [
            Self::Succeeded(Phase::Acquire),
            Self::Succeeded(Phase::Build),
            Self::Succeeded(Phase::Install),
            Self::Failed(Phase::Acquire),
            Self::Failed(Phase::Build),
            Self::Failed(Phase::Install),
            Self::Manual,
            Self::Fatal,
            Self::Workload,
            Self::Startup,
            Self::Protocol,
        ]
        .into_iter()
        .find(|reason| reason.as_str() == text)
    }

    /// Autofix repairs failed build or install phases; acquisition failures
    /// come from inputs a patch cannot fix.
    pub fn autofix_eligible(self) -> bool {
        matches!(self, Self::Failed(Phase::Build | Phase::Install))
    }
}

/// Largest saved state the next request can carry for these options: the
/// frame bound minus a request with the same options and the longest id and
/// cleanup reason the protocol allows.
pub fn restored_state_budget(options: &InvocationOptions) -> Result<usize, ProtocolError> {
    let request = FramePayload::Request(RequestPayload {
        invocation_id: "x".repeat(MAX_INVOCATION_ID),
        operation: Operation::Install,
        persist_state: true,
        recipe_digest: "0".repeat(64),
        options: InvocationOptions {
            cleanup_reason: "x".repeat(MAX_CLEANUP_REASON),
            ..options.clone()
        },
        restored_states: Default::default(),
    });
    Ok(MAX_FRAME_LEN.saturating_sub(encode_message(u32::MAX, &request)?.len()))
}

/// Longest cleanup reason a request may carry.
pub const MAX_CLEANUP_REASON: usize = 4096;

/// Op 1 (host→worker): start one invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestPayload {
    /// `[A-Za-z0-9_-]{1,64}`, e.g. `<recipe-name>-<unix-millis>-<pid>`.
    pub invocation_id: String,
    /// Engine operation.
    pub operation: Operation,
    /// Caller persist flag (`--no-persist-state` negated).
    pub persist_state: bool,
    /// Digest of the recipe inputs (opaque string, no path).
    pub recipe_digest: String,
    /// Explicit invocation options; never inferred from coordinator environment.
    pub options: InvocationOptions,
    /// Matching per-recipe runtime state; the coordinator restores its files.
    pub restored_states: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationOptions {
    pub defines: Vec<(String, String)>,
    pub cleanup_reason: String,
    pub execution: crate::ExecutionOptions,
}

impl Default for InvocationOptions {
    fn default() -> Self {
        Self {
            defines: Vec::new(),
            cleanup_reason: Reason::Manual.as_str().into(),
            execution: crate::ExecutionOptions::default(),
        }
    }
}

impl InvocationOptions {
    /// [`Self::validate`], plus the rules that depend on the operation.
    pub fn validate_for(&self, operation: Operation) -> Result<(), ProtocolError> {
        self.validate()?;
        if !operation.accepts_force() && self.execution.forced() {
            return Err(ProtocolError::PayloadInvalid(
                "force options apply only to install operations".into(),
            ));
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        self.execution
            .validate()
            .map_err(|error| ProtocolError::PayloadInvalid(error.to_string()))?;
        if self.defines.len() > 64 || self.cleanup_reason.len() > MAX_CLEANUP_REASON {
            return Err(ProtocolError::PayloadInvalid(
                "invocation options exceed their bounds".into(),
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
                return Err(ProtocolError::PayloadInvalid(format!(
                    "invalid or duplicate define {name:?}"
                )));
            }
        }
        Ok(())
    }
}

/// Op 2 (worker→host): report one persistence point; the host quiesces
/// storage, persists `ctx`, then answers with [`AckPayload`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointPayload {
    /// Checkpoint sequence for the `{seq → generation}` association.
    pub seq: u64,
    /// Engine phase that just completed (e.g. `build`).
    pub phase: String,
    /// Persistence-point label (e.g. `phase:build`, `cleanup:manual`).
    pub reason: String,
    /// Canonical staged recipe identity, never a host state destination.
    pub recipe: String,
    /// Only resumable runtime state; package facts and paths are initialized anew.
    pub state: serde_json::Value,
}

/// Op 3 (host→worker): acknowledge a checkpoint with a durable generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AckPayload {
    /// Checkpoint `seq` being acknowledged.
    pub seq: u64,
    /// Durable storage generation now backing that checkpoint.
    pub generation: u64,
}

/// Op 4 (worker→host): structured failure instead of (or before) a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailurePayload {
    /// True when the invocation cannot continue (no further frames but
    /// `result` still terminates the stream).
    pub fatal: bool,
    /// Phase that failed (empty when the failure is not phase-scoped).
    pub phase: String,
    /// Machine-readable reason.
    pub reason: String,
    /// Human-readable detail (goes to logs, never parsed).
    pub message: String,
}

/// One entry of [`ResultPayload::failures`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureInfo {
    pub phase: String,
    pub reason: String,
    pub message: String,
}

/// Op 5 (worker→host): final record. Sent exactly once, then the worker
/// exits 0. Exit without it is a `protocol`/`workload` failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultPayload {
    pub ok: bool,
    pub failures: Vec<FailureInfo>,
    /// Summary of the final `ctx` (plain JSON, no paths required).
    pub ctx_summary: serde_json::Value,
}

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
/// to emit the single truncation marker; the host uses the same rules to
/// verify the stream.
#[derive(Debug, Default)]
pub struct DiagnosticBudget {
    bytes: usize,
    frames: usize,
    truncated: bool,
}

impl DiagnosticBudget {
    /// Worker: the next payload for `bytes` (consuming what it carries), or
    /// the truncation marker once the budget is spent. `None` after truncation.
    pub fn next(&mut self, stream: DiagnosticStream, bytes: &mut &[u8]) -> Option<DiagnosticPayload> {
        if self.truncated {
            return None;
        }
        let room = MAX_DIAGNOSTIC_BYTES - self.bytes;
        if room == 0 || self.frames == MAX_DIAGNOSTIC_FRAMES {
            self.truncated = true;
            return Some(DiagnosticPayload {
                stream,
                data: Vec::new(),
                truncated: true,
            });
        }
        let take = room.min(MAX_DIAGNOSTIC_CHUNK).min(bytes.len());
        let data = bytes[..take].to_vec();
        *bytes = &bytes[take..];
        self.bytes += take;
        self.frames += 1;
        Some(DiagnosticPayload {
            stream,
            data,
            truncated: false,
        })
    }

    /// Host: account for a received payload under the same rules.
    pub fn accept(&mut self, payload: &DiagnosticPayload) -> Result<(), &'static str> {
        if self.truncated {
            return Err("diagnostic after truncation");
        }
        if payload.truncated {
            if self.bytes != MAX_DIAGNOSTIC_BYTES && self.frames != MAX_DIAGNOSTIC_FRAMES {
                return Err("premature diagnostic truncation");
            }
            self.truncated = true;
            return Ok(());
        }
        self.bytes += payload.data.len();
        self.frames += 1;
        if self.bytes > MAX_DIAGNOSTIC_BYTES || self.frames > MAX_DIAGNOSTIC_FRAMES {
            return Err("diagnostic budget exceeded");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticPayload {
    pub stream: DiagnosticStream,
    pub data: Vec<u8>,
    /// Exactly one empty marker after the invocation byte/frame budget is full.
    pub truncated: bool,
}

fn check_diagnostic(p: &DiagnosticPayload) -> Result<(), ProtocolError> {
    if p.data.len() > MAX_DIAGNOSTIC_CHUNK || p.data.is_empty() != p.truncated {
        return Err(ProtocolError::PayloadInvalid(
            format!(
                "diagnostic needs 1..={MAX_DIAGNOSTIC_CHUNK} bytes, or one empty truncation marker"
            ),
        ));
    }
    Ok(())
}

/// A typed frame payload. The op code is derived from the variant, so an
/// encoded frame can never carry a mismatched op/payload pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FramePayload {
    Request(RequestPayload),
    Checkpoint(CheckpointPayload),
    Ack(AckPayload),
    Failure(FailurePayload),
    Result(ResultPayload),
    Diagnostic(DiagnosticPayload),
}

impl FramePayload {
    pub fn op(&self) -> Op {
        match self {
            FramePayload::Request(_) => Op::Request,
            FramePayload::Checkpoint(_) => Op::Checkpoint,
            FramePayload::Ack(_) => Op::Ack,
            FramePayload::Failure(_) => Op::Failure,
            FramePayload::Result(_) => Op::Result,
            FramePayload::Diagnostic(_) => Op::Diagnostic,
        }
    }
}

/// A fully decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub seq: u32,
    pub op: Op,
    pub payload: FramePayload,
}

impl Frame {
    /// Reject frames arriving from the wrong side of the pipe.
    pub fn check_direction(&self, expected: Direction) -> Result<(), ProtocolError> {
        let actual = self.op.direction();
        if actual == expected {
            Ok(())
        } else {
            Err(ProtocolError::WrongDirection {
                op: self.op,
                expected,
                actual,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Errors: every failure mode is explicit, never silent truncation
// ---------------------------------------------------------------------------

/// Protocol failure. Any of these aborts the invocation on the receiving
/// side with failure category `protocol`; no retry on the same invocation.
#[derive(Debug)]
pub enum ProtocolError {
    /// Underlying transport I/O failed (other than a clean EOF, see `Closed`).
    Io(std::io::Error),
    /// Peer closed the pipe. At a frame boundary this means orderly
    /// shutdown; anywhere else the frame is truncated (see `Truncated`).
    /// After `result` the host should expect exactly this.
    Closed,
    /// Declared frame larger than [`MAX_FRAME_LEN`]. Rejected before any
    /// payload allocation.
    Oversize { declared: u64 },
    /// Declared frame shorter than [`HEADER_LEN`]; no valid payload fits.
    Undersize { declared: u32 },
    /// Fewer bytes arrived than the frame declares. Never silently accepted.
    Truncated { detail: String },
    /// Header version differs from PROTOCOL_VERSION.
    VersionMismatch { got: u32 },
    /// `seq` is not exactly the next expected number (gap or replay).
    SeqMismatch { expected: u32, got: u32 },
    /// Unknown op code.
    UnknownOp(u16),
    /// Payload is not valid UTF-8 JSON, has the wrong shape, or breaks a
    /// field rule (bad `invocation_id`, non-object `*_ctx`, ...).
    PayloadInvalid(String),
    /// Trailing bytes after one complete frame in [`decode_message`].
    TrailingBytes { frame_len: usize, buffer_len: usize },
    /// Frame arrived from the wrong side of the pipe.
    WrongDirection {
        op: Op,
        expected: Direction,
        actual: Direction,
    },
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::Io(e) => write!(f, "protocol transport I/O: {e}"),
            ProtocolError::Closed => write!(f, "protocol peer closed the pipe"),
            ProtocolError::Oversize { declared } => write!(
                f,
                "protocol frame oversize: declared {declared} bytes, limit {MAX_FRAME_LEN}"
            ),
            ProtocolError::Undersize { declared } => write!(
                f,
                "protocol frame undersize: declared {declared} bytes, header alone is {HEADER_LEN}"
            ),
            ProtocolError::Truncated { detail } => {
                write!(f, "protocol frame truncated: {detail}")
            }
            ProtocolError::VersionMismatch { got } => write!(
                f,
                "protocol version mismatch: got {got}, want {PROTOCOL_VERSION}"
            ),
            ProtocolError::SeqMismatch { expected, got } => write!(
                f,
                "protocol sequence mismatch: got seq {got}, want exactly {expected} (gap or replay)"
            ),
            ProtocolError::UnknownOp(op) => write!(f, "protocol unknown op: {op}"),
            ProtocolError::PayloadInvalid(detail) => {
                write!(f, "protocol payload invalid: {detail}")
            }
            ProtocolError::TrailingBytes {
                frame_len,
                buffer_len,
            } => write!(
                f,
                "protocol trailing bytes: frame is {frame_len} bytes, buffer holds {buffer_len}"
            ),
            ProtocolError::WrongDirection {
                op,
                expected,
                actual,
            } => write!(
                f,
                "protocol wrong direction: op {op:?} travels {actual:?}, expected {expected:?}"
            ),
        }
    }
}

impl std::error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ProtocolError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for ProtocolError {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            ProtocolError::Truncated {
                detail: format!("unexpected EOF: {e}"),
            }
        } else {
            ProtocolError::Io(e)
        }
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Longest invocation id. Ids travel in env, result records and host paths.
pub const MAX_INVOCATION_ID: usize = 64;

/// The one invocation-id rule: `[A-Za-z0-9_-]{1,64}`.
pub fn valid_invocation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_INVOCATION_ID
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn check_request(p: &RequestPayload) -> Result<(), ProtocolError> {
    p.options.validate_for(p.operation)?;
    if p.restored_states.len() > 1024 || (!p.persist_state && !p.restored_states.is_empty()) {
        return Err(ProtocolError::PayloadInvalid(
            "invalid restored state set".into(),
        ));
    }
    for (recipe, state) in &p.restored_states {
        if !valid_recipe_identity(recipe) || !state.is_object() {
            return Err(ProtocolError::PayloadInvalid(
                "invalid restored recipe state".into(),
            ));
        }
    }
    if !valid_invocation_id(&p.invocation_id) {
        return Err(ProtocolError::PayloadInvalid(format!(
            "request invocation_id {:?} must match [A-Za-z0-9_-]{{1,64}}",
            p.invocation_id
        )));
    }
    Ok(())
}

fn valid_recipe_identity(path: &str) -> bool {
    path.len() <= crate::domain::MAX_PATH
        && !path.contains('\0')
        && crate::domain::input_name(path).is_some_and(|name| {
            name.split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
        })
}

fn check_checkpoint(p: &CheckpointPayload) -> Result<(), ProtocolError> {
    if !valid_recipe_identity(&p.recipe) || !p.state.is_object() {
        return Err(ProtocolError::PayloadInvalid(
            "checkpoint requires a staged recipe identity and state object".into(),
        ));
    }
    Ok(())
}

fn serialize_payload(payload: &FramePayload) -> Result<Vec<u8>, ProtocolError> {
    let json = match payload {
        FramePayload::Request(p) => {
            check_request(p)?;
            serde_json::to_vec(p)
        }
        FramePayload::Checkpoint(p) => {
            check_checkpoint(p)?;
            serde_json::to_vec(p)
        }
        FramePayload::Ack(p) => serde_json::to_vec(p),
        FramePayload::Failure(p) => serde_json::to_vec(p),
        FramePayload::Result(p) => serde_json::to_vec(p),
        FramePayload::Diagnostic(p) => {
            check_diagnostic(p)?;
            serde_json::to_vec(p)
        }
    }
    .map_err(|e| ProtocolError::PayloadInvalid(format!("cannot serialize payload: {e}")))?;
    if HEADER_LEN.saturating_add(json.len()) > MAX_FRAME_LEN {
        return Err(ProtocolError::Oversize {
            declared: (HEADER_LEN + json.len()) as u64,
        });
    }
    Ok(json)
}

fn parse_payload(op: Op, seq: u32, json: &[u8]) -> Result<Frame, ProtocolError> {
    let text = std::str::from_utf8(json)
        .map_err(|e| ProtocolError::PayloadInvalid(format!("payload is not UTF-8: {e}")))?;
    let payload = match op {
        Op::Request => {
            let p: RequestPayload = serde_json::from_str(text).map_err(|e| {
                ProtocolError::PayloadInvalid(format!("request payload malformed: {e}"))
            })?;
            check_request(&p)?;
            FramePayload::Request(p)
        }
        Op::Checkpoint => {
            let p: CheckpointPayload = serde_json::from_str(text).map_err(|e| {
                ProtocolError::PayloadInvalid(format!("checkpoint payload malformed: {e}"))
            })?;
            check_checkpoint(&p)?;
            FramePayload::Checkpoint(p)
        }
        Op::Ack => {
            let p: AckPayload = serde_json::from_str(text).map_err(|e| {
                ProtocolError::PayloadInvalid(format!("ack payload malformed: {e}"))
            })?;
            FramePayload::Ack(p)
        }
        Op::Failure => {
            let p: FailurePayload = serde_json::from_str(text).map_err(|e| {
                ProtocolError::PayloadInvalid(format!("failure payload malformed: {e}"))
            })?;
            FramePayload::Failure(p)
        }
        Op::Diagnostic => {
            let p: DiagnosticPayload = serde_json::from_str(text).map_err(|e| {
                ProtocolError::PayloadInvalid(format!("diagnostic payload malformed: {e}"))
            })?;
            check_diagnostic(&p)?;
            FramePayload::Diagnostic(p)
        }
        Op::Result => {
            let p: ResultPayload = serde_json::from_str(text).map_err(|e| {
                ProtocolError::PayloadInvalid(format!("result payload malformed: {e}"))
            })?;
            FramePayload::Result(p)
        }
    };
    Ok(Frame { seq, op, payload })
}

// ---------------------------------------------------------------------------
// Encode / decode
// ---------------------------------------------------------------------------

/// Encode one frame with an explicit sequence number.
///
/// The op code is derived from `payload`, so header and body can never
/// disagree. Fails with [`ProtocolError::Oversize`] when the frame would
/// exceed [`MAX_FRAME_LEN`], or [`ProtocolError::PayloadInvalid`] when a
/// field rule breaks.
pub fn encode_message(seq: u32, payload: &FramePayload) -> Result<Vec<u8>, ProtocolError> {
    if seq == 0 {
        return Err(ProtocolError::SeqMismatch {
            expected: FIRST_SEQ,
            got: seq,
        });
    }
    let json = serialize_payload(payload)?;
    let op = payload.op() as u16;
    let total = (HEADER_LEN + json.len()) as u32;
    let mut out = Vec::with_capacity(total as usize);
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&op.to_be_bytes());
    out.extend_from_slice(&json);
    Ok(out)
}

/// Decode exactly one frame from `buf`.
///
/// `buf` must hold exactly the frame's bytes: short buffers are
/// [`ProtocolError::Truncated`], trailing bytes are
/// [`ProtocolError::TrailingBytes`]. `expected_seq` is the next sequence
/// number for this direction (starting at [`FIRST_SEQ`]); anything else is
/// [`ProtocolError::SeqMismatch`].
pub fn decode_message(buf: &[u8], expected_seq: u32) -> Result<Frame, ProtocolError> {
    if buf.len() < 4 {
        return Err(ProtocolError::Truncated {
            detail: format!("need 4 length bytes, buffer holds {}", buf.len()),
        });
    }
    let total = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    check_total(total)?;
    let total_usize = total as usize;
    if buf.len() < total_usize {
        return Err(ProtocolError::Truncated {
            detail: format!("frame declares {total} bytes, buffer holds {}", buf.len()),
        });
    }
    if buf.len() > total_usize {
        return Err(ProtocolError::TrailingBytes {
            frame_len: total_usize,
            buffer_len: buf.len(),
        });
    }
    let version = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if version != PROTOCOL_VERSION {
        return Err(ProtocolError::VersionMismatch { got: version });
    }
    let seq = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
    if seq != expected_seq {
        return Err(ProtocolError::SeqMismatch {
            expected: expected_seq,
            got: seq,
        });
    }
    let op = Op::from_u16(u16::from_be_bytes([buf[12], buf[13]]))?;
    parse_payload(op, seq, &buf[HEADER_LEN..])
}

fn check_total(total: u32) -> Result<usize, ProtocolError> {
    if total as u64 > MAX_FRAME_LEN as u64 {
        return Err(ProtocolError::Oversize {
            declared: total as u64,
        });
    }
    if (total as usize) < HEADER_LEN {
        return Err(ProtocolError::Undersize { declared: total });
    }
    Ok(total as usize)
}

// ---------------------------------------------------------------------------
// Streaming sender / receiver over the protocol pipes
// ---------------------------------------------------------------------------

/// Sending side of one direction. Assigns seq numbers starting at 1;
/// each `send` consumes the next one.
#[derive(Debug)]
pub struct FrameSender {
    next_seq: u32,
}

/// Receiving side of one direction. Expects seq numbers starting at 1;
/// each successful `recv` advances the expectation by one, so gaps and
/// replays fail.
#[derive(Debug)]
pub struct FrameReceiver {
    next_seq: u32,
}

impl FrameSender {
    pub fn new() -> Self {
        Self {
            next_seq: FIRST_SEQ,
        }
    }

    /// Encode `payload` with the next seq number and write the whole frame.
    /// Returns the seq number used. Short writes are retried to completion;
    /// transport errors surface as [`ProtocolError::Io`].
    pub fn send<W: Write>(
        &mut self,
        writer: &mut W,
        payload: &FramePayload,
    ) -> Result<u32, ProtocolError> {
        let seq = self.next_seq;
        let bytes = encode_message(seq, payload)?;
        write_all_complete(writer, &bytes)?;
        self.next_seq = self.next_seq.wrapping_add(1);
        if self.next_seq == 0 {
            // u32 wrap: seq 0 is never valid; stop instead of reusing numbers.
            return Err(ProtocolError::PayloadInvalid(
                "sequence space exhausted (u32 wrap)".to_string(),
            ));
        }
        Ok(seq)
    }
}

impl Default for FrameSender {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameReceiver {
    pub fn new() -> Self {
        Self {
            next_seq: FIRST_SEQ,
        }
    }

    /// Read exactly one frame: length prefix, then the declared body.
    ///
    /// A clean EOF before any length byte is [`ProtocolError::Closed`];
    /// EOF anywhere else is [`ProtocolError::Truncated`]. Oversize length
    /// prefixes are rejected before any payload allocation. On success the
    /// expected seq advances by one.
    pub fn recv<R: Read>(&mut self, reader: &mut R) -> Result<Frame, ProtocolError> {
        let mut len_buf = [0u8; 4];
        if !fill_or_eof(reader, &mut len_buf)? {
            return Err(ProtocolError::Closed);
        }
        let total = u32::from_be_bytes(len_buf);
        let total_usize = check_total(total)?;
        let mut rest = vec![0u8; total_usize - 4];
        read_exact_counted(reader, &mut rest, total_usize)?;
        let mut full = Vec::with_capacity(total_usize);
        full.extend_from_slice(&len_buf);
        full.extend_from_slice(&rest);
        let frame = decode_message(&full, self.next_seq)?;
        self.next_seq = self.next_seq.wrapping_add(1);
        Ok(frame)
    }
}

impl Default for FrameReceiver {
    fn default() -> Self {
        Self::new()
    }
}

fn write_all_complete<W: Write>(writer: &mut W, mut buf: &[u8]) -> Result<(), ProtocolError> {
    while !buf.is_empty() {
        match writer.write(&buf[..buf.len().min(TRANSPORT_IO_CHUNK)]) {
            Ok(0) => {
                return Err(ProtocolError::Io(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "protocol pipe accepted zero bytes",
                )));
            }
            Ok(n) => buf = &buf[n..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ProtocolError::Io(e)),
        }
    }
    Ok(())
}

/// Fill `buf`; returns false when EOF hits before the first byte.
fn fill_or_eof<R: Read>(reader: &mut R, mut buf: &mut [u8]) -> Result<bool, ProtocolError> {
    let mut first = true;
    while !buf.is_empty() {
        let take = buf.len().min(TRANSPORT_IO_CHUNK);
        match reader.read(&mut buf[..take]) {
            Ok(0) => {
                if first {
                    return Ok(false);
                }
                return Err(ProtocolError::Truncated {
                    detail: "EOF inside frame length prefix".to_string(),
                });
            }
            Ok(n) => {
                first = false;
                buf = &mut buf[n..];
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ProtocolError::Io(e)),
        }
    }
    Ok(true)
}

fn read_exact_counted<R: Read>(
    reader: &mut R,
    mut buf: &mut [u8],
    total: usize,
) -> Result<(), ProtocolError> {
    let want = buf.len();
    let mut got = 0usize;
    while !buf.is_empty() {
        let take = buf.len().min(TRANSPORT_IO_CHUNK);
        match reader.read(&mut buf[..take]) {
            Ok(0) => {
                return Err(ProtocolError::Truncated {
                    detail: format!(
                        "EOF inside frame body: frame declares {total} bytes, got {} of {want} body bytes",
                        got,
                    ),
                });
            }
            Ok(n) => {
                got += n;
                buf = &mut buf[n..];
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ProtocolError::Io(e)),
        }
    }
    Ok(())
}
