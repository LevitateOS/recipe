//! The state of a program and the material Recipe keeps for it
//! (docs/recipe-format.md, sections 6.2, 6.3 and 8.5), decided from Recipe's
//! own records, never from recipe code:
//!
//! - downloaded: a kept generation holds the run's downloads (`recipe
//!   download` left them, or an unfinished build that got past its download
//!   step), and its archive still has the SHA-256 Recipe recorded. The
//!   sources' and the pins' SHA-256 are part of the recipe text and the pins
//!   file, which the state key names;
//! - built: a kept finished result whose state key is the current one and
//!   whose archive still has the SHA-256 Recipe recorded;
//! - installed: the receipt (the caller reads it: `crate::packages`, or a
//!   root's receipt for the system scope).
//!
//! The state key names what can be known without running anything: the
//! recipe's text, its base, its pins file and its modules by SHA-256, the
//! defines, `PREFIX`, the engine's behaviour versions, and the same of each
//! dependency. A run reuses a result only by its full identity
//! (`crate::retained`), which also names the inputs' and the dependencies'
//! outputs' contents, the root image and the execution options; the key is
//! how `recipe state` and `recipe list` find that result without preparing
//! a run.

use crate::needs::Declaration;
use crate::retained::{Reached, Scope};
use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The state of a program (section 6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    None,
    Downloaded,
    Built,
    Installed,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Downloaded => "downloaded",
            Self::Built => "built",
            Self::Installed => "installed",
        }
    }
}

/// What a program holds on disk beside its installed files (section 8.2,
/// `on_disk`): kept downloads, kept results, unfinished runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct OnDisk {
    pub downloads: u64,
    pub build: u64,
    pub unfinished: u64,
}

/// What Recipe's records say of a program (not its receipt).
#[derive(Clone, Copy, Debug)]
pub struct Recorded {
    /// `None`, `Downloaded` or `Built`.
    pub state: State,
    pub on_disk: OnDisk,
}

/// What `recipe cleanup` deletes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Material {
    /// Downloads and build material.
    All,
    /// `--downloads`.
    Downloads,
    /// `--build`: kept results and unfinished runs.
    Build,
}

impl Material {
    fn kinds(self) -> &'static [Reached] {
        match self {
            Self::All => &[Reached::Downloaded, Reached::Checkpoint, Reached::Built],
            Self::Downloads => &[Reached::Downloaded],
            Self::Build => &[Reached::Checkpoint, Reached::Built],
        }
    }
}

/// The state key of the last recipe of `graph` (the main recipe; its
/// dependencies come before it, as [`crate::needs::declarations`] orders
/// them). `prefix` is the main recipe's `PREFIX`; dependencies have none.
pub fn key(
    graph: &[Declaration],
    defines: &[(String, String)],
    prefix: Option<&str>,
) -> Result<String> {
    let main = graph.last().context("empty recipe graph")?;
    let mut keys = BTreeMap::new();
    node_key(graph, &main.recipe, defines, prefix, &mut keys)
}

/// The state key of every recipe of `graph`, by recipe path.
pub(crate) fn keys(
    graph: &[Declaration],
    defines: &[(String, String)],
    prefix: Option<&str>,
) -> Result<BTreeMap<PathBuf, String>> {
    let main = graph.last().context("empty recipe graph")?;
    let mut keys = BTreeMap::new();
    let main_key = node_key(graph, &main.recipe, defines, prefix, &mut keys)?;
    keys.insert(main.recipe.clone(), main_key);
    Ok(keys)
}

fn node_key(
    graph: &[Declaration],
    recipe: &Path,
    defines: &[(String, String)],
    prefix: Option<&str>,
    keys: &mut BTreeMap<PathBuf, String>,
) -> Result<String> {
    let node = graph
        .iter()
        .find(|node| node.recipe == recipe)
        .context("a dependency is missing from the recipe graph")?;
    let mut dependencies = Vec::new();
    for dependency in &node.dependencies {
        let key = match keys.get(&dependency.recipe) {
            Some(key) => key.clone(),
            None => {
                let key = node_key(graph, &dependency.recipe, defines, None, keys)?;
                keys.insert(dependency.recipe.clone(), key.clone());
                key
            }
        };
        dependencies.push((&dependency.name, dependency.build_only, key));
    }
    let mut modules = Vec::new();
    for module in &node.declared.modules {
        let bytes = std::fs::read(module)
            .with_context(|| format!("cannot read the module {}", module.display()))?;
        modules.push((module, hex::encode(Sha256::digest(bytes))));
    }
    let mut defines = defines.to_vec();
    defines.sort();
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&(
        "recipe-state-key-v1",
        crate::identity::Declared::current(),
        &node.source_digests,
        modules,
        defines,
        prefix,
        dependencies,
    ))?)))
}

