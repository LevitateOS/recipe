//! Recipe state management - read/write recipe variables
//!
//! Recipes contain their own state (installed, installed_version, installed_files).
//! This module provides functions to read and update these variables in recipe files.

use anyhow::{Context, Result};
use std::path::Path;

/// Read a variable value from a recipe file
pub fn get_var<T: FromRecipeVar>(recipe_path: &Path, var_name: &str) -> Result<Option<T>> {
    let content = std::fs::read_to_string(recipe_path)
        .with_context(|| format!("Failed to read recipe: {}", recipe_path.display()))?;

    // Find the variable declaration: let <var_name> = <value>;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("let ") {
            if let Some(after_name) = rest.strip_prefix(var_name) {
                // Check for word boundary: next char must be whitespace or '='
                let first_char = after_name.chars().next();
                if first_char == Some(' ') || first_char == Some('=') || first_char == Some('\t') {
                    let after_name = after_name.trim();
                    if let Some(value_part) = after_name.strip_prefix('=') {
                        let value_str = value_part.trim().trim_end_matches(';').trim();
                        return T::from_recipe_str(value_str).map(Some);
                    }
                }
            }
        }
    }

    Ok(None)
}

/// Set a variable value in a recipe file
pub fn set_var<T: ToRecipeVar>(recipe_path: &Path, var_name: &str, value: &T) -> Result<()> {
    let content = std::fs::read_to_string(recipe_path)
        .with_context(|| format!("Failed to read recipe: {}", recipe_path.display()))?;

    let value_str = value.to_recipe_str();
    let new_line = format!("let {} = {};", var_name, value_str);
    let var_pattern = format!("let {} ", var_name);
    let var_pattern_eq = format!("let {}=", var_name);

    let mut found = false;
    let mut lines: Vec<String> = content
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with(&var_pattern) || trimmed.starts_with(&var_pattern_eq) {
                found = true;
                // Preserve original indentation
                let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
                format!("{}{}", indent, new_line)
            } else {
                line.to_string()
            }
        })
        .collect();

    // If variable doesn't exist, add it after other state variables or at the beginning
    if !found {
        let insert_pos = find_state_insert_position(&lines);
        lines.insert(insert_pos, new_line);
    }

    let new_content = lines.join("\n");
    std::fs::write(recipe_path, new_content)
        .with_context(|| format!("Failed to write recipe: {}", recipe_path.display()))?;

    Ok(())
}

/// Find the best position to insert a new state variable
fn find_state_insert_position(lines: &[String]) -> usize {
    // Look for existing state variables and insert after them
    let state_vars = ["installed", "installed_version", "installed_at", "installed_files"];
    let mut last_state_line = 0;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        for var in &state_vars {
            if trimmed.starts_with(&format!("let {} ", var)) || trimmed.starts_with(&format!("let {}=", var)) {
                last_state_line = i + 1;
            }
        }
    }

    if last_state_line > 0 {
        return last_state_line;
    }

    // Otherwise insert after version/description variables (after definition section)
    let def_vars = ["name", "version", "description", "depends"];
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        for var in &def_vars {
            if trimmed.starts_with(&format!("let {} ", var)) || trimmed.starts_with(&format!("let {}=", var)) {
                last_state_line = i + 1;
            }
        }
    }

    if last_state_line > 0 {
        // Add blank line before state section
        return last_state_line;
    }

    // Default to line 0
    0
}

/// Trait for types that can be read from recipe variables
pub trait FromRecipeVar: Sized {
    fn from_recipe_str(s: &str) -> Result<Self>;
}

/// Trait for types that can be written to recipe variables
pub trait ToRecipeVar {
    fn to_recipe_str(&self) -> String;
}

// Implementations for basic types

impl FromRecipeVar for bool {
    fn from_recipe_str(s: &str) -> Result<Self> {
        match s {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => anyhow::bail!("Invalid boolean: {}", s),
        }
    }
}

impl ToRecipeVar for bool {
    fn to_recipe_str(&self) -> String {
        if *self { "true" } else { "false" }.to_string()
    }
}

