//! LLM utilities for recipe scripts
//!
//! These helpers shell out to an external agent CLI (Codex or Claude) to do
//! non-deterministic extraction when parsing is genuinely hard.
//!
//! Recipe does not try to interpret the model output. It only runs the chosen
//! CLI non-interactively, enforces timeouts/size limits, and returns the final
//! text.
//!
//! ## Configuration (XDG)
//! Create `recipe/llm.toml` under your XDG config dirs:
//! - User: `$XDG_CONFIG_HOME/recipe/llm.toml` (default `~/.config/recipe/llm.toml`)
//! - System: `$XDG_CONFIG_DIRS/recipe/llm.toml` (default `/etc/xdg/recipe/llm.toml`)
//!
//! A `default_provider` is required (equal footing; no implicit fallback).
//!
//! You can define named LLM profiles under `[profiles.<name>]` in `llm.toml`, and select a profile
//! per `recipe` invocation using `recipe --llm-profile <name> ...`.

use rhai::EvalAltResult;

fn current_working_dir() -> Result<std::path::PathBuf, Box<EvalAltResult>> {
    std::env::current_dir().map_err(|e| format!("Failed to resolve cwd: {e}").into())
}

fn to_eval_err(e: String) -> Box<EvalAltResult> {
    e.into()
}

fn extract_recipe_file_hint(content: &str) -> Option<String> {
    // Try to extract `recipe_file: "/path/to/file.rhai"` from ctx for better prompts.
    let key = "recipe_file:";
    let idx = content.find(key)?;
    let rest = &content[idx + key.len()..];
    let quote_start = rest.find('"')?;
    let rest2 = &rest[quote_start + 1..];
    let quote_end = rest2.find('"')?;
    let path = &rest2[..quote_end];
    let trimmed = path.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_owned())
}

fn strip_outer_code_fence(s: String) -> String {
    let trimmed = s.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_owned();
    }

    // Handle:
    // ```rhai
    // ...
    // ```
    let after_open = match trimmed.find('\n') {
        Some(i) => &trimmed[i + 1..],
        None => return trimmed.to_owned(),
    };
    let after_open = after_open.trim_end();
    if !after_open.ends_with("```") {
        return trimmed.to_owned();
    }
    let without_close = after_open[..after_open.len() - 3].trim_end_matches('\n');
    without_close.trim().to_owned()
}

fn build_extract_prompt(content: &str, task: &str) -> String {
    let mut header = String::new();
    header.push_str("You are a precise extraction and editing tool for Rhai recipe files.\n");
    header.push_str("The complete relevant content is provided below.\n");
    header
        .push_str("Do NOT run commands, do NOT browse, and do NOT try to locate files on disk.\n");
    header.push_str("Return ONLY the requested output. No prose. No markdown. No code fences.\n");

    if let Some(path) = extract_recipe_file_hint(content) {
        header.push_str(&format!("\nTARGET FILE PATH:\n{path}\n"));
    }

    format!(
        "{header}\nTASK:\n{task}\n\nCONTENT (complete):\n{content}\n",
        header = header,
        task = task,
        content = content
    )
}

fn run_provider(prompt: String) -> Result<String, Box<EvalAltResult>> {
    let cfg = crate::llm::resolve_config_for_call().map_err(to_eval_err)?;
    let cwd = current_working_dir()?;

    let stdin = prompt.into_bytes();

    let out = match cfg.default_provider {
        crate::llm::LlmProviderId::Codex => {
            let inv =
                crate::llm::providers::codex::build(&cfg, &cwd, stdin).map_err(to_eval_err)?;
            let res = crate::llm::run_call(inv.cmd, inv.call).map_err(to_eval_err)?;
            crate::llm::providers::codex::finalize(res, inv.last_message_file.path())
                .map_err(to_eval_err)
        }
        crate::llm::LlmProviderId::Claude => {
            let inv =
                crate::llm::providers::claude::build(&cfg, &cwd, stdin).map_err(to_eval_err)?;
            let res = crate::llm::run_call(inv.cmd, inv.call).map_err(to_eval_err)?;
            crate::llm::providers::claude::finalize(res).map_err(to_eval_err)
        }
    }?;

    Ok(strip_outer_code_fence(out))
}

/// Ask the LLM to extract structured information from text.
///
/// # Arguments
/// * `content` - The text content to analyze (e.g., HTML, changelog)
/// * `prompt` - What to extract (e.g., "What is the latest version number?")
///
/// # Returns
/// The LLM's response as a string.
///
/// # Example (Rhai)
/// ```rhai
/// let html = http_get("https://example.com/downloads");
/// let version = llm_extract(html, "What is the latest stable version number? Reply with just the version.");
/// ```
pub fn llm_extract(_content: &str, _prompt: &str) -> Result<String, Box<EvalAltResult>> {
    run_provider(build_extract_prompt(_content, _prompt))
}

/// Ask the LLM to find the latest version from a downloads page.
///
/// Specialized wrapper around llm_extract for the common case of
/// finding version numbers on project download pages.
///
/// # Arguments
/// * `url` - URL of the downloads/releases page
/// * `project_name` - Name of the project (for context)
///
/// # Returns
/// The version string (e.g., "10.2", "1.0.0-beta.3")
pub fn llm_find_latest_version(
    _url: &str,
    _project_name: &str,
) -> Result<String, Box<EvalAltResult>> {
    let content = crate::helpers::acquire::http::http_get(_url)?;
    let task = format!(
        "Project: {project}\nReturn ONLY the latest stable version string. No prose.",
        project = _project_name
    );
    let prompt = build_extract_prompt(
        &content,
        &format!("(source url: {url})\n{task}", url = _url, task = task),
    );
    run_provider(prompt)
}

/// Ask the LLM to extract a download URL matching criteria.
///
/// # Arguments
/// * `content` - Page content (HTML or text)
/// * `criteria` - What to look for (e.g., "x86_64 Linux tarball", "DVD ISO")
///
/// # Returns
/// The extracted URL
pub fn llm_find_download_url(
    _content: &str,
    _criteria: &str,
) -> Result<String, Box<EvalAltResult>> {
    let task = format!(
        "Return ONLY the matching URL. No prose.\nCriteria: {c}",
        c = _criteria
    );
    run_provider(build_extract_prompt(_content, &task))
}