/// The scopes of a program: its own runs at `destination`, and the runs of
/// its dependencies made for it.
fn scopes(
    state_dir: Option<&Path>,
    destination: &Path,
    graph: &[Declaration],
) -> Result<(Option<Scope>, Vec<Scope>)> {
    let main = graph.last().context("empty recipe graph")?;
    let own = Scope::of(state_dir, &main.recipe, destination)?;
    let mut dependencies = Vec::new();
    for node in &graph[..graph.len() - 1] {
        let at = crate::retained::dependency_destination(destination, &node.recipe);
        if let Some(scope) = Scope::of(state_dir, &node.recipe, &at)? {
            dependencies.push(scope);
        }
    }
    Ok((own, dependencies))
}

/// Whether Recipe keeps anything for `recipe` at `destination` (cheap: no
/// archive is read).
pub fn keeps_anything(state_dir: Option<&Path>, recipe: &Path, destination: &Path) -> bool {
    Scope::of(state_dir, recipe, destination).is_ok_and(|scope| scope.is_some())
}

/// What the last recipe of `graph` holds at `destination` and in the
/// dependency runs made for it, by kind (no archive is read).
pub fn on_disk(
    state_dir: Option<&Path>,
    destination: &Path,
    graph: &[Declaration],
) -> Result<OnDisk> {
    let (own, dependencies) = scopes(state_dir, destination, graph)?;
    let mut on_disk = OnDisk::default();
    for scope in own.iter().chain(&dependencies) {
        for generation in scope.generations()? {
            let bytes = generation.bytes();
            match generation.reached() {
                Reached::Downloaded => on_disk.downloads += bytes,
                Reached::Built => on_disk.build += bytes,
                Reached::Checkpoint => on_disk.unfinished += bytes,
            }
        }
    }
    Ok(on_disk)
}

/// What Recipe's records say of the last recipe of `graph` at
/// `destination` (its lock and saved-state scope, as for a run).
pub fn recorded(
    state_dir: Option<&Path>,
    destination: &Path,
    graph: &[Declaration],
    defines: &[(String, String)],
    prefix: Option<&str>,
) -> Result<Recorded> {
    let on_disk = on_disk(state_dir, destination, graph)?;
    let main = graph.last().context("empty recipe graph")?;
    let own = Scope::of(state_dir, &main.recipe, destination)?;
    let state = match &own {
        None => State::None,
        Some(scope) => match scope.reached(&key(graph, defines, prefix)?)? {
            None => State::None,
            Some(Reached::Built) => State::Built,
            // A checkpoint is taken inside build, after the download step.
            Some(Reached::Downloaded | Reached::Checkpoint) => State::Downloaded,
        },
    };
    Ok(Recorded { state, on_disk })
}

/// `recipe cleanup NAME`: delete the program's `what` at `destination`, and
/// the same of the dependency runs made for it. Returns the bytes freed.
pub fn cleanup(
    state_dir: Option<&Path>,
    destination: &Path,
    graph: &[Declaration],
    what: Material,
) -> Result<u64> {
    let (own, dependencies) = scopes(state_dir, destination, graph)?;
    let mut freed = 0;
    for scope in own.iter().chain(&dependencies) {
        freed += scope.remove(what.kinds())?;
    }
    Ok(freed)
}

/// `recipe cleanup` without a name: delete `what` of every program.
pub fn cleanup_all(state_dir: Option<&Path>, what: Material) -> Result<u64> {
    let mut freed = 0;
    for scope in Scope::all(state_dir)? {
        freed += scope.remove(what.kinds())?;
    }
    Ok(freed)
}