impl FromRecipeVar for String {
    fn from_recipe_str(s: &str) -> Result<Self> {
        // Handle quoted strings
        if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
            Ok(s[1..s.len()-1].to_string())
        } else {
            Ok(s.to_string())
        }
    }
}

impl ToRecipeVar for String {
    fn to_recipe_str(&self) -> String {
        format!("\"{}\"", self.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

impl ToRecipeVar for str {
    fn to_recipe_str(&self) -> String {
        format!("\"{}\"", self.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

impl FromRecipeVar for i64 {
    fn from_recipe_str(s: &str) -> Result<Self> {
        s.parse().with_context(|| format!("Invalid integer: {}", s))
    }
}

impl ToRecipeVar for i64 {
    fn to_recipe_str(&self) -> String {
        self.to_string()
    }
}

impl FromRecipeVar for Vec<String> {
    fn from_recipe_str(s: &str) -> Result<Self> {
        // Parse Rhai array syntax: ["a", "b", "c"]
        if !s.starts_with('[') || !s.ends_with(']') {
            anyhow::bail!("Invalid array syntax: {}", s);
        }

        let inner = s[1..s.len()-1].trim();
        if inner.is_empty() {
            return Ok(vec![]);
        }

        let mut result = Vec::new();
        let mut current = String::new();
        let mut in_string = false;
        let mut escape_next = false;

        for ch in inner.chars() {
            if escape_next {
                // Handle escape sequences properly
                match ch {
                    '\\' => current.push('\\'),  // \\ -> \
                    '"' => current.push('"'),    // \" -> "
                    '\'' => current.push('\''),  // \' -> '
                    'n' => current.push('\n'),   // \n -> newline
                    't' => current.push('\t'),   // \t -> tab
                    'r' => current.push('\r'),   // \r -> carriage return
                    _ => {
                        // Unknown escape - preserve backslash and char
                        current.push('\\');
                        current.push(ch);
                    }
                }
                escape_next = false;
                continue;
            }

            match ch {
                '\\' => {
                    escape_next = true;
                }
                '"' => {
                    in_string = !in_string;
                }
                ',' if !in_string => {
                    let trimmed = current.trim();
                    if !trimmed.is_empty() {
                        result.push(String::from_recipe_str(trimmed)?);
                    }
                    current.clear();
                }
                _ => {
                    current.push(ch);
                }
            }
        }

        let trimmed = current.trim();
        if !trimmed.is_empty() {
            result.push(String::from_recipe_str(trimmed)?);
        }

        Ok(result)
    }
}

impl ToRecipeVar for Vec<String> {
    fn to_recipe_str(&self) -> String {
        let items: Vec<String> = self.iter().map(|s| s.to_recipe_str()).collect();
        format!("[{}]", items.join(", "))
    }
}

/// Unit type represents Rhai's () / nil
impl FromRecipeVar for () {
    fn from_recipe_str(s: &str) -> Result<Self> {
        if s == "()" {
            Ok(())
        } else {
            anyhow::bail!("Invalid unit: {}", s)
        }
    }
}

impl ToRecipeVar for () {
    fn to_recipe_str(&self) -> String {
        "()".to_string()
    }
}

/// Optional string value (either a string or ())
#[derive(Debug, Clone)]
pub enum OptionalString {
    Some(String),
    None,
}

impl FromRecipeVar for OptionalString {
    fn from_recipe_str(s: &str) -> Result<Self> {
        if s == "()" {
            Ok(OptionalString::None)
        } else {
            Ok(OptionalString::Some(String::from_recipe_str(s)?))
        }
    }
}

impl ToRecipeVar for OptionalString {
    fn to_recipe_str(&self) -> String {
        match self {
            OptionalString::Some(s) => s.to_recipe_str(),
            OptionalString::None => "()".to_string(),
        }
    }
}

impl From<Option<String>> for OptionalString {
    fn from(opt: Option<String>) -> Self {
        match opt {
            Some(s) => OptionalString::Some(s),
            None => OptionalString::None,
        }
    }
}

impl From<OptionalString> for Option<String> {
    fn from(opt: OptionalString) -> Self {
        match opt {
            OptionalString::Some(s) => Some(s),
            OptionalString::None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_test_recipe(content: &str) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.rhai");
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    #[test]
    fn test_get_bool_var() {
        let (_dir, path) = write_test_recipe(r#"
let name = "test";
let installed = false;
"#);

        let val: Option<bool> = get_var(&path, "installed").unwrap();
        assert_eq!(val, Some(false));
    }

    #[test]
    fn test_get_string_var() {
        let (_dir, path) = write_test_recipe(r#"
let name = "test-pkg";
let version = "1.0.0";
"#);

        let name: Option<String> = get_var(&path, "name").unwrap();
        assert_eq!(name, Some("test-pkg".to_string()));

        let version: Option<String> = get_var(&path, "version").unwrap();
        assert_eq!(version, Some("1.0.0".to_string()));
    }

    #[test]
    fn test_get_missing_var() {
        let (_dir, path) = write_test_recipe(r#"
let name = "test";
"#);

        let val: Option<bool> = get_var(&path, "installed").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn test_set_existing_var() {
        let (_dir, path) = write_test_recipe(r#"
let name = "test";
let installed = false;
"#);

        set_var(&path, "installed", &true).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("let installed = true;"));
    }

    #[test]
    fn test_set_new_var() {
        let (_dir, path) = write_test_recipe(r#"
let name = "test";
let version = "1.0.0";
"#);

        set_var(&path, "installed", &true).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("let installed = true;"));
    }

    #[test]
    fn test_get_array_var() {
        let (_dir, path) = write_test_recipe(r#"
let installed_files = ["/usr/bin/foo", "/usr/lib/bar.so"];
"#);

        let files: Option<Vec<String>> = get_var(&path, "installed_files").unwrap();
        assert_eq!(files, Some(vec!["/usr/bin/foo".to_string(), "/usr/lib/bar.so".to_string()]));
    }

    #[test]
    fn test_set_array_var() {
        let (_dir, path) = write_test_recipe(r#"
let name = "test";
let installed_files = [];
"#);

        let files = vec!["/usr/bin/test".to_string()];
        set_var(&path, "installed_files", &files).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains(r#"let installed_files = ["/usr/bin/test"];"#));
    }

    #[test]
    fn test_optional_string() {
        let (_dir, path) = write_test_recipe(r#"
let installed_version = ();
"#);

        let val: Option<OptionalString> = get_var(&path, "installed_version").unwrap();
        assert!(matches!(val, Some(OptionalString::None)));

        set_var(&path, "installed_version", &OptionalString::Some("1.0.0".to_string())).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains(r#"let installed_version = "1.0.0";"#));
    }

    #[test]
    fn test_var_substring_no_match() {
        // Test that get_var("installed") doesn't match "installed_files"
        let (_dir, path) = write_test_recipe(r#"
let installed_files = ["/usr/bin/foo"];
let installed = false;
"#);

        let val: Option<bool> = get_var(&path, "installed").unwrap();
        assert_eq!(val, Some(false));

        // set_var should also not affect installed_files
        set_var(&path, "installed", &true).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("let installed = true;"));
        assert!(content.contains(r#"let installed_files = ["/usr/bin/foo"];"#));
    }

    #[test]
    fn test_array_escape_sequences() {
        // Test that escape sequences in arrays are handled correctly
        let (_dir, path) = write_test_recipe(r#"
let files = ["C:\\path\\to\\file", "hello\"world", "tab\there"];
"#);

        let files: Option<Vec<String>> = get_var(&path, "files").unwrap();
        assert_eq!(files, Some(vec![
            "C:\\path\\to\\file".to_string(),
            "hello\"world".to_string(),
            "tab\there".to_string(),
        ]));
    }

    #[test]
    fn test_array_unknown_escape_preserved() {
        // Test that unknown escapes preserve the backslash
        let (_dir, path) = write_test_recipe(r#"
let pattern = ["\\d+", "\\s*"];
"#);

        let pattern: Option<Vec<String>> = get_var(&path, "pattern").unwrap();
        // \d should become \d (backslash preserved for unknown escape)
        assert_eq!(pattern, Some(vec![
            "\\d+".to_string(),
            "\\s*".to_string(),
        ]));
    }
}
