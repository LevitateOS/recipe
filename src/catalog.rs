//! What a front end may show about a recipe before anything runs.
//!
//! An entry is read from the compiled recipe alone: the declared name, version
//! and description, the optional `ABOUT` block, NEEDS, the dependency names and
//! the lifecycle hooks the file defines. No setup, hook or helper is evaluated
//! and no Ginux launcher is needed, so a whole collection can be listed by an
//! ordinary user.

use anyhow::{Context, Result, bail, ensure};
use rhai::{ASTFlags, Expr, Stmt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// The version of the JSON printed by `recipe list --json`.
pub const SCHEMA_VERSION: u32 = 1;

const HOOKS: [&str; 5] = ["acquire", "build", "install", "remove", "cleanup"];

/// One recipe as a catalog entry.
///
/// `ABOUT` is the recipe author's own description (kind, category, longer
/// text, home page, licence, what it adds, notes on steps). It must be a
/// `const` literal map like NEEDS; its fields are passed through as written.
/// NEEDS is enforced by Ginux; `ABOUT` is a claim.
pub fn entry(recipe: &Path, search_path: Option<&Path>) -> Result<serde_json::Value> {
    let declaration = crate::needs::declaration(recipe, search_path)?;
    let metadata = crate::core::executor::declared_metadata(&declaration.recipe, search_path)?;
    let engine = crate::core::invocation::compile_engine();
    let compiled =
        crate::core::executor::compile_recipe(&engine, &declaration.recipe, search_path)?;

    let mut about = None;
    for statement in compiled.source_ast.statements() {
        if let Stmt::Var(binding, flags, _) = statement
            && binding.0.name == "ABOUT"
        {
            ensure!(about.is_none(), "recipe must declare ABOUT at most once");
            ensure!(
                flags.contains(ASTFlags::CONSTANT) && matches!(&binding.1, Expr::Map(..)),
                "ABOUT must be a const literal map"
            );
            about = Some(
                crate::needs::literal(&binding.1)
                    .context("ABOUT must be literal data, without helper calls or variables")?,
            );
        }
    }

    let field = |name: &str| {
        (!metadata.dynamic_fields.contains(name))
            .then(|| metadata.fields.get(name).cloned())
            .flatten()
    };
    let name = field("name").unwrap_or_else(|| {
        declaration
            .recipe
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_owned()
    });
    let names = |build_only: bool| -> Vec<&str> {
        declaration
            .dependencies
            .iter()
            .filter(|dependency| dependency.build_only == build_only)
            .map(|dependency| dependency.name.as_str())
            .collect()
    };
    let hooks: Vec<&str> = HOOKS
        .into_iter()
        .filter(|hook| {
            compiled
                .ast
                .iter_functions()
                .any(|function| function.name == *hook)
        })
        .collect();

    Ok(serde_json::json!({
        "name": name,
        "version": field("version"),
        "description": field("description"),
        "path": declaration.recipe,
        "about": about,
        "needs": declaration.needs,
        "deps": names(false),
        "build_deps": names(true),
        "hooks": hooks,
    }))
}

/// The file a source of recipes describes itself in, beside its recipes.
pub const SOURCE_FILE: &str = "repo.toml";
/// The file `recipe index` writes and a source publishes.
pub const INDEX_FILE: &str = "index.json";
/// The version of the JSON in [`INDEX_FILE`].
pub const INDEX_SCHEMA_VERSION: u32 = 1;

/// What a source says about itself. `name` is the short name in `SOURCE:NAME`;
/// `uses` names the other sources its recipes' dependencies may come from.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub name: String,
    pub title: String,
    pub about: String,
    pub address: String,
    pub keeper: String,
    #[serde(default)]
    pub uses: Vec<String>,
}

fn is_source_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Reads a folder's `repo.toml`.
pub fn source(folder: &Path) -> Result<Source> {
    let path = folder.join(SOURCE_FILE);
    let text = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "cannot read {}; a source describes itself there (name, title, about, address, keeper)",
            path.display()
        )
    })?;
    let source: Source = toml::from_str(&text)
        .with_context(|| format!("{} does not describe a source", path.display()))?;
    ensure!(
        is_source_name(&source.name),
        "{}: name must be lower-case letters, digits and '-', got {:?}",
        path.display(),
        source.name
    );
    for (field, value) in [
        ("title", &source.title),
        ("about", &source.about),
        ("address", &source.address),
        ("keeper", &source.keeper),
    ] {
        ensure!(!value.trim().is_empty(), "{}: {field} is empty", path.display());
    }
    for used in &source.uses {
        ensure!(
            is_source_name(used) && *used != source.name,
            "{}: uses must name other sources, got {used:?}",
            path.display()
        );
    }
    Ok(source)
}

/// What `recipe index` would write for a folder.
pub struct Index {
    /// The whole of `index.json`.
    pub text: String,
    pub recipes: usize,
    pub commit: String,
}

