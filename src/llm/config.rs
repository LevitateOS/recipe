use crate::llm::provider::{LlmProviderId, ProviderConfig, ResolvedLlmConfig};
use serde::Deserialize;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_MAX_INPUT_BYTES: usize = 10 * 1024 * 1024;

thread_local! {
    static SELECTED_PROFILE: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub(crate) fn with_llm_profile<T>(profile: Option<&str>, f: impl FnOnce() -> T) -> T {
    let prev = SELECTED_PROFILE.with(|p| p.borrow().clone());
    SELECTED_PROFILE.with(|p| *p.borrow_mut() = profile.map(|s| s.to_owned()));

    // Ensure we always restore the previous profile even if `f()` panics.
    struct RestoreGuard {
        prev: Option<String>,
    }
    impl Drop for RestoreGuard {
        fn drop(&mut self) {
            let prev = std::mem::take(&mut self.prev);
            SELECTED_PROFILE.with(|p| *p.borrow_mut() = prev);
        }
    }

    let _guard = RestoreGuard { prev };
    f()
}

fn selected_profile() -> Option<String> {
    SELECTED_PROFILE.with(|p| p.borrow().clone())
}

#[derive(Debug, Clone, Deserialize, Default)]
struct LlmToml {
    version: Option<u32>,
    default_provider: Option<String>,
    default_profile: Option<String>,
    timeout_secs: Option<u64>,
    max_output_bytes: Option<usize>,
    max_input_bytes: Option<usize>,
    providers: Option<ProvidersToml>,
    profiles: Option<BTreeMap<String, ProfileToml>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ProfileToml {
    default_provider: Option<String>,
    timeout_secs: Option<u64>,
    max_output_bytes: Option<usize>,
    max_input_bytes: Option<usize>,
    providers: Option<ProvidersToml>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ProvidersToml {
    codex: Option<ProviderToml>,
    claude: Option<ProviderToml>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ProviderToml {
    bin: Option<String>,
    args: Option<Vec<String>>,
    model: Option<String>,
    effort: Option<String>,
    #[serde(rename = "config")]
    config_overrides: Option<Vec<String>>,
    env: Option<BTreeMap<String, String>>,
}

impl LlmToml {
    fn merge(&mut self, other: LlmToml) {
        if other.version.is_some() {
            self.version = other.version;
        }
        if other.default_provider.is_some() {
            self.default_provider = other.default_provider;
        }
        if other.default_profile.is_some() {
            self.default_profile = other.default_profile;
        }
        if other.timeout_secs.is_some() {
            self.timeout_secs = other.timeout_secs;
        }
        if other.max_output_bytes.is_some() {
            self.max_output_bytes = other.max_output_bytes;
        }
        if other.max_input_bytes.is_some() {
            self.max_input_bytes = other.max_input_bytes;
        }
        match (self.providers.as_mut(), other.providers) {
            (Some(dst), Some(src)) => dst.merge(src),
            (None, Some(src)) => self.providers = Some(src),
            _ => {}
        }
        match (self.profiles.as_mut(), other.profiles) {
            (Some(dst), Some(src)) => {
                for (name, prof) in src {
                    match dst.get_mut(&name) {
                        Some(existing) => existing.merge(prof),
                        None => {
                            dst.insert(name, prof);
                        }
                    }
                }
            }
            (None, Some(src)) => self.profiles = Some(src),
            _ => {}
        }
    }

    fn apply_profile(&mut self, p: &ProfileToml) {
        if p.default_provider.is_some() {
            self.default_provider = p.default_provider.clone();
        }
        if p.timeout_secs.is_some() {
            self.timeout_secs = p.timeout_secs;
        }
        if p.max_output_bytes.is_some() {
            self.max_output_bytes = p.max_output_bytes;
        }
        if p.max_input_bytes.is_some() {
            self.max_input_bytes = p.max_input_bytes;
        }
        match (self.providers.as_mut(), p.providers.clone()) {
            (Some(dst), Some(src)) => dst.merge(src),
            (None, Some(src)) => self.providers = Some(src),
            _ => {}
        }
    }
}

impl ProfileToml {
    fn merge(&mut self, other: ProfileToml) {
        if other.default_provider.is_some() {
            self.default_provider = other.default_provider;
        }
        if other.timeout_secs.is_some() {
            self.timeout_secs = other.timeout_secs;
        }
        if other.max_output_bytes.is_some() {
            self.max_output_bytes = other.max_output_bytes;
        }
        if other.max_input_bytes.is_some() {
            self.max_input_bytes = other.max_input_bytes;
        }
        match (self.providers.as_mut(), other.providers) {
            (Some(dst), Some(src)) => dst.merge(src),
            (None, Some(src)) => self.providers = Some(src),
            _ => {}
        }
    }
}

impl ProvidersToml {
    fn merge(&mut self, other: ProvidersToml) {
        match (&mut self.codex, other.codex) {
            (Some(dst), Some(src)) => dst.merge(src),
            (None, Some(src)) => self.codex = Some(src),
            _ => {}
        }
        match (&mut self.claude, other.claude) {
            (Some(dst), Some(src)) => dst.merge(src),
            (None, Some(src)) => self.claude = Some(src),
            _ => {}
        }
    }
}

impl ProviderToml {
    fn merge(&mut self, other: ProviderToml) {
        if other.bin.is_some() {
            self.bin = other.bin;
        }
        if other.args.is_some() {
            self.args = other.args;
        }
        if other.model.is_some() {
            self.model = other.model;
        }
        if other.effort.is_some() {
            self.effort = other.effort;
        }
        if other.config_overrides.is_some() {
            self.config_overrides = other.config_overrides;
        }
        match (&mut self.env, other.env) {
            (Some(dst), Some(src)) => {
                for (k, v) in src {
                    dst.insert(k, v);
                }
            }
            (None, Some(src)) => self.env = Some(src),
            _ => {}
        }
    }
}

fn split_xdg_config_dirs() -> Vec<PathBuf> {
    let raw = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".to_owned());
    raw.split(':')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn xdg_config_home() -> PathBuf {
    if let Ok(raw) = std::env::var("XDG_CONFIG_HOME") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".").join(".config"))
}

fn read_toml(path: &Path) -> Result<LlmToml, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    toml::from_str::<LlmToml>(&text).map_err(|e| format!("Invalid TOML in {}: {e}", path.display()))
}

fn find_config_files() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for dir in split_xdg_config_dirs() {
        paths.push(dir.join("recipe").join("llm.toml"));
    }
    paths.push(xdg_config_home().join("recipe").join("llm.toml"));

    paths
}

fn provider_required<'a>(
    name: &str,
    p: Option<&'a ProviderToml>,
) -> Result<&'a ProviderToml, String> {
    p.ok_or_else(|| format!("Missing [providers.{name}] block in recipe/llm.toml"))
}

