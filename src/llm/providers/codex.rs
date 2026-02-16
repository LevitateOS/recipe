use crate::llm::provider::{LlmProviderId, ProviderResult, ResolvedCall, ResolvedLlmConfig};
use std::collections::BTreeMap;
use std::path::Path;

fn codex_config_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex").join("config.toml"))
}

fn discover_mcp_server_names() -> Vec<String> {
    let Some(path) = codex_config_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = text.parse::<toml::Value>() else {
        return Vec::new();
    };
    let Some(root) = value.as_table() else {
        return Vec::new();
    };
    let Some(mcp) = root.get("mcp_servers").and_then(|v| v.as_table()) else {
        return Vec::new();
    };
    mcp.keys().cloned().collect()
}

pub(crate) struct CodexInvocation {
    pub(crate) cmd: tokio::process::Command,
    pub(crate) call: ResolvedCall,
    pub(crate) last_message_file: tempfile::NamedTempFile,
}

pub(crate) fn build(
    cfg: &ResolvedLlmConfig,
    cwd: &Path,
    stdin_prompt: Vec<u8>,
) -> Result<CodexInvocation, String> {
    let mut cmd = tokio::process::Command::new(&cfg.codex.bin);

    cmd.arg("exec");
    // Recipe LLM helpers should be pure text transforms of provided input. Disable the shell tool
    // so Codex doesn't run its own command explorations while "extracting" from provided content.
    cmd.args(["--disable", "shell_tool"]);
    for arg in &cfg.codex.args {
        cmd.arg(arg);
    }

    if let Some(model) = &cfg.codex.model {
        cmd.args(["--model", model]);
    }

    // Arbitrary config overrides from llm.toml -> repeated --config key=value
    for ov in &cfg.codex.config_overrides {
        cmd.args(["--config", ov]);
    }

    // Effort maps to Codex's config override used in Ralph.
    if let Some(effort) = &cfg.codex.effort {
        cmd.arg("--config");
        cmd.arg(format!("model_reasoning_effort={}", effort));
    }

    // Hard-disable MCP servers for recipe invocations.
    //
    // There is no `codex exec --no-mcp` flag (as of `codex --help` on this machine).
    // Codex loads enabled MCP servers from ~/.codex/config.toml, so we override each discovered
    // server's `.enabled=false` at invocation time.
    //
    // Note: server names may contain '-', but this is Codex's dotted-path override syntax
    // (not TOML dotted keys). Quoting here causes Codex to treat it as a literal key name and
    // create a new table, so we do not quote.
    for name in discover_mcp_server_names() {
        cmd.arg("--config");
        cmd.arg(format!("mcp_servers.{name}.enabled=false"));
    }

    let last_message_file = tempfile::NamedTempFile::new()
        .map_err(|e| format!("Failed to create temp file for codex output: {e}"))?;

    cmd.arg("--output-last-message");
    cmd.arg(last_message_file.path());

    // Read prompt from stdin
    cmd.arg("-");

    let mut env = BTreeMap::new();
    env.extend(cfg.codex.env.clone());

    Ok(CodexInvocation {
        cmd,
        call: ResolvedCall {
            provider: LlmProviderId::Codex,
            prompt: "<stdin>".to_owned(),
            stdin: stdin_prompt,
            timeout_secs: cfg.timeout_secs,
            max_output_bytes: cfg.max_output_bytes,
            max_input_bytes: cfg.max_input_bytes,
            stream_stdout: true,
            stream_stderr: true,
            cwd: cwd.to_path_buf(),
            env,
        },
        last_message_file,
    })
}

pub(crate) fn finalize(result: ProviderResult, last_message_path: &Path) -> Result<String, String> {
    if result.exit_code != 0 {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(format!(
            "codex exited with code {}: {}",
            result.exit_code,
            stderr.trim()
        ));
    }

    let text = std::fs::read_to_string(last_message_path).map_err(|e| {
        format!(
            "codex did not write --output-last-message file ({}): {e}",
            last_message_path.display()
        )
    })?;

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("codex last message file was empty".to_owned());
    }

    // Return as-is (no semantic parsing), just trim trailing whitespace.
    Ok(trimmed.to_owned())
}
