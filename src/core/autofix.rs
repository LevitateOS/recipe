use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const AUTOFIX_PROMPT: &str = r#"
You are patching Rhai recipe files for the `recipe` package manager.

Goal: fix the failing `recipe install` by editing code/config files as needed.

Hard constraints:
- Output MUST be a single unified diff suitable for `git apply` (no prose, no markdown, no code fences).
- Do NOT change or weaken lifecycle checks:
  - Do NOT modify `fn is_installed`, `fn is_built`, or `fn is_acquired`.
- Do NOT "paper over" failures:
  - Do NOT add `|| true` or other patterns that ignore errors from critical commands.
- Assume you cannot run commands or inspect the filesystem: all relevant context is included below.
"#;

fn strip_outer_code_fence(s: String) -> String {
    let trimmed = s.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_owned();
    }
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

fn git_repo_root(from: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(from)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

fn detect_cwd(cfg: &crate::AutoFixConfig, recipe_path: &Path) -> Result<PathBuf> {
    if let Some(p) = &cfg.cwd {
        return Ok(p.clone());
    }

    let recipe_dir = recipe_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    if let Ok(here) = std::env::current_dir() {
        if let Some(root) = git_repo_root(&here) {
            return Ok(root);
        }
    }
    if let Some(root) = git_repo_root(&recipe_dir) {
        return Ok(root);
    }

    Ok(recipe_dir)
}

fn normalize_allow_paths(cwd: &Path, cfg: &crate::AutoFixConfig) -> Vec<PathBuf> {
    if cfg.allow_paths.is_empty() {
        return vec![cwd.to_path_buf()];
    }

    cfg.allow_paths
        .iter()
        .map(|p| {
            if p.is_absolute() {
                p.clone()
            } else {
                cwd.join(p)
            }
        })
        .collect()
}

fn parse_extends(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("//! extends:") {
            return Some(rest.trim().to_string());
        }
        if !trimmed.starts_with("//") {
            break;
        }
    }
    None
}

fn resolve_base_path(
    base_rel: &str,
    child_path: &Path,
    search_path: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(child_dir) = child_path.parent() {
        let candidate = child_dir.join(base_rel);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    if let Some(sp) = search_path {
        let candidate = sp.join(base_rel);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn read_file(p: &Path) -> Result<String> {
    std::fs::read_to_string(p).with_context(|| format!("Reading {}", p.display()))
}

fn extract_autofix_prompt_block(source: &str) -> Option<String> {
    let mut lines = source.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("// AUTOFIX_PROMPT:") {
            let mut out = String::new();
            out.push_str(rest.trim());
            out.push('\n');

            for l in lines {
                let t = l.trim_start();
                if t.is_empty() {
                    break;
                }
                let Some(body) = t.strip_prefix("//") else {
                    break;
                };
                out.push_str(body.trim_start());
                out.push('\n');
            }

            let trimmed = out.trim();
            return (!trimmed.is_empty()).then_some(trimmed.to_owned());
        }
    }
    None
}

fn read_extra_prompt(prompt_file: Option<&Path>) -> Result<Option<String>> {
    let Some(p) = prompt_file else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(p).with_context(|| format!("Reading {}", p.display()))?;
    let trimmed = text.trim();
    Ok((!trimmed.is_empty()).then_some(trimmed.to_owned()))
}

fn build_prompt(
    cwd: &Path,
    allow_paths: &[PathBuf],
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    reason: &str,
    failure: &str,
    child_source: &str,
    base_path: Option<&Path>,
    base_source: Option<&str>,
    extra_files: &[(PathBuf, String)],
    comment_prompt: Option<&str>,
    extra_prompt: Option<&str>,
) -> String {
    let mut p = String::new();
    p.push_str(AUTOFIX_PROMPT);

    p.push_str("\nWORKING DIRECTORY:\n");
    p.push_str(&cwd.display().to_string());
    p.push_str("\n\nALLOWED PATCH ROOTS:\n");
    for ap in allow_paths {
        p.push_str("- ");
        p.push_str(&ap.display().to_string());
        p.push('\n');
    }

    p.push_str("\nRECIPE PATH:\n");
    p.push_str(&recipe_path.display().to_string());
    p.push('\n');

    if let Some(sp) = search_path {
        p.push_str("\nRECIPES SEARCH PATH:\n");
        p.push_str(&sp.display().to_string());
        p.push('\n');
    }

    p.push_str("\nDEFINES (KEY=VALUE):\n");
    if defines.is_empty() {
        p.push_str("(none)\n");
    } else {
        for (k, v) in defines {
            p.push_str(k);
            p.push('=');
            p.push_str(v);
            p.push('\n');
        }
    }

    p.push_str("\nFAILURE REASON:\n");
    p.push_str(reason);
    p.push('\n');

    p.push_str("\nFAILURE OUTPUT:\n");
    p.push_str(failure.trim());
    p.push('\n');

    if let Some(cp) = comment_prompt {
        p.push_str("\nRECIPE-SUPPLIED AUTOFIX PROMPT:\n");
        p.push_str(cp.trim());
        p.push('\n');
    }
    if let Some(ep) = extra_prompt {
        p.push_str("\nEXTRA AUTOFIX PROMPT:\n");
        p.push_str(ep.trim());
        p.push('\n');
    }

    p.push_str("\nFILE: ");
    p.push_str(&recipe_path.display().to_string());
    p.push('\n');
    p.push_str(child_source);
    p.push('\n');

    if let (Some(bp), Some(bs)) = (base_path, base_source) {
        p.push_str("\nFILE: ");
        p.push_str(&bp.display().to_string());
        p.push('\n');
        p.push_str(bs);
        p.push('\n');
    }

    for (path, content) in extra_files {
        p.push_str("\nFILE: ");
        p.push_str(&path.display().to_string());
        p.push('\n');
        p.push_str(content);
        p.push('\n');
    }

    p.push_str("\nRETURN ONLY A UNIFIED DIFF. NO OTHER TEXT.\n");
    p
}

fn extract_patch_paths(patch: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // diff --git a/foo b/foo
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() == 2 {
                for p in parts {
                    if let Some(s) = p.strip_prefix("a/").or_else(|| p.strip_prefix("b/")) {
                        out.push(s.to_owned());
                    }
                }
            }
            continue;
        }
        let Some(rest) = line
            .strip_prefix("+++ b/")
            .or_else(|| line.strip_prefix("--- a/"))
        else {
            continue;
        };
        let path = rest.trim();
        if path == "/dev/null" {
            continue;
        }
        out.push(path.to_owned());
    }
    out.sort();
    out.dedup();
    out
}

