use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

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

pub(super) fn strip_outer_code_fence(s: String) -> String {
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

pub(super) fn git_repo_root(from: &Path) -> Option<PathBuf> {
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

pub(super) fn detect_cwd(cfg: &crate::AutoFixConfig, recipe_path: &Path) -> Result<PathBuf> {
    if let Some(p) = &cfg.cwd {
        return Ok(p.clone());
    }

    let recipe_dir = recipe_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    if let Ok(here) = std::env::current_dir()
        && let Some(root) = git_repo_root(&here)
    {
        return Ok(root);
    }
    if let Some(root) = git_repo_root(&recipe_dir) {
        return Ok(root);
    }

    Ok(recipe_dir)
}

pub(super) fn normalize_allow_paths(cwd: &Path, cfg: &crate::AutoFixConfig) -> Vec<PathBuf> {
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

pub(super) fn parse_extends(source: &str) -> Option<String> {
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

pub(super) fn resolve_base_path(
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

pub(super) fn read_file(p: &Path) -> Result<String> {
    std::fs::read_to_string(p).with_context(|| format!("Reading {}", p.display()))
}

pub(super) fn extract_autofix_prompt_block(source: &str) -> Option<String> {
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

pub(super) fn read_extra_prompt(prompt_file: Option<&Path>) -> Result<Option<String>> {
    let Some(p) = prompt_file else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(p).with_context(|| format!("Reading {}", p.display()))?;
    let trimmed = text.trim();
    Ok((!trimmed.is_empty()).then_some(trimmed.to_owned()))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_prompt(
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