fn resolve_provider_cfg(default_bin: &str, p: &ProviderToml) -> ProviderConfig {
    ProviderConfig {
        bin: p.bin.clone().unwrap_or_else(|| default_bin.to_owned()),
        args: p.args.clone().unwrap_or_default(),
        model: p.model.clone(),
        effort: p.effort.clone(),
        config_overrides: p.config_overrides.clone().unwrap_or_default(),
        env: p.env.clone().unwrap_or_default(),
    }
}

fn build_resolved(cfg: &LlmToml) -> Result<ResolvedLlmConfig, String> {
    let default_provider = cfg
        .default_provider
        .as_deref()
        .ok_or_else(|| {
            "Missing required key `default_provider` in recipe/llm.toml (expected 'codex' or 'claude')"
                .to_owned()
        })
        .and_then(LlmProviderId::parse)?;

    let providers = cfg
        .providers
        .as_ref()
        .ok_or_else(|| "Missing required table [providers] in recipe/llm.toml".to_owned())?;

    let codex = provider_required("codex", providers.codex.as_ref())?;
    let claude = provider_required("claude", providers.claude.as_ref())?;

    Ok(ResolvedLlmConfig {
        default_provider,
        timeout_secs: cfg.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS).max(1),
        max_output_bytes: cfg
            .max_output_bytes
            .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES)
            .max(1024),
        max_input_bytes: cfg
            .max_input_bytes
            .unwrap_or(DEFAULT_MAX_INPUT_BYTES)
            .max(1024),
        codex: resolve_provider_cfg("codex", codex),
        claude: resolve_provider_cfg("claude", claude),
    })
}

fn load_config_impl() -> Result<LlmToml, String> {
    let candidates = find_config_files();
    let mut merged = LlmToml::default();
    let mut found_any = false;

    for path in &candidates {
        if !path.exists() {
            continue;
        }
        let parsed = read_toml(path)?;
        merged.merge(parsed);
        found_any = true;
    }

    if !found_any {
        let mut msg = String::from("Recipe LLM config not found. Create one of:\n");
        for p in candidates {
            msg.push_str(&format!("  - {}\n", p.display()));
        }
        return Err(msg.trim_end().to_owned());
    }

    Ok(merged)
}

pub(crate) fn resolve_config_for_call() -> Result<ResolvedLlmConfig, String> {
    let merged = load_config_impl()?;

    let selected = selected_profile().or_else(|| merged.default_profile.clone());

    let mut effective = merged.clone();
    if let Some(name) = selected {
        let profiles = merged.profiles.as_ref().ok_or_else(|| {
            format!("Unknown LLM profile '{name}' (no [profiles] table in recipe/llm.toml)")
        })?;
        let p = profiles.get(&name).cloned().ok_or_else(|| {
            let mut keys: Vec<&str> = profiles.keys().map(|s| s.as_str()).collect();
            keys.sort_unstable();
            if keys.is_empty() {
                format!("Unknown LLM profile '{name}' (no profiles defined)")
            } else {
                format!(
                    "Unknown LLM profile '{name}'. Available profiles: {}",
                    keys.join(", ")
                )
            }
        })?;
        effective.apply_profile(&p);
    }

    build_resolved(&effective)
}
