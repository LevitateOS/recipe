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
const MAX_ID: usize = 64;

/// Toolchain variables R05 records as allowlisted (§5 "plus toolchain
/// variables recorded by I01/R05"). Extra values must be slash-free
/// literals (`gcc`) or absolute *domain* paths (`/work/...`); host paths
/// are rejected because the domain cannot resolve them.
const TOOLCHAIN_ENV_ALLOW: &[&str] = &[
    "CARGO_HOME",
    "RUSTUP_HOME",
    "CC",
    "CXX",
    "MAKEFLAGS",
];

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

/// Enforce the §5 invocation-id shape explicitly (never truncate silently).
pub(crate) fn validate_invocation_id(id: &str) -> Result<()> {
    if id.is_empty() || id.len() > MAX_ID {
        bail!(
            "input preparation failure: invocation id {id:?} has {} B, want 1..={MAX_ID} B",
            id.len(),
        );
    }
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!(
            "input preparation failure: invocation id {id:?} is outside `[A-Za-z0-9_-]` \
             (ids travel in env, result records, and host staging paths)"
        );
    }
    Ok(())
}

/// Build the allowlisted domain environment for `invocation_id` (§5).
///
/// The seven fixed entries use only domain paths and literals; `extra_env`
/// supplies caller-declared toolchain variables filtered against
/// [`TOOLCHAIN_ENV_ALLOW`]. Total entries ≤ 8 and each `NAME=value` ≤ 256 B
/// (§2); anything else fails explicitly.
pub fn allowlisted_env(
    invocation_id: &str,
    extra_env: &[(String, String)],
) -> Result<Vec<(String, String)>> {
    validate_invocation_id(invocation_id)?;
    let mut env = vec![
        ("PATH".to_string(), "/work/tools:/usr/bin:/bin".to_string()),
        ("RECIPE_WORKER".to_string(), "1".to_string()),
        ("RECIPE_INVOCATION".to_string(), invocation_id.to_string()),
        ("BUILD_DIR".to_string(), "/work/build".to_string()),
        ("TMPDIR".to_string(), "/work/tmp".to_string()),
        ("ARCH".to_string(), std::env::consts::ARCH.to_string()),
        ("NPROC".to_string(), num_cpus::get().to_string()),
    ];
    for (name, value) in extra_env {
        if !TOOLCHAIN_ENV_ALLOW.contains(&name.as_str()) {
            bail!(
                "input preparation failure: env {name:?} is not allowlisted \
                 (want one of {TOOLCHAIN_ENV_ALLOW:?}; §5 keeps HOME, host paths, \
                 and secrets out of the worker environment)"
            );
        }
        validate_domain_value(name, value)?;
        if env.iter().any(|(existing, _)| existing == name) {
            bail!("input preparation failure: duplicate env entry {name:?}");
        }
        env.push((name.clone(), value.clone()));
    }
    if env.len() > 8 {
        bail!(
            "input preparation failure: {} env entries exceed the §2 limit of 8 × 256 B \
             (keep the worker environment to the §5 allowlist)",
            env.len(),
        );
    }
    for (name, value) in &env {
        let entry = format!("{name}={value}");
        if entry.len() > 256 {
            bail!(
                "input preparation failure: env entry {name:?} is {} B, exceeding the §2 256 B bound \
                 (shorten the value; entries are never truncated)",
                entry.len(),
            );
        }
    }
    Ok(env)
}

/// Extra env values are domain literals, never host paths.
fn validate_domain_value(name: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("input preparation failure: env {name:?} has an empty value");
    }
    if value.bytes().any(|byte| byte == 0 || byte == b'\n') {
        bail!("input preparation failure: env {name:?} contains NUL or newline");
    }
    if value.contains("..") {
        bail!(
            "input preparation failure: env {name:?} value {value:?} escapes with `..` \
             (extra env values must be plain literals or absolute domain paths)"
        );
    }
    if value.starts_with('/') {
        let rooted = ["/work/", "/inputs/", "/out/", "/usr/", "/bin/"]
            .iter()
            .any(|prefix| value == prefix.trim_end_matches('/') || value.starts_with(prefix));
        if !rooted {
            bail!(
                "input preparation failure: env {name:?} value {value:?} is an absolute path \
                 outside /work, /inputs, /out, /usr, /bin \
                 (host checkout paths never enter the worker environment)"
            );
        }
    }
    Ok(())
}
