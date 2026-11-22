//! The Ginux launcher's wire format, as the coordinator reads it: the result
//! record (`docs/ginux-recipe-interfaces.md` §2).

use crate::failure::Category;
use anyhow::Result;
use std::path::Path;

/// How the supervised workload ended, per the result record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Exited,
    Signaled,
    SupervisorError,
}

impl Outcome {
    const ALL: [Outcome; 3] = [Self::Exited, Self::Signaled, Self::SupervisorError];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::Signaled => "signaled",
            Self::SupervisorError => "supervisor-error",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|outcome| outcome.as_str() == text)
    }
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The supervisor's authoritative result record.
pub(crate) struct ResultRecord {
    pub outcome: Outcome,
    /// Workload exit code when `exited`.
    pub code: Option<i64>,
    pub signal: Option<u64>,
}

impl ResultRecord {
    /// Read and validate the record. Missing or malformed is a `protocol`
    /// failure, never success (§2).
    pub(crate) fn read(path: &Path, invocation: &str) -> Result<Self> {
        let protocol = |message: String| anyhow::Error::new(Category::Protocol.error(message));
        let text = std::fs::read_to_string(path).map_err(|err| {
            protocol(format!(
                "invocation {invocation} produced no result file at {}: {err:#} \
                 (missing result file is never success)",
                path.display(),
            ))
        })?;
        let value: serde_json::Value = serde_json::from_str(&text).map_err(|err| {
            protocol(format!(
                "invocation {invocation} result file is not valid JSON: {err:#} \
                 (malformed result file is never success)"
            ))
        })?;
        let version = value.get("version").and_then(|v| v.as_u64());
        if version != Some(1) {
            return Err(protocol(format!(
                "invocation {invocation} result file has unsupported version {version:?} (want 1)"
            )));
        }
        let text = value
            .get("outcome")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let outcome = Outcome::parse(text).ok_or_else(|| {
            protocol(format!(
                "invocation {invocation} result file has unknown outcome {text:?} \
                 (want exited|signaled|supervisor-error)"
            ))
        })?;
        let field = |name: &str| match value.get(name) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::Number(n)) => n.as_i64().map(Some).ok_or_else(|| {
                protocol(format!(
                    "invocation {invocation} result file field {name:?} is not an integer"
                ))
            }),
            Some(other) => Err(protocol(format!(
                "invocation {invocation} result file field {name:?} is {}, want an integer or null",
                json_value_kind(other),
            ))),
        };
        // `generation` belongs to Ginux's numbering; it is validated, not consumed.
        field("generation")?;
        let signal = field("signal")?
            .map(u64::try_from)
            .transpose()
            .map_err(|_| {
                protocol(format!(
                    "invocation {invocation} result file field \"signal\" is negative"
                ))
            })?;
        Ok(Self {
            outcome,
            code: field("code")?,
            signal,
        })
    }
}

/// The record's outcome, if a valid record exists (for error diagnostics).
pub(crate) fn peek_outcome(path: &Path, invocation: &str) -> Option<Outcome> {
    ResultRecord::read(path, invocation)
        .ok()
        .map(|record| record.outcome)
}

/// A short excerpt of the record for diagnostics.
pub(crate) fn result_note(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let short: String = text.chars().take(300).collect();
    Some(format!("result file present: {short}"))
}

fn json_value_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}
