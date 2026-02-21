use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub(crate) struct RecipeMetadata {
    pub(crate) name: String,
    pub(crate) version: Option<String>,
    pub(crate) description: Option<String>,
}

impl RecipeMetadata {
    /// Load metadata from a recipe file by parsing its ctx block
    pub(crate) fn load(path: &Path) -> Self {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };

        let mut meta = Self::default();

        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("name:") {
                meta.name = extract_string_value(line);
            } else if line.starts_with("version:") {
                meta.version = Some(extract_string_value(line));
            } else if line.starts_with("description:") {
                meta.description = Some(extract_string_value(line));
            }
        }

        if meta.name.is_empty() {
            meta.name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_default();
        }

        meta
    }
}

/// Extract string value from a line like `name: "value",`
pub(crate) fn extract_string_value(line: &str) -> String {
    let Some(colon_pos) = line.find(':') else {
        return String::new();
    };
    let value_part = line[colon_pos + 1..].trim();
    value_part
        .trim_start_matches('"')
        .trim_end_matches(',')
        .trim_end_matches('"')
        .to_string()
}

/// Iterator over recipe files in a directory
pub(crate) fn enumerate_recipes(recipes_path: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    std::fs::read_dir(recipes_path)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "rhai"))
}

/// Default recipes directory (XDG compliant)
pub(crate) fn default_recipes_path() -> PathBuf {
    if let Ok(path) = std::env::var("RECIPE_PATH") {
        return PathBuf::from(path);
    }

    let data_home = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/share")
        });

    data_home.join("recipe/recipes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_recipes_dir() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let recipes_path = dir.path().to_path_buf();
        (dir, recipes_path)
    }

    fn write_recipe(recipes_path: &Path, name: &str, content: &str) -> PathBuf {
        let path = recipes_path.join(format!("{}.rhai", name));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_extract_string_value() {
        assert_eq!(extract_string_value("name: \"test\","), "test");
        assert_eq!(extract_string_value("version: \"1.0\","), "1.0");
        assert_eq!(
            extract_string_value("  description: \"A test package\","),
            "A test package"
        );
    }

    #[test]
    fn test_recipe_metadata_load() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        let recipe_path = write_recipe(
            &recipes_path,
            "test",
            r#"let ctx = #{
    name: "mypackage",
    version: "2.0",
    description: "A test package",
};"#,
        );

        let meta = RecipeMetadata::load(&recipe_path);
        assert_eq!(meta.name, "mypackage");
        assert_eq!(meta.version, Some("2.0".to_string()));
        assert_eq!(meta.description, Some("A test package".to_string()));
    }

    #[test]
    fn test_recipe_metadata_fallback_name() {
        let (_dir, recipes_path) = create_test_recipes_dir();
        let recipe_path = write_recipe(&recipes_path, "test", "let ctx = #{};");

        let meta = RecipeMetadata::load(&recipe_path);
        assert_eq!(meta.name, "test");
    }
}
