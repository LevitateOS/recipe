//! LLM providers behind one call: [`complete`] resolves the configured
//! provider, runs it on the prompt, and returns its text.

mod config;
mod provider;
mod runner;

mod providers {
    pub(crate) mod claude;
    pub(crate) mod codex;
}

use provider::LlmProviderId;

/// Run `prompt` through the configured (or `profile`-selected) provider in
/// `cwd` and return the reply text.
pub(crate) fn complete(prompt: String, cwd: &std::path::Path, profile: Option<&str>) -> Result<String, String> {
    let cfg = config::resolve_config_for_call(profile)?;
    let stdin = prompt.into_bytes();
    match cfg.default_provider {
        LlmProviderId::Codex => {
            let invocation = providers::codex::build(&cfg, cwd, stdin)?;
            // The provider writes its final message into this file; it must
            // outlive the run and is read by finalize.
            let result = runner::run_call(invocation.cmd, invocation.call)?;
            providers::codex::finalize(result, invocation.last_message_file.path())
        }
        LlmProviderId::Claude => {
            let invocation = providers::claude::build(&cfg, cwd, stdin)?;
            let result = runner::run_call(invocation.cmd, invocation.call)?;
            providers::claude::finalize(result)
        }
    }
}
