//! LLM utilities for recipe scripts
//!
//! Provides helpers for using a local LLM to extract structured information
//! from web pages or text when parsing is non-trivial.
//!
//! ## Use Cases
//!
//! - Finding the "latest version" when the page structure is complex
//! - Extracting download URLs from pages that don't have a predictable format
//! - Parsing changelogs or release notes
//!
//! ## Configuration
//!
//! Set `RECIPE_LLM_ENDPOINT` to point to your local LLM server:
//! ```bash
//! export RECIPE_LLM_ENDPOINT="http://localhost:11434/api/generate"  # Ollama
//! ```
//!
//! ## Philosophy
//!
//! The LLM is a TOOL, not the identity. Recipes should:
//! 1. Try deterministic parsing first (regex, JSON, etc.)
//! 2. Fall back to LLM only when structure is unpredictable
//! 3. Always validate LLM output before using it

use rhai::EvalAltResult;

/// Get LLM endpoint from environment, if configured.
fn _get_llm_endpoint() -> Option<String> {
    std::env::var("RECIPE_LLM_ENDPOINT").ok()
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
    // TODO: Implement when LLM endpoint design is finalized
    //
    // Design considerations:
    // - Should support multiple backends (Ollama, llama.cpp, etc.)
    // - Needs timeout handling for slow inference
    // - Should cache responses for identical content+prompt
    // - May need structured output format (JSON mode)

    Err("llm_extract not yet implemented - set RECIPE_LLM_ENDPOINT".into())
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
pub fn llm_find_latest_version(_url: &str, _project_name: &str) -> Result<String, Box<EvalAltResult>> {
    // TODO: Implement
    // 1. Fetch the URL
    // 2. Strip HTML to text (reduce token usage)
    // 3. Ask LLM: "What is the latest stable version of {project_name}? Reply with just the version number."
    // 4. Validate response looks like a version (semver-ish)

    Err("llm_find_latest_version not yet implemented".into())
}

/// Ask the LLM to extract a download URL matching criteria.
///
/// # Arguments
/// * `content` - Page content (HTML or text)
/// * `criteria` - What to look for (e.g., "x86_64 Linux tarball", "DVD ISO")
///
/// # Returns
/// The extracted URL
pub fn llm_find_download_url(_content: &str, _criteria: &str) -> Result<String, Box<EvalAltResult>> {
    // TODO: Implement
    // 1. Ask LLM to find URL matching criteria
    // 2. Validate response is a valid URL
    // 3. Optionally verify URL is reachable (HEAD request)

    Err("llm_find_download_url not yet implemented".into())
}
