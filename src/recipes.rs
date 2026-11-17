//! How recipes name each other: `//! extends:` headers, base/module targets
//! and `deps`/`build_deps` names. Input discovery (coordinator), the engine
//! (worker) all resolve through these rules, so what discovery
//! stages is what the engine looks for; [`related`] finds a recipe's base
//! and modules, for all of them.

use std::path::{Component, Path, PathBuf};

/// Top-level bindings that declare dependencies: `deps` (needed by every
/// phase) and `build_deps` (needed only to build).
pub const DEPENDENCY_FIELDS: [&str; 2] = ["deps", "build_deps"];

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
    locate_related(target, referrer, search_path).map(|(path, _)| path)
}

/// [`find_related`], and whether the file lies beside the referrer (`true`)
/// or in the search path (`false`).
pub fn locate_related(
    target: &str,
    referrer: &Path,
    search_path: Option<&Path>,
) -> Option<(PathBuf, bool)> {
    let target = target.trim();
    let candidate = |directory: &Path| Some(directory.join(target)).filter(|path| path.is_file());
    if let Some(path) = referrer.parent().and_then(candidate) {
        return Some((path, true));
    }
    search_path.and_then(candidate).map(|path| (path, false))
}

/// The base a recipe extends, as [`related`] found and read it.
#[derive(Debug)]
pub(crate) struct Base {
    /// The path `//! extends:` names.
    pub target: String,
    /// Where [`locate_related`] found it, as found (not canonical).
    pub found: PathBuf,
    /// Found beside the recipe, not in the search path.
    pub beside: bool,
    /// `found`, canonical: the base's identity.
    pub canonical: PathBuf,
    pub text: String,
}

/// One literal `import` of a recipe, base or module, resolved by [`related`].
#[derive(Debug)]
pub(crate) struct Import {
    /// The path as the import writes it (`"look"` in `import "look" as look;`).
    pub target: String,
    /// The file it names: [`module_file`] of `target`.
    pub file: String,
    /// Where [`locate_related`] found it, as found (not canonical).
    pub found: PathBuf,
    /// Found beside the importing file, not in the search path.
    pub beside: bool,
    /// `found`, canonical: the module's identity.
    pub canonical: PathBuf,
}

/// An imported module as [`related`] read it.
#[derive(Debug)]
pub(crate) struct Module {
    pub text: String,
    /// Compiled without the optimizer.
    pub ast: rhai::AST,
}

/// The files a recipe is compiled from besides itself and its pins file.
#[derive(Debug)]
pub(crate) struct Related {
    /// The base it extends.
    pub base: Option<Base>,
    /// Each module it, its base or a module imports, once, by canonical path.
    pub modules: std::collections::BTreeMap<PathBuf, Module>,
    /// The imports of each file walked (the recipe, its base by its
    /// canonical path, each module by its canonical path), in the order the
    /// file makes them.
    pub edges: std::collections::BTreeMap<PathBuf, Vec<Import>>,
}

/// The one rule for what the recipe `recipe` (whose text is `source`) is
/// compiled from besides itself, used by the engine's digests
/// (`compile_recipe`), input discovery and the placement check:
///
/// - its base: the `//! extends:` target, a relative path inside the
///   recipe tree, found beside the recipe, then in the search path; one
///   level only, a base that extends again is refused;
/// - the modules the recipe and its base import, recursively: literal paths
///   only (`import "look.rhai" as look;`), each named file found beside the
///   importing file, then in the search path, each module read once.
///
/// Imports are found on ASTs compiled without the optimizer, so an import is
/// found whatever engine later compiles the recipe.
pub(crate) fn related(
    recipe: &Path,
    source: &str,
    search_path: Option<&Path>,
) -> anyhow::Result<Related> {
    use anyhow::{Context, anyhow};
    use rhai::{ASTNode, Expr, Stmt};
    let base = match parse_extends(source) {
        None => None,
        Some(target) => {
            check_related_target(&target)
                .map_err(|problem| anyhow!("Base recipe '{target}' {problem}"))?;
            let (found, beside) =
                locate_related(&target, recipe, search_path).with_context(|| {
                    format!(
                        "Base recipe '{}' not found (child: {}, search_path: {:?})",
                        target,
                        recipe.display(),
                        search_path
                    )
                })?;
            let canonical = found.canonicalize().unwrap_or_else(|_| found.clone());
            let text = std::fs::read_to_string(&canonical)
                .with_context(|| format!("Failed to read base recipe: {}", canonical.display()))?;
            if parse_extends(&text).is_some() {
                anyhow::bail!(
                    "Recursive extends not supported: {} extends {} which also extends",
                    recipe.display(),
                    canonical.display()
                );
            }
            Some(Base {
                target,
                found,
                beside,
                canonical,
                text,
            })
        }
    };
    let plain = crate::core::invocation::compile_engine();
    // The base compiles first, as in `compile_recipe`; the recipe is walked
    // after it.
    let mut pending = Vec::new();
    let base_ast = base
        .as_ref()
        .map(|base| {
            plain.compile(&base.text).map_err(|e| {
                anyhow!(
                    "Failed to compile base recipe {}: {}",
                    base.canonical.display(),
                    e
                )
            })
        })
        .transpose()?;
    let ast = plain
        .compile(source)
        .map_err(|e| anyhow!("Failed to compile recipe {}: {}", recipe.display(), e))?;
    pending.push((recipe.to_path_buf(), ast));
    if let (Some(base), Some(ast)) = (&base, base_ast) {
        pending.push((base.canonical.clone(), ast));
    }
    let mut found = Related {
        base,
        modules: Default::default(),
        edges: Default::default(),
    };
    let mut depth = 0;
    while let Some((path, ast)) = pending.pop() {
        depth += 1;
        anyhow::ensure!(depth < 256, "too many imported modules");
        let mut targets = Vec::new();
        let mut computed = None;
        ast.walk(&mut |nodes| {
            if let Some(ASTNode::Stmt(Stmt::Import(import, position))) = nodes.last() {
                match &import.0 {
                    Expr::StringConstant(target, _) => targets.push(target.to_string()),
                    _ => computed = Some(*position),
                }
            }
            true
        });
        if let Some(position) = computed {
            let line = position
                .line()
                .map(|line| format!("line {line}: "))
                .unwrap_or_default();
            anyhow::bail!(
                "{}: {line}import takes a literal path (`import \"look.rhai\" as look;`), so Recipe can stage the module",
                path.display(),
            );
        }
        let mut edges = Vec::new();
        for target in targets {
            check_related_target(&target)
                .map_err(|problem| anyhow!("module {target:?} {problem}"))?;
            let file = module_file(&target);
            let (module, beside) =
                locate_related(&file, &path, search_path).with_context(|| {
                    format!(
                        "{}: module {target:?} not found beside it or in the recipe folder",
                        path.display()
                    )
                })?;
            let canonical = module.canonicalize().unwrap_or_else(|_| module.clone());
            if !found.modules.contains_key(&canonical) {
                let text = std::fs::read_to_string(&canonical)
                    .with_context(|| format!("cannot read module {}", canonical.display()))?;
                let ast = plain
                    .compile(&text)
                    .map_err(|e| anyhow!("cannot compile module {}: {e}", canonical.display()))?;
                pending.push((canonical.clone(), ast.clone()));
                found
                    .modules
                    .insert(canonical.clone(), Module { text, ast });
            }
            edges.push(Import {
                target,
                file,
                found: module,
                beside,
                canonical,
            });
        }
        found.edges.insert(path, edges);
    }
    Ok(found)
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
