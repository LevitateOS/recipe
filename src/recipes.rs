//! How recipes name each other: `//! extends:` headers, base/module targets
//! and `deps`/`build_deps` names. Input discovery (coordinator), the engine
//! (worker) and autofix all resolve through these rules, so what discovery
//! stages is what the engine looks for.

use std::path::{Component, Path, PathBuf};

/// Top-level bindings that declare dependencies: `deps` (needed by every
/// phase) and `build_deps` (needed only to build).
pub const DEPENDENCY_FIELDS: [&str; 2] = ["deps", "build_deps"];

/// Recipe bindings the engine owns or reads, which a `--define` must not shadow.
pub const RESERVED_NAMES: [&str; 6] = [
    "RUNTIME",
    "RECIPE_API",
    "NEEDS",
    "ctx",
    "deps",
    "build_deps",
];

/// Most nested recipes (main recipe plus dependency chain) one invocation enters.
pub const MAX_DEPTH: usize = 64;

/// The `//! extends: <path>` target in a recipe's leading comment block.
pub fn parse_extends(source: &str) -> Option<String> {
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

/// A base or module target must be a nonempty relative path that stays inside
/// the recipe tree.
pub fn check_related_target(target: &str) -> Result<(), &'static str> {
    if target.trim().is_empty() {
        return Err("is empty");
    }
    if target.starts_with('/') {
        return Err("is an absolute path");
    }
    if Path::new(target)
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err("escapes with `..`");
    }
    Ok(())
}

/// Locate `target`: beside the referring recipe first, then in the search path.
/// This is the rule for bases, modules and dependencies alike.
pub fn find_related(target: &str, referrer: &Path, search_path: Option<&Path>) -> Option<PathBuf> {
    let target = target.trim();
    referrer
        .parent()
        .into_iter()
        .chain(search_path)
        .map(|directory| directory.join(target))
        .find(|candidate| candidate.is_file())
}

/// The file an `import "target"` names: `target.rhai`.
pub fn module_file(target: &str) -> String {
    Path::new(target)
        .with_extension("rhai")
        .to_string_lossy()
        .into_owned()
}

/// The recipe file a `deps`/`build_deps` entry names: `<name>.rhai`, where
/// `name` is a plain relative name.
pub fn dependency_file(name: &str) -> Result<String, String> {
    if let Err(problem) = check_related_target(name) {
        return Err(format!("dependency name {name:?} {problem}"));
    }
    let path = Path::new(name);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(format!(
            "dependency name {name:?} must be a nonempty relative recipe name without `.`/`..` components"
        ));
    }
    Ok(format!("{name}.rhai"))
}