fn has_parent_dir_components(path: &str) -> bool {
    Path::new(path)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

#[derive(Debug, Clone)]
struct ProtectedFnRange {
    name: &'static str,
    start_line: usize,
    end_line: usize,
}

fn line_fn_name(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if t.starts_with("//") || t.starts_with("/*") {
        return None;
    }
    let t = t.strip_prefix("fn")?;
    if t.is_empty() || !t.as_bytes()[0].is_ascii_whitespace() {
        return None;
    }
    let t = t.trim_start();
    let end = t
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(t.len());
    if end == 0 {
        return None;
    }
    Some(&t[..end])
}

fn find_fn_end_line(source: &str, start_offset: usize, start_line: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut i = start_offset;
    let mut line = start_line;

    let mut depth: i32 = 0;
    let mut started = false;

    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut in_string: Option<u8> = None;
    let mut escape = false;

    while i < bytes.len() {
        let b = bytes[i];

        if b == b'\n' {
            line = line.saturating_add(1);
            in_line_comment = false;
        }

        if in_line_comment {
            i += 1;
            continue;
        }
        if in_block_comment {
            if b == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                in_block_comment = false;
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if let Some(q) = in_string {
            if escape {
                escape = false;
                i += 1;
                continue;
            }
            if b == b'\\' {
                escape = true;
                i += 1;
                continue;
            }
            if b == q {
                in_string = None;
            }
            i += 1;
            continue;
        }

        if b == b'/' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'/' {
                in_line_comment = true;
                i += 2;
                continue;
            }
            if bytes[i + 1] == b'*' {
                in_block_comment = true;
                i += 2;
                continue;
            }
        }

        if b == b'"' || b == b'\'' {
            in_string = Some(b);
            i += 1;
            continue;
        }

        if b == b'{' {
            depth += 1;
            started = true;
        } else if b == b'}' && started {
            depth -= 1;
            if depth <= 0 {
                return Some(line);
            }
        }

        i += 1;
    }

    None
}

fn protected_fn_ranges(source: &str) -> Vec<ProtectedFnRange> {
    let protected = [
        ("is_installed", "is_installed"),
        ("is_built", "is_built"),
        ("is_acquired", "is_acquired"),
    ];

    let mut out = Vec::new();
    let mut offset: usize = 0;

    for (idx, line) in source.lines().enumerate() {
        let line_no = idx + 1;
        let Some(name) = line_fn_name(line) else {
            offset = offset.saturating_add(line.len().saturating_add(1));
            continue;
        };

        for (want, label) in protected {
            if name == want {
                if let Some(end_line) = find_fn_end_line(source, offset, line_no) {
                    out.push(ProtectedFnRange {
                        name: label,
                        start_line: line_no,
                        end_line,
                    });
                }
            }
        }

        offset = offset.saturating_add(line.len().saturating_add(1));
    }

    out
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    if !line.starts_with("@@") {
        return None;
    }
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }
    let old_tok = parts.get(1)?;
    let new_tok = parts.get(2)?;
    let old_start = parse_hunk_range_start(old_tok)?;
    let new_start = parse_hunk_range_start(new_tok)?;
    Some((old_start, new_start))
}

