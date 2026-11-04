//! ctx block parsing and serialization
//!
//! Recipes store state in a `ctx` map literal. This module provides:
//! - `find_ctx_block`: locate ctx in source
//! - `serialize`: convert Map to Rhai literal
//! - `persist`: update ctx in source file

use anyhow::{Result, anyhow};
use rhai::Dynamic;

/// Find the byte range of `let ctx = #{...};` in source
///
/// Returns (start, end) offsets where start is the 'l' of 'let ctx'
/// and end is the byte after the closing ';'.
pub fn find_ctx_block(source: &str) -> Option<(usize, usize)> {
    // Find "let ctx = #{"
    let ctx_start = source.find("let ctx = #{")?;

    // Find the matching closing brace by tracking nesting
    let search_start = ctx_start + "let ctx = #{".len();
    let bytes = source.as_bytes();
    let mut depth = 1;
    let mut i = search_start;

    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b'"' => {
                // Skip string contents
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 2; // Skip escaped char
                        continue;
                    }
                    if bytes[i] == b'"' {
                        break;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    if depth != 0 {
        return None;
    }

    // i now points right after the closing '}'
    // Look for the trailing ';'
    let mut end = i;
    while end < bytes.len() && bytes[end].is_ascii_whitespace() {
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b';' {
        end += 1;
    }

    Some((ctx_start, end))
}

/// Serialize a Rhai Map to a multi-line ctx literal
pub fn serialize(map: &rhai::Map) -> String {
    let mut out = String::from("let ctx = #{\n");

    // Sort keys for deterministic output
    let mut keys: Vec<_> = map.keys().collect();
    keys.sort_by(|a, b| a.as_str().cmp(b.as_str()));

    for key in keys {
        let value = map.get(key).unwrap();
        out.push_str(&format!("    {}: {},\n", key, format_value(value)));
    }
    out.push_str("};");
    out
}

/// Format a Dynamic value as a Rhai literal
fn format_value(v: &Dynamic) -> String {
    if v.is_string() {
        // Escape the string properly
        let s = v.clone().into_string().unwrap();
        format!("\"{}\"", escape_string(&s))
    } else if v.is_int() {
        v.as_int().unwrap().to_string()
    } else if v.is_bool() {
        v.as_bool().unwrap().to_string()
    } else if v.is_unit() {
        "()".to_string()
    } else {
        // Fallback for other types
        format!("\"{}\"", escape_string(&v.to_string()))
    }
}

/// Escape special characters in strings
fn escape_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// Replace the ctx block in source with updated values
pub fn persist(source: &str, map: &rhai::Map) -> Result<String> {
    let (start, end) = find_ctx_block(source).ok_or_else(|| anyhow!("ctx block not found"))?;
    let new_ctx = serialize(map);
    Ok(format!("{}{}{}", &source[..start], new_ctx, &source[end..]))
}
