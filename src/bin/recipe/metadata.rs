use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub(crate) struct RecipeMetadata {
    pub(crate) name: String,
    pub(crate) version: Option<String>,
    pub(crate) description: Option<String>,
}

impl RecipeMetadata {
    /// Read the loader's merged declaration without executing recipe code.
    pub(crate) fn load(path: &Path, search_path: Option<&Path>) -> anyhow::Result<Self> {
        let declared = levitate_recipe::declared_metadata(path, search_path)?;
        let field = |name: &str| {
            if declared.dynamic_fields.contains(name) {
                Some("<dynamic>".to_owned())
            } else {
                declared.fields.get(name).cloned()
            }
        };
        Ok(Self {
            name: field("name").unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_owned()
            }),
            version: field("version"),
            description: field("description"),
        })
    }
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
