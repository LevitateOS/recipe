//! Worker protocol v2 framing and bounded diagnostics.
//!
//! Implements `docs/ginux-recipe-interfaces.md` §4 on the Recipe side. Ginux
//! treats these frames as opaque bytes; Recipe owns their shape.
//!
//! ## Wire format
//!
//! Every frame is:
//!
//! ```text
//! [u32 BE total-length][u32 BE version=2][u32 BE seq][u16 BE op][payload]
//! ```
//!
//! - `total-length` is the full frame size **including** its own 4 bytes, so
//!   `payload.len() == total-length - HEADER_LEN` with
//!   `HEADER_LEN == 14` (4 + 4 + 4 + 2).
//! - `version` is always [`PROTOCOL_VERSION`] (2). Anything else is a
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
//! `[A-Za-z0-9_-]{1,64}` shape, `base_path` names a recipe-side source (never
//! a host filesystem path), and `main_ctx` / `base_ctx` / `ctx_summary` are
//! plain JSON values (`*_ctx` must be JSON objects). Validation here rejects
//! malformed shapes; keeping host paths out is a caller contract documented
//! on each field.
//!
//! ## Use (the exact API R02 should call)
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
pub const PROTOCOL_VERSION: u32 = 2;

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
/// 5=result, 6=diagnostic (transport v2).
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

/// Op 1 (host→worker): start one invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestPayload {
    /// `[A-Za-z0-9_-]{1,64}`, e.g. `<recipe-name>-<unix-millis>-<pid>`.
    pub invocation_id: String,
    /// Engine operation: `install`, `remove`, `cleanup`, or a check name.
    pub operation: String,
    /// Caller persist flag (`--no-persist-ctx` negated).
    pub persist_ctx: bool,
    /// Digest of the recipe inputs (opaque string, no path).
    pub recipe_digest: String,
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
    /// Updated main-recipe `ctx` (must be a JSON object).
    pub main_ctx: serde_json::Value,
    /// Updated base-recipe `ctx` (must be a JSON object).
    pub base_ctx: serde_json::Value,
    /// Which base source this pairs with, if any. Never a host path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_path: Option<String>,
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
            "diagnostic needs 1..=768 bytes, or one empty truncation marker".into(),
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

fn valid_invocation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn check_request(p: &RequestPayload) -> Result<(), ProtocolError> {
    if !valid_invocation_id(&p.invocation_id) {
        return Err(ProtocolError::PayloadInvalid(format!(
            "request invocation_id {:?} must match [A-Za-z0-9_-]{{1,64}}",
            p.invocation_id
        )));
    }
    if p.operation.is_empty() {
        return Err(ProtocolError::PayloadInvalid(
            "request operation must not be empty".to_string(),
        ));
    }
    Ok(())
}

fn check_checkpoint(p: &CheckpointPayload) -> Result<(), ProtocolError> {
    if !p.main_ctx.is_object() {
        return Err(ProtocolError::PayloadInvalid(
            "checkpoint main_ctx must be a JSON object".to_string(),
        ));
    }
    if !p.base_ctx.is_object() {
        return Err(ProtocolError::PayloadInvalid(
            "checkpoint base_ctx must be a JSON object".to_string(),
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
// Streaming sender / receiver (what R02 drives over the protocol pipes)
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

    /// Next sequence number that will be assigned.
    pub fn next_seq(&self) -> u32 {
        self.next_seq
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

    /// Next sequence number expected from the peer.
    pub fn next_seq(&self) -> u32 {
        self.next_seq
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
