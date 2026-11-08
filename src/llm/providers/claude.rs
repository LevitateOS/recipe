use crate::llm::provider::{ProviderResult, ResolvedCall, ResolvedLlmConfig};
use std::path::Path;

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

pub(crate) struct ClaudeInvocation {
    pub(crate) cmd: std::process::Command,
    pub(crate) call: ResolvedCall,
}

pub(crate) fn build(
    cfg: &ResolvedLlmConfig,
    cwd: &Path,
    stdin_prompt: Vec<u8>,
) -> Result<ClaudeInvocation, String> {
    let mut cmd = std::process::Command::new(&cfg.claude.bin);

    for arg in &cfg.claude.args {
        cmd.arg(arg);
    }

    // Ensure non-interactive mode. Claude supports -p/--print.
    if !has_flag(&cfg.claude.args, "-p") && !has_flag(&cfg.claude.args, "--print") {
        cmd.arg("-p");
    }

    // Ensure stdin is interpreted as text prompt when used with --print.
    if !has_flag(&cfg.claude.args, "--input-format") {
        cmd.args(["--input-format", "text"]);
    }

    // Default output format to plain text unless user configured otherwise.
    if !has_flag(&cfg.claude.args, "--output-format") {
        cmd.args(["--output-format", "text"]);
    }

    if let Some(model) = &cfg.claude.model {
        cmd.args(["--model", model]);
    }
    if let Some(effort) = &cfg.claude.effort {
        cmd.args(["--effort", effort]);
    }

    let env = cfg.claude.env.clone();

    Ok(ClaudeInvocation {
        cmd,
        call: ResolvedCall {
            stdin: stdin_prompt,
            timeout_secs: cfg.timeout_secs,
            max_output_bytes: cfg.max_output_bytes,
            max_input_bytes: cfg.max_input_bytes,
            stream_stdout: false,
            stream_stderr: false,
            cwd: cwd.to_path_buf(),
            env,
        },
    })
}

pub(crate) fn finalize(result: ProviderResult) -> Result<String, String> {
    if !result.exit.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(format!(
            "claude exited with {}: {}",
            result.exit,
            stderr.trim()
        ));
    }
    let out = String::from_utf8_lossy(&result.stdout);
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return Err("claude returned empty output".to_owned());
    }
    Ok(trimmed.to_owned())
}