fn parse_hunk_range_start(tok: &str) -> Option<usize> {
    let t = tok.strip_prefix('-').or_else(|| tok.strip_prefix('+'))?;
    let start = t.split(',').next().unwrap_or(t);
    start.parse::<usize>().ok()
}

fn validate_patch_protects_lifecycle_functions(cwd: &Path, patch: &str) -> Result<()> {
    let mut protected_by_file: BTreeMap<String, Vec<ProtectedFnRange>> = BTreeMap::new();
    for rel in extract_patch_paths(patch) {
        let path = cwd.join(&rel);
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let ranges = protected_fn_ranges(&src);
        if !ranges.is_empty() {
            protected_by_file.insert(rel, ranges);
        }
    }
    if protected_by_file.is_empty() {
        return Ok(());
    }

    let mut current_file: Option<String> = None;
    let mut in_hunk = false;
    let mut old_line: usize = 0;

    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() == 2 {
                let b = parts[1];
                let rel = b.strip_prefix("b/").unwrap_or(b);
                current_file = Some(rel.to_owned());
            } else {
                current_file = None;
            }
            in_hunk = false;
            continue;
        }

        if let Some((o, _n)) = parse_hunk_header(line) {
            old_line = o;
            in_hunk = true;
            continue;
        }

        if !in_hunk {
            continue;
        }

        let Some(ref rel) = current_file else {
            continue;
        };
        let ranges = protected_by_file.get(rel);

        let b = match line.as_bytes().first().copied() {
            Some(v) => v,
            None => continue,
        };

        match b {
            b' ' => {
                old_line = old_line.saturating_add(1);
            }
            b'-' => {
                if let Some(ranges) = ranges {
                    for r in ranges {
                        if old_line >= r.start_line && old_line <= r.end_line {
                            bail!(
                                "Refusing autofix patch: modifies protected lifecycle function `{}` in {}",
                                r.name,
                                rel
                            );
                        }
                    }
                }
                old_line = old_line.saturating_add(1);
            }
            b'+' => {
                if let Some(ranges) = ranges {
                    for r in ranges {
                        // Insertions are anchored "before old_line". Inserting before the fn
                        // signature line is ok; inserting anywhere after that line and up to the
                        // closing brace is a modification of the protected function.
                        if old_line > r.start_line && old_line <= r.end_line {
                            bail!(
                                "Refusing autofix patch: modifies protected lifecycle function `{}` in {}",
                                r.name,
                                rel
                            );
                        }
                    }
                }
            }
            b'\\' => {}
            _ => {}
        }
    }

    Ok(())
}

fn validate_patch(cwd: &Path, allow_paths: &[PathBuf], patch: &str) -> Result<()> {
    let patch = patch.trim();
    if patch.is_empty() {
        bail!("LLM returned empty patch");
    }

    // Require it to look like a unified diff.
    let looks_like_diff = patch.lines().any(|l| {
        l.starts_with("diff --git ")
            || l.starts_with("--- ")
            || l.starts_with("+++ ")
            || l.starts_with("@@ ")
    });
    if !looks_like_diff {
        bail!("LLM output does not look like a unified diff (missing ---/+++ headers)");
    }

    for line in patch.lines() {
        if !(line.starts_with('+') || line.starts_with('-')) {
            continue;
        }

        // Ignore diff headers.
        if line.starts_with("+++ ") || line.starts_with("--- ") {
            continue;
        }

        // Hard block attempts to (re)define lifecycle check functions.
        if line.contains("fn is_installed")
            || line.contains("fn is_built")
            || line.contains("fn is_acquired")
        {
            bail!(
                "Refusing autofix patch: must not change lifecycle check functions (is_installed/is_built/is_acquired)"
            );
        }

        // Block "paper over" patterns.
        if line.starts_with('+') {
            // Match `|| true` with any whitespace (including `||true`).
            let mut rest = line;
            while let Some(idx) = rest.find("||") {
                rest = &rest[idx + 2..];
                let t = rest.trim_start();
                if let Some(after) = t.strip_prefix("true") {
                    let ok_boundary = match after.as_bytes().first().copied() {
                        None => true,
                        Some(b) => !b.is_ascii_alphanumeric() && b != b'_',
                    };
                    if ok_boundary {
                        bail!("Refusing autofix patch: must not introduce `|| true`");
                    }
                }
            }
        }
    }

    let cwd_abs = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let allow_abs: Vec<PathBuf> = allow_paths
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();

    for rel in extract_patch_paths(patch) {
        if rel.starts_with('/') {
            bail!("Refusing autofix patch: absolute path '{rel}'");
        }
        if has_parent_dir_components(&rel) {
            bail!("Refusing autofix patch: path traversal in '{rel}'");
        }
        let abs = cwd_abs.join(&rel);
        if !allow_abs.iter().any(|root| abs.starts_with(root)) {
            bail!(
                "Refusing autofix patch: touches '{}' which is outside allowed roots",
                rel
            );
        }
    }

    validate_patch_protects_lifecycle_functions(cwd, patch)?;

    Ok(())
}

