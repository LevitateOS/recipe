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
//!
//! Also here: what `recipe cleanup` deletes, the keep option remembered per
//! program (section 6.4), and `recipe storage`, everything Recipe holds by
//! kind (section 8.4).

use crate::needs::Declaration;
use crate::retained::{Reached, Scope};
use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

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
    pub(crate) fn kinds(self) -> &'static [Reached] {
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
/// `host_libc` is `HOST_LIBC`, the C library of the system installed on.
pub fn key(
    graph: &[Declaration],
    defines: &[(String, String)],
    prefix: Option<&str>,
    host_libc: &str,
) -> Result<String> {
    let main = graph.last().context("empty recipe graph")?;
    let mut keys = BTreeMap::new();
    node_key(graph, &main.recipe, defines, prefix, host_libc, &mut keys)
}

/// The state key of every recipe of `graph`, by recipe path.
pub(crate) fn keys(
    graph: &[Declaration],
    defines: &[(String, String)],
    prefix: Option<&str>,
    host_libc: &str,
) -> Result<BTreeMap<PathBuf, String>> {
    let main = graph.last().context("empty recipe graph")?;
    let mut keys = BTreeMap::new();
    let main_key = node_key(graph, &main.recipe, defines, prefix, host_libc, &mut keys)?;
    keys.insert(main.recipe.clone(), main_key);
    Ok(keys)
}

fn node_key(
    graph: &[Declaration],
    recipe: &Path,
    defines: &[(String, String)],
    prefix: Option<&str>,
    host_libc: &str,
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
                let key = node_key(graph, &dependency.recipe, defines, None, host_libc, keys)?;
                keys.insert(dependency.recipe.clone(), key.clone());
                key
            }
        };
        dependencies.push((&dependency.name, dependency.build_only, key));
    }
    // The recipe's files by name and content, wherever its folder lies.
    let content = node.content()?;
    let mut defines = defines.to_vec();
    defines.sort();
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&(
        "recipe-state-key-v1",
        crate::identity::Declared::current(),
        content,
        defines,
        prefix,
        host_libc,
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
    host_libc: &str,
) -> Result<Recorded> {
    let on_disk = on_disk(state_dir, destination, graph)?;
    let main = graph.last().context("empty recipe graph")?;
    let own = Scope::of(state_dir, &main.recipe, destination)?;
    let state = match &own {
        None => State::None,
        Some(scope) => match scope.reached(&key(graph, defines, prefix, host_libc)?)? {
            None => State::None,
            Some(Reached::Built) => State::Built,
            // A checkpoint is taken inside build, after the download step.
            Some(Reached::Downloaded | Reached::Checkpoint) => State::Downloaded,
        },
    };
    Ok(Recorded { state, on_disk })
}

/// How long `recipe cleanup` waits for a run of the program to end.
const CLEANUP_WAIT: Duration = Duration::from_secs(10);

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
        freed += scope.remove(what.kinds(), CLEANUP_WAIT)?;
    }
    Ok(freed)
}

/// `recipe cleanup` without a name: delete `what` of every program. A
/// program a run holds is reported and left for later; the others are
/// cleaned. With [`Material::All`] (or [`Material::Build`]) also the
/// publications no run holds any more ([`crate::publication::orphans`]).
pub fn cleanup_all(state_dir: Option<&Path>, what: Material) -> Result<u64> {
    let mut freed = 0;
    for scope in Scope::all(state_dir)? {
        match scope.remove(what.kinds(), CLEANUP_WAIT) {
            Ok(bytes) => freed += bytes,
            Err(error) => crate::diagnostics::diag!("recipe: warning: {error:#}; left for later"),
        }
    }
    if what != Material::Downloads {
        freed += crate::publication::remove_orphans(&crate::retained::state_root(state_dir)?);
    }
    Ok(freed)
}

/// After a successful install of `recipe` at `destination` (its outputs
/// published, its package activated): delete what the dependency runs made
/// for it keep (`dependencies`, their recipe files). The run's own scope was
/// cleaned by the run itself, under the destination lock it held
/// (`crate::launch`). A dependency destination in use by another run is left
/// alone and reported. Returns the bytes freed.
pub(crate) fn cleanup_dependencies(
    state_dir: Option<&Path>,
    destination: &Path,
    dependencies: &[&Path],
) -> u64 {
    let mut freed = 0;
    for recipe in dependencies {
        let at = crate::retained::dependency_destination(destination, recipe);
        let removed = Scope::of(state_dir, recipe, &at).and_then(|scope| match scope {
            Some(scope) => scope.remove(Material::All.kinds(), Duration::ZERO),
            None => Ok(0),
        });
        match removed {
            Ok(bytes) => freed += bytes,
            Err(error) => crate::diagnostics::diag!(
                "recipe: warning: the material of the dependency {} is kept: {error:#}; \
                 `recipe cleanup` deletes it later",
                recipe.display()
            ),
        }
    }
    freed
}