/// The index of a folder of recipes: every [`entry`] without its path, with the
/// checksum and size of the recipe file and of the files beside it that share
/// its name, the source's own description and the commit those files are at.
///
/// The same folder at the same commit always gives the same text. Nothing is
/// returned while a recipe cannot be read or a listed file differs from what
/// is committed, so a published index never promises a file nobody can fetch.
pub fn index(folder: &Path) -> Result<Index> {
    let source = source(folder)?;

    let mut names = Vec::new();
    for item in std::fs::read_dir(folder)
        .with_context(|| format!("cannot read {}", folder.display()))?
    {
        let name = item?.file_name();
        let Some(name) = name.to_str() else {
            bail!("{}: file name is not UTF-8", folder.join(&name).display());
        };
        names.push(name.to_owned());
    }
    names.sort();
    let stems: Vec<&str> = names
        .iter()
        .filter_map(|name| name.strip_suffix(".rhai"))
        .collect();
    // A file beside a recipe belongs to the longest recipe name it starts
    // with: `python3.12.svg` is `python3.12`'s, not `python3`'s.
    let owner = |name: &str| -> Option<&str> {
        stems
            .iter()
            .copied()
            .filter(|stem| {
                !name.ends_with(".rhai")
                    && name.len() > stem.len() + 1
                    && name.starts_with(stem)
                    && name.as_bytes()[stem.len()] == b'.'
            })
            .max_by_key(|stem| stem.len())
    };
    let file = |name: &str| -> Result<serde_json::Value> {
        let path = folder.join(name);
        ensure!(
            path.symlink_metadata()?.is_file(),
            "{} is not a regular file",
            path.display()
        );
        let bytes =
            std::fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
        Ok(serde_json::json!({
            "path": name,
            "sha256": hex::encode(Sha256::digest(&bytes)),
            "bytes": bytes.len(),
        }))
    };

    let mut recipes = Vec::new();
    let mut problems = Vec::new();
    let mut listed = vec![SOURCE_FILE.to_owned()];
    for stem in &stems {
        let recipe = format!("{stem}.rhai");
        let mut value = match entry(&folder.join(&recipe), Some(folder)) {
            Ok(value) => value,
            Err(error) => {
                problems.push(format!("{recipe}: {error:#}"));
                continue;
            }
        };
        if value["name"] != *stem {
            problems.push(format!(
                "{recipe}: declares the name {}; a recipe is looked up by its file name, so they must agree",
                value["name"]
            ));
            continue;
        }
        let mut files = vec![file(&recipe)?];
        listed.push(recipe);
        for name in names.iter().filter(|name| owner(name) == Some(*stem)) {
            files.push(file(name)?);
            listed.push(name.clone());
        }
        let fields = value.as_object_mut().expect("an entry is an object");
        fields.remove("path");
        fields.insert("files".to_owned(), files.into());
        recipes.push(value);
    }
    ensure!(
        problems.is_empty(),
        "no index for {}: {} cannot be listed\n  {}",
        folder.display(),
        if problems.len() == 1 { "a recipe" } else { "some recipes" },
        problems.join("\n  ")
    );

    let commit = commit(folder, &listed)?;
    let count = recipes.len();
    let index = serde_json::json!({
        "schema_version": INDEX_SCHEMA_VERSION,
        "source": source,
        "commit": commit,
        "recipes": recipes,
    });
    Ok(Index {
        text: serde_json::to_string_pretty(&index)? + "\n",
        recipes: count,
        commit,
    })
}

/// The commit the listed files are at: the last one that changed anything in
/// the folder other than the index, so committing the index does not change
/// what the next index says.
fn commit(folder: &Path, listed: &[String]) -> Result<String> {
    let git = |literal: bool, arguments: &[&str]| -> Result<String> {
        let mut command = std::process::Command::new("git");
        command.arg("-C").arg(folder).args(arguments);
        if literal {
            command.env("GIT_LITERAL_PATHSPECS", "1");
        }
        let output = command
            .output()
            .context("recipe index reads the commit with git; cannot run git")?;
        ensure!(
            output.status.success(),
            "git {} failed in {}: {}\nA source is a Git repository; the index names the commit its files are at.",
            arguments[0],
            folder.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8(output.stdout)?.trim_end().to_owned())
    };

    // With history cut off, the last commit would look like the one that
    // changed every file, and each index would name a different commit.
    ensure!(
        git(false, &["rev-parse", "--is-shallow-repository"])? != "true",
        "{} is a shallow clone; recipe index needs the full history (in CI, fetch all of it)",
        folder.display()
    );
    let mut status = vec!["status", "--porcelain", "--untracked-files=all", "--ignored", "--"];
    status.extend(listed.iter().map(String::as_str));
    let changed = git(true, &status)?;
    ensure!(
        changed.is_empty(),
        "these files differ from what is committed; commit them, then run recipe index:\n{changed}"
    );
    let exclude = format!(":(exclude){INDEX_FILE}");
    let commit = git(false, &["log", "-1", "--format=%H", "--", ".", &exclude])?;
    ensure!(!commit.is_empty(), "{} has no commit yet", folder.display());
    Ok(commit)
}