fn apply_patch(cwd: &Path, patch: &str) -> Result<()> {
    // `git apply` expects the patch stream to be newline-terminated; many LLM providers
    // (and our own trimming) remove trailing newlines.
    let mut patch_bytes = patch.as_bytes().to_vec();
    if !patch_bytes.ends_with(b"\n") {
        patch_bytes.push(b'\n');
    }

    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .args(["apply", "--whitespace=nowarn", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let mut child = cmd.spawn().context("Spawning git apply")?;
    {
        let mut stdin = child.stdin.take().context("Opening git apply stdin")?;
        stdin
            .write_all(&patch_bytes)
            .context("Writing patch to git apply")?;
    }
    let status = child.wait().context("Waiting for git apply")?;
    if !status.success() {
        bail!("git apply failed with status {status}");
    }
    Ok(())
}

fn run_llm(prompt: String, cwd: &Path) -> Result<String> {
    let cfg = crate::llm::resolve_config_for_call().map_err(anyhow::Error::msg)?;
    let stdin = prompt.into_bytes();

    let out = match cfg.default_provider {
        crate::llm::LlmProviderId::Codex => {
            let inv = crate::llm::providers::codex::build(&cfg, cwd, stdin)
                .map_err(anyhow::Error::msg)?;
            let res = crate::llm::run_call(inv.cmd, inv.call).map_err(anyhow::Error::msg)?;
            crate::llm::providers::codex::finalize(res, inv.last_message_file.path())
                .map_err(anyhow::Error::msg)?
        }
        crate::llm::LlmProviderId::Claude => {
            let inv = crate::llm::providers::claude::build(&cfg, cwd, stdin)
                .map_err(anyhow::Error::msg)?;
            let res = crate::llm::run_call(inv.cmd, inv.call).map_err(anyhow::Error::msg)?;
            crate::llm::providers::claude::finalize(res).map_err(anyhow::Error::msg)?
        }
    };

    Ok(strip_outer_code_fence(out))
}

pub(crate) fn run_and_apply(
    cfg: &crate::AutoFixConfig,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    reason: &str,
    failure: &str,
) -> Result<()> {
    let cwd = detect_cwd(cfg, recipe_path)?;
    let allow_paths = normalize_allow_paths(&cwd, cfg);

    let child_source = read_file(recipe_path)?;
    let child_prompt = extract_autofix_prompt_block(&child_source);

    let mut comment_prompt = child_prompt;

    let extends = parse_extends(&child_source);
    let base_path = extends
        .as_deref()
        .and_then(|base_rel| resolve_base_path(base_rel, recipe_path, search_path));
    let base_source = match &base_path {
        Some(p) => Some(read_file(p)?),
        None => None,
    };
    if comment_prompt.is_none() {
        if let Some(bs) = base_source.as_deref() {
            comment_prompt = extract_autofix_prompt_block(bs);
        }
    }

    // Kernel-specific context: if we can find linux-deps, include it too.
    let mut extra_files: Vec<(PathBuf, String)> = Vec::new();
    if let Some(sp) = search_path {
        let linux_deps = sp.join("linux-deps.rhai");
        if linux_deps.is_file() {
            let combined = format!(
                "{}\n{}",
                child_source,
                base_source.as_deref().unwrap_or_default()
            );
            if combined.contains("linux-deps") {
                let content = read_file(&linux_deps)?;
                extra_files.push((linux_deps, content));
            }
        }
    }

    let extra_prompt = read_extra_prompt(cfg.prompt_file.as_deref())?;

    let prompt = build_prompt(
        &cwd,
        &allow_paths,
        recipe_path,
        search_path,
        defines,
        reason,
        failure,
        &child_source,
        base_path.as_deref(),
        base_source.as_deref(),
        &extra_files,
        comment_prompt.as_deref(),
        extra_prompt.as_deref(),
    );

    let patch = run_llm(prompt, &cwd).context("LLM invocation failed")?;
    validate_patch(&cwd, &allow_paths, &patch)?;
    apply_patch(&cwd, &patch)?;
    Ok(())
}
