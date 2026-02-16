use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LlmProviderId {
    Codex,
    Claude,
}

impl LlmProviderId {
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            other => Err(format!(
                "Unknown LLM provider '{other}'. Expected 'codex' or 'claude'."
            )),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderConfig {
    pub(crate) bin: String,
    pub(crate) args: Vec<String>,
    pub(crate) model: Option<String>,
    pub(crate) effort: Option<String>,
    pub(crate) config_overrides: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedLlmConfig {
    pub(crate) default_provider: LlmProviderId,
    pub(crate) timeout_secs: u64,
    pub(crate) max_output_bytes: usize,
    pub(crate) max_input_bytes: usize,

    pub(crate) codex: ProviderConfig,
    pub(crate) claude: ProviderConfig,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedCall {
    #[allow(dead_code)]
    pub(crate) provider: LlmProviderId,
    #[allow(dead_code)]
    pub(crate) prompt: String,
    pub(crate) stdin: Vec<u8>,
    pub(crate) timeout_secs: u64,
    pub(crate) max_output_bytes: usize,
    pub(crate) max_input_bytes: usize,
    pub(crate) stream_stdout: bool,
    pub(crate) stream_stderr: bool,
    pub(crate) cwd: PathBuf,
    pub(crate) env: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProviderResult {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) exit_code: i32,
}
