//! Invocation identity and the allowlisted worker environment (§5).
//!
//! The worker environment is explicit and closed: fixed domain paths, the
//! invocation id, and architecture/toolchain facts. No `HOME`, no absolute
//! host checkout paths, no secret-bearing variables cross into the domain.
//! Anything outside the allowlist is an explicit failure, never silently
//! dropped.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};

/// Longest recipe-stem prefix kept in the invocation id.
const MAX_ID: usize = crate::worker::protocol::MAX_INVOCATION_ID;

/// `<recipe-name>-<unix-millis>-<pid>`, `[A-Za-z0-9_-]{1,64}` (§5).
pub fn invocation_id(recipe_stem: &str) -> Result<String> {
    let mut stem: String = recipe_stem
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
                byte as char
            } else {
                '-'
            }
        })
        .collect();
    if stem.is_empty() {
        stem.push_str("recipe");
    }
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let pid = std::process::id();
    let suffix = format!("-{millis}-{pid}");
    let keep = MAX_ID.saturating_sub(suffix.len()).max(1);
    if stem.len() > keep {
        stem.truncate(keep);
    }
    while stem.ends_with('-') {
        stem.pop();
    }
    if stem.is_empty() {
        stem.push_str("recipe");
    }
    let id = format!("{stem}{suffix}");
    validate_invocation_id(&id)?;
    Ok(id)
}

/// Enforce the invocation-id rule ([`crate::worker::protocol::valid_invocation_id`])
/// with a useful message (never truncate silently).
pub fn validate_invocation_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > MAX_ID {
        bail!(
            "input preparation failure: invocation id {id:?} has {} B, want 1..={MAX_ID} B",
            id.len(),
        );
    }
    if !crate::worker::protocol::valid_invocation_id(id) {
        bail!(
            "input preparation failure: invocation id {id:?} is outside `[A-Za-z0-9_-]` \
             (ids travel in env, result records, and host staging paths)"
        );
    }
    Ok(())
}

/// Build the fixed domain environment for `invocation_id` (§5): domain paths
/// and literals only.
pub fn allowlisted_env(invocation_id: &str) -> Result<Vec<(String, String)>> {
    validate_invocation_id(invocation_id)?;
    use crate::domain;
    let mut path: Vec<String> = domain::tool_bin_dirs().collect();
    path.extend([domain::TOOLS.into(), "/usr/bin".into(), "/bin".into()]);
    Ok(vec![
        ("PATH".to_string(), path.join(":")),
        (domain::ENV_WORKER.to_string(), "1".to_string()),
        (domain::ENV_INVOCATION.to_string(), invocation_id.to_string()),
        ("TMPDIR".to_string(), domain::TMP.to_string()),
    ])
}
