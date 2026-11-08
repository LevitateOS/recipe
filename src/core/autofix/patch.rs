use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{parser, prompt};

fn validate_patch_protects_lifecycle_functions(cwd: &Path, patch: &str) -> Result<()> {
    let mut protected_by_file: BTreeMap<String, Vec<parser::ProtectedRange>> = BTreeMap::new();
    for rel in parser::extract_patch_paths(patch) {
        let path = cwd.join(&rel);
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let ranges = parser::protected_ranges(&src);
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

        if let Some(path) = line.strip_prefix("+++ b/") {
            current_file = Some(path.to_owned());
            in_hunk = false;
            continue;
        }

        if let Some((o, _n)) = parser::parse_hunk_header(line) {
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
                                "Refusing autofix patch: modifies protected lifecycle function or recipe declaration `{}` in {}",
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
                                "Refusing autofix patch: modifies protected lifecycle function or recipe declaration `{}` in {}",
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

        let changed = &line[1..];
        if changed
            .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            .any(|token| matches!(token, "NEEDS" | "RECIPE_API" | "deps" | "build_deps"))
            || changed.trim_start().starts_with("//! extends:")
        {
            bail!(
                "Refusing autofix patch: recipe NEEDS/API/dependency authority must be edited explicitly"
            );
        }

        // Hard block attempts to (re)define lifecycle check functions.
        if crate::lifecycle::CHECKS
            .iter()
            .any(|check| line.contains(&format!("fn {check}")))
        {
            bail!(
                "Refusing autofix patch: must not change lifecycle check functions ({})",
                crate::lifecycle::CHECKS.join("/")
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

    for rel in parser::extract_patch_paths(patch) {
        if rel.starts_with('/') {
            bail!("Refusing autofix patch: absolute path '{rel}'");
        }
        if parser::has_parent_dir_components(&rel) {
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

    let paths = parser::extract_patch_paths(patch);
    if paths.is_empty() || paths.len() > 32 {
        bail!("autofix requires between 1 and 32 source paths");
    }
    for line in patch.lines() {
        if (line.starts_with("+++ ") || line.starts_with("--- "))
            && !(line.starts_with("+++ b/") || line.starts_with("--- a/"))
        {
            bail!("autofix supports modifications of existing files with a/ and b/ headers");
        }
    }
    for relative in paths {
        if relative.chars().any(char::is_whitespace)
            || relative.contains('"')
            || relative.contains('\\')
        {
            bail!("autofix patch paths must be unquoted relative source names");
        }
        let path = cwd_abs.join(&relative);
        if !std::fs::symlink_metadata(&path)?.is_file() {
            bail!("autofix source must be an existing regular file");
        }
        let actual = path.canonicalize()?;
        if !allow_abs.iter().any(|root| actual.starts_with(root)) {
            bail!("autofix path resolves outside allowed roots: {relative}");
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

    let mut cmd = crate::process::git();
    cmd.current_dir(cwd)
        .args(["apply", "--whitespace=nowarn", "-"]);
    // Coordinator stdout carries the final JSON, so git output is diagnostics.
    let finished = crate::process::Run::new(cmd)
        .stdin(patch_bytes)
        .route(
            crate::process::Route::Diagnostics,
            crate::process::Route::Diagnostics,
        )
        .keep(crate::process::Keep::Tail(64 * 1024))
        .run()
        .context("running git apply")?;
    if !finished.exit.success() {
        bail!("git apply failed with {}", finished.exit);
    }
    Ok(())
}

fn run_llm(prompt: String, cwd: &Path, profile: Option<&str>) -> Result<String> {
    let reply = crate::llm::complete(prompt, cwd, profile).map_err(anyhow::Error::msg)?;
    Ok(prompt::strip_outer_code_fence(reply))
}

pub(crate) fn run_and_apply(
    cfg: &crate::AutoFixConfig,
    recipe_path: &Path,
    search_path: Option<&Path>,
    defines: &[(String, String)],
    reason: &str,
    failure: &str,
    profile: Option<&str>,
) -> Result<()> {
    let cwd = prompt::detect_cwd(cfg, recipe_path)?;
    let allow_paths = prompt::normalize_allow_paths(&cwd, cfg);

    let child_source = prompt::read_file(recipe_path)?;
    let child_prompt = prompt::extract_autofix_prompt_block(&child_source);

    let mut comment_prompt = child_prompt;

    let extends = crate::recipes::parse_extends(&child_source);
    let base_path = extends
        .as_deref()
        .filter(|base_rel| crate::recipes::check_related_target(base_rel).is_ok())
        .and_then(|base_rel| crate::recipes::find_related(base_rel, recipe_path, search_path));
    let base_source = match &base_path {
        Some(p) => Some(prompt::read_file(p)?),
        None => None,
    };
    if comment_prompt.is_none()
        && let Some(bs) = base_source.as_deref()
    {
        comment_prompt = prompt::extract_autofix_prompt_block(bs);
    }

    // Context: the recipes this one declares as `deps`/`build_deps`, found by
    // the engine's rule. Input preparation already rejected dynamic declarations.
    let mut extra_files: Vec<(PathBuf, String)> = Vec::new();
    for source in std::iter::once(child_source.as_str()).chain(base_source.as_deref()) {
        let references = crate::inputs::static_references(source).unwrap_or_default();
        for (file, module) in references {
            let Some(path) = (!module)
                .then(|| crate::recipes::find_related(&file, recipe_path, search_path))
                .flatten()
            else {
                continue;
            };
            if !extra_files.iter().any(|(known, _)| *known == path) {
                let content = prompt::read_file(&path)?;
                extra_files.push((path, content));
            }
        }
    }

    let extra_prompt = prompt::read_extra_prompt(cfg.prompt_file.as_deref())?;

    let prompt = prompt::build_prompt(
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

    let patch = run_llm(prompt, &cwd, profile).context("LLM invocation failed")?;
    validate_patch(&cwd, &allow_paths, &patch)?;
    apply_patch(&cwd, &patch)?;
    Ok(())
}