/// The keep option (docs/recipe-format.md, section 6.4), remembered per
/// program in the state root: the names of the programs whose downloads and
/// build material stay after a successful install.
const KEEP_FILE: &str = "keep.json";

#[derive(Default, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct KeepFile {
    programs: BTreeSet<String>,
}

fn read_keep(root: &Path) -> Result<KeepFile> {
    match std::fs::read(root.join(KEEP_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("invalid keep.json in the state root"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(KeepFile::default()),
        Err(error) => Err(error).context("cannot read keep.json in the state root"),
    }
}

/// The programs whose keep option is on.
pub fn keeping(state_dir: Option<&Path>) -> Result<BTreeSet<String>> {
    Ok(read_keep(&crate::retained::state_root(state_dir)?)?.programs)
}

/// Whether the keep option is on for the program `name`.
pub fn keep(state_dir: Option<&Path>, name: &str) -> Result<bool> {
    let root = crate::retained::state_root(state_dir)?;
    Ok(read_keep(&root)?.programs.contains(name))
}

/// Turn the keep option on or off for the program `name` (`--keep`,
/// `--no-keep`; `recipe remove` turns it off).
pub fn set_keep(state_dir: Option<&Path>, name: &str, on: bool) -> Result<()> {
    use fs2::FileExt;
    use std::io::Write;
    let root = crate::retained::state_root(state_dir)?;
    if !on && !root.join(KEEP_FILE).exists() {
        return Ok(());
    }
    std::fs::create_dir_all(&root)?;
    let lock = crate::fsx::open_lease(&root.join(".keep.lock"), true)?;
    lock.lock_exclusive().context("cannot lock keep.json")?;
    let mut file = read_keep(&root)?;
    let changed = if on {
        file.programs.insert(name.to_owned())
    } else {
        file.programs.remove(name)
    };
    if !changed {
        return Ok(());
    }
    let mut staged = tempfile::NamedTempFile::new_in(&root)?;
    staged.write_all(&serde_json::to_vec(&file)?)?;
    staged.as_file().sync_all()?;
    staged.persist(root.join(KEEP_FILE))?;
    crate::fsx::sync_directories(&[root.as_path()])?;
    Ok(())
}

/// One kind of what Recipe holds (docs/recipe-format.md, section 8.4).
#[derive(Clone, Debug, Serialize)]
pub struct Kind {
    pub kind: &'static str,
    pub bytes: u64,
    pub removable: bool,
    /// The command that removes it, or None.
    pub remove: Option<&'static str>,
    pub programs: Vec<ProgramBytes>,
    /// For `build_image` and `logs`.
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProgramBytes {
    pub name: String,
    pub bytes: u64,
}

/// Everything Recipe holds on disk, by kind, each byte counted once.
#[derive(Clone, Debug, Serialize)]
pub struct Storage {
    pub schema_version: u32,
    pub profile: PathBuf,
    pub state_dir: PathBuf,
    pub total_bytes: u64,
    /// Always all seven kinds, in the order of section 8.4.
    pub kinds: Vec<Kind>,
}

fn kind(
    kind: &'static str,
    remove: &'static str,
    programs: BTreeMap<String, u64>,
    path: Option<PathBuf>,
) -> Kind {
    Kind {
        kind,
        bytes: programs.values().sum(),
        removable: true,
        remove: Some(remove),
        programs: programs
            .into_iter()
            .filter(|(_, bytes)| *bytes > 0)
            .map(|(name, bytes)| ProgramBytes { name, bytes })
            .collect(),
        path,
    }
}

/// The program a scope's material belongs to: the recipe's name, or for a
/// dependency run made for a consumer (`<destination>/.deps/…`) the
/// consumer's, when its scope is known.
fn owner(scope: &Scope, all: &[Scope]) -> String {
    let stem = |recipe: &Path| {
        recipe
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let consumer = scope
        .destination
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == ".deps"))
        .and_then(Path::parent)
        .and_then(|destination| all.iter().find(|other| other.destination == destination));
    stem(&consumer.unwrap_or(scope).recipe)
}

/// `recipe storage`: what Recipe holds for the package store `store` and
/// the state root of `state_dir`. Nothing is read beyond file sizes and
/// Recipe's records; nothing is changed.
pub fn storage(state_dir: Option<&Path>, store: &crate::packages::PackageStore) -> Result<Storage> {
    let root = crate::retained::state_root(state_dir)?;
    let receipts = store.list()?;
    let installed_names: BTreeSet<String> = receipts
        .iter()
        .map(|package| package.name.clone())
        .collect();
    let keeping = read_keep(&root)?.programs;
    // Installed: the package store.
    let mut installed: BTreeMap<String, u64> = receipts
        .iter()
        .map(|package| (package.name.clone(), package.size_bytes.unwrap_or(0)))
        .collect();
    let store_bytes = store.storage()?.active_bytes;
    let files: u64 = installed.values().sum();
    // The profile's links and the receipt.
    if store_bytes > files {
        *installed.entry(String::new()).or_default() += store_bytes - files;
    }
    let mut downloaded = BTreeMap::new();
    let mut built = BTreeMap::new();
    let mut kept = BTreeMap::new();
    let mut unfinished = BTreeMap::new();
    // Publications no command took: left by an interrupted run (or outputs
    // of a producer run its caller did not take); `recipe cleanup` deletes
    // them.
    for (name, bytes) in crate::publication::published(&root) {
        *unfinished.entry(name).or_insert(0u64) += bytes;
    }
    let scopes = Scope::all(state_dir)?;
    for scope in &scopes {
        let name = owner(scope, &scopes);
        let keeps = installed_names.contains(&name) && keeping.contains(&name);
        for generation in scope.generations()? {
            let into = match generation.reached() {
                // An unfinished run's material counts only as unfinished.
                Reached::Checkpoint => &mut unfinished,
                _ if keeps => &mut kept,
                Reached::Downloaded => &mut downloaded,
                Reached::Built => &mut built,
            };
            *into.entry(name.clone()).or_insert(0u64) += generation.bytes();
        }
    }
    let image = crate::runtime::build_image()?;
    let logs = crate::invocation_log::bytes(&root);
    let mut installed = kind("installed", "recipe remove NAME", installed, None);
    // The profile's own bytes belong to no program.
    installed
        .programs
        .retain(|program| !program.name.is_empty());
    let kinds = vec![
        installed,
        kind(
            "downloaded",
            "recipe cleanup NAME --downloads",
            downloaded,
            None,
        ),
        kind("built", "recipe cleanup NAME --build", built, None),
        kind("kept", "recipe cleanup NAME", kept, None),
        kind(
            "unfinished",
            "recipe cleanup NAME --build",
            unfinished,
            None,
        ),
        Kind {
            kind: "build_image",
            bytes: image.bytes,
            removable: image.removable,
            remove: image.removable.then_some("recipe runtime remove"),
            programs: Vec::new(),
            path: Some(image.path),
        },
        Kind {
            kind: "logs",
            bytes: logs.bytes,
            removable: true,
            remove: Some("recipe cleanup --logs"),
            programs: Vec::new(),
            path: Some(logs.path),
        },
    ];
    Ok(Storage {
        schema_version: crate::catalog::SCHEMA_VERSION,
        profile: store.profile_path(),
        state_dir: root,
        total_bytes: kinds.iter().map(|kind| kind.bytes).sum(),
        kinds,
    })
}

/// Hold the published outputs of the state root of `state_dir` for the rest
/// of this process, while it reads them (a system change writes a producer's
/// outputs into a root): `recipe cleanup` does not delete them meanwhile.
pub fn hold_publications(state_dir: Option<&Path>) -> Result<()> {
    let base = crate::retained::state_root(state_dir)?.join("exports");
    std::fs::create_dir_all(&base)?;
    crate::publication::hold(&base.canonicalize()?)
}

/// `recipe cleanup --logs`: delete the diagnostic logs of every finished
/// invocation (a running one keeps its own). Returns the bytes freed.
pub fn cleanup_logs(state_dir: Option<&Path>) -> Result<u64> {
    crate::invocation_log::remove_all(&crate::retained::state_root(state_dir)?)
}
