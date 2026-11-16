use std::path::{Path, PathBuf};

/// Iterator over recipe files in a directory; modules (imported by recipes)
/// are not recipes.
pub(crate) fn enumerate_recipes(recipes_path: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    std::fs::read_dir(recipes_path)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "rhai"))
        .filter(|path| !levitate_recipe::catalog::is_module(path))
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

/// The one recipe folder: the explicit collection, which stays isolated (the
/// builder's OS recipes among them), or the user's own. What the OS ships is a
/// source, not a folder.
pub(crate) fn recipe_paths(selected: Option<PathBuf>) -> Vec<PathBuf> {
    if let Some(path) = selected {
        return vec![path];
    }
    vec![default_recipes_path()]
}

pub(crate) fn default_package_root() -> anyhow::Result<PathBuf> {
    if unsafe { libc::geteuid() } == 0 {
        return Ok(PathBuf::from("/var/lib/recipe"));
    }
    Ok(dirs::data_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot locate package data; select --package-root"))?
        .join("recipe"))
}
