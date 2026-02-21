use std::path::Path;

pub(super) fn extract_patch_paths(patch: &str) -> Vec<String> {
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

pub(super) fn has_parent_dir_components(path: &str) -> bool {
    Path::new(path)
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

#[derive(Debug, Clone)]
pub(super) struct ProtectedFnRange {
    pub(super) name: &'static str,
    pub(super) start_line: usize,
    pub(super) end_line: usize,
}

pub(super) fn line_fn_name(line: &str) -> Option<&str> {
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

pub(super) fn find_fn_end_line(
    source: &str,
    start_offset: usize,
    start_line: usize,
) -> Option<usize> {
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

pub(super) fn protected_fn_ranges(source: &str) -> Vec<ProtectedFnRange> {
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
            if name == want
                && let Some(end_line) = find_fn_end_line(source, offset, line_no)
            {
                out.push(ProtectedFnRange {
                    name: label,
                    start_line: line_no,
                    end_line,
                });
            }
        }

        offset = offset.saturating_add(line.len().saturating_add(1));
    }

    out
}

pub(super) fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
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

pub(super) fn parse_hunk_range_start(tok: &str) -> Option<usize> {
    let t = tok.strip_prefix('-').or_else(|| tok.strip_prefix('+'))?;
    let start = t.split(',').next().unwrap_or(t);
    start.parse::<usize>().ok()
}
