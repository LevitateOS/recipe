//! Input discovery: base recipe, Rhai imports, and declared extras.
//!
//! Resolution is the executor's: `//! extends:` and `import "..."` paths
//! resolve relative to the referring file's directory first, then against the
//! executor search path; the modules come from [`crate::recipes::imports`].
//! Anything else — absolute host paths, missing files, recipe symlinks,
//! undeclared directories, secret-bearing names — fails with an explicit
//! error instead of staging unchecked host state.
//!
//! Staged domain names preserve relative directories under `/inputs`. Literal
//! imports and dependency declarations are discovered by parsing, never by
//! evaluating recipe code on the coordinator.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::domain::{MAIN_RECIPE_NAME as MAIN_NAME, MAX_INPUT_NAME};
/// Explicit assets include installation DVDs; stream bounded copies.
const MAX_INPUT_FILE: u64 = 16 * 1024 * 1024 * 1024;
const MAX_INPUTS_TOTAL: u64 = 32 * 1024 * 1024 * 1024;
/// Transitive import depth; deeper nesting is an explicit failure, not a hang.
const MAX_IMPORT_DEPTH: usize = 16;
/// Entries (files, directories, links) one invocation stages. A recipe with a
/// larger filesystem-tree input says so in `NEEDS.storage_max_objects`, which
/// is then the cap ([`discover_inputs`]).
const MAX_INPUT_ENTRIES: u64 = 100_000;

/// The input name of the root of a system change, which Recipe gives the
/// root's effects recipe read-only (README, "Root effects").
pub(crate) const SYSTEM_ROOT_INPUT: &str = "root";

/// Top-level folders of a [`SYSTEM_ROOT_INPUT`] that are not staged: run-time
/// data, mount points, home folders and the boot partition. The effects
/// recipe reads only `etc/`, `usr/` and the root's loader and libraries;
/// these may be large (a whole desktop's `var/`), change while they are read
/// (`var/log`, mounted `proc`, `sys`, `dev`), or hold this very run's scratch
/// pool (`var/tmp` of an installer's target).
const SYSTEM_ROOT_LEFT_OUT: [&str; 13] = [
    "var",
    "boot",
    "home",
    "root",
    "tmp",
    "run",
    "proc",
    "sys",
    "dev",
    "mnt",
    "media",
    "srv",
    "lost+found",
];

/// Extra directory inputs that are whole filesystem trees: absolute links in
/// them resolve within the tree, and no `.git` or `target` is left out as in a
/// source checkout.
fn filesystem_tree(name: &str) -> bool {
    matches!(name, "rootfs" | "target" | SYSTEM_ROOT_INPUT)
        || name == crate::domain::DEPENDENCY_INPUT
        || name.starts_with(&format!("{}/", crate::domain::DEPENDENCY_INPUT))
        || name == crate::domain::DEPENDENCY_TOOLS
}

/// Whether `entry` of a [`SYSTEM_ROOT_INPUT`] is left out, the one rule for
/// what of a system root is staged. A top-level folder of
/// [`SYSTEM_ROOT_LEFT_OUT`] is, but only a real folder: a top-level link
/// (`lib -> usr/lib`) is kept, as the effects recipe finds the loader through
/// it. So is what the secret-bearing guard refuses (`etc/pki/tls/cert.pem`, a
/// link to the CA bundle; a host's private keys): a whole root is never
/// refused for its key material, and the effects recipe reads none of it.
fn left_out_of_system_root(entry: &walkdir::DirEntry) -> bool {
    if entry.depth() == 0 {
        return false;
    }
    if entry.depth() == 1
        && entry.file_type().is_dir()
        && entry
            .file_name()
            .to_str()
            .is_some_and(|name| SYSTEM_ROOT_LEFT_OUT.contains(&name))
    {
        return true;
    }
    reject_secret_bearing_host(entry.path(), SYSTEM_ROOT_INPUT).is_err()
}

/// A [`SYSTEM_ROOT_INPUT`] given live ([`ExtraInput::live`]): only its top
/// level is read. Each top-level folder that [`left_out_of_system_root`] keeps
/// becomes an [`InputKind::Bind`], each top-level link is kept as a link, and
/// top-level files (a swap file, `.autorelabel`) are left out: the effects
/// recipe reads none. Nothing below the top level is listed, hashed or copied.
fn discover_live_root(collector: &mut Collector<'_>, extra: &ExtraInput) -> Result<()> {
    if extra.name != SYSTEM_ROOT_INPUT {
        bail!(
            "input preparation failure: only the system root input {SYSTEM_ROOT_INPUT:?} is bound live, not {:?}",
            extra.name
        );
    }
    if !std::fs::symlink_metadata(&extra.path)?.is_dir() {
        bail!(
            "input preparation failure: the system root {} is not a directory",
            extra.path.display()
        );
    }
    collector.stage_kind(&extra.name, &extra.path, InputKind::Directory)?;
    let mut links = HashMap::new();
    for entry in walkdir::WalkDir::new(&extra.path)
        .min_depth(1)
        .max_depth(1)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry.context("input preparation failure: cannot list the system root")?;
        if left_out_of_system_root(&entry) {
            continue;
        }
        let relative = entry
            .file_name()
            .to_str()
            .context("input preparation failure: non-Unicode name in the system root")?;
        let name = format!("{}/{relative}", extra.name);
        if entry.file_type().is_dir() {
            collector.stage_kind(&name, entry.path(), InputKind::Bind)?;
        } else if entry.file_type().is_symlink() {
            let target = std::fs::read_link(entry.path())?;
            let target = target
                .to_str()
                .context("input preparation failure: non-Unicode symlink")?;
            links.insert(relative.to_owned(), target.to_owned());
            collector.stage_kind(&name, entry.path(), InputKind::Symlink(target.to_owned()))?;
        }
    }
    for relative in links.keys() {
        validate_tree_link(relative, &links, true)?;
    }
    Ok(())
}

/// Kind and link bytes participate in the retained input identity.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum InputKind {
    File,
    Directory,
    /// Alias of a prior regular input, resolved only inside the private snapshot.
    Hardlink(String),
    /// Relative target confined to the explicitly selected source tree.
    Symlink(String),
    /// A host folder of a live system root ([`ExtraInput::live`]) that the
    /// launcher binds read-only at this name for the run, in its own mount
    /// namespace (`crate::launch`): never hashed, copied or sealed. Its
    /// identity is not known, so a run with one keeps no state
    /// (`crate::backend`).
    Bind,
}

/// One staged host file and its `/inputs/NAME` identity.
#[derive(Debug, Clone)]
pub struct StagedInput {
    pub kind: InputKind,
    /// Relative UTF-8 domain name under `/inputs`, without traversal or control bytes.
    pub name: String,
    /// Host file this was staged from (never crosses into the domain).
    pub host_path: PathBuf,
    /// Byte size.
    pub size: u64,
    /// sha256 of the file content.
    pub sha256: [u8; 32],
    /// Unix permission bits (`& 0o777`).
    pub mode: u32,
}

/// Caller-declared asset, source-closure, or cache file/tree: `NAME=host-path`.
#[derive(Debug, Clone)]
pub struct ExtraInput {
    /// Relative domain name; validated like every other input name.
    pub name: String,
    /// Host file to stage at `/inputs/{name}`.
    pub path: PathBuf,
    /// The root of a system change given to its effects recipe
    /// ([`SYSTEM_ROOT_INPUT`]): its top-level folders are bound read-only for
    /// a Ginux run as root ([`InputKind::Bind`]) instead of hashed and copied.
    /// Anywhere else it is staged like any input
    /// (`super::prepare_invocation`).
    pub live: bool,
}

/// Discovery result consumed by [`super::prepare_invocation`][crate::inputs].
#[derive(Debug)]
pub struct Discovered {
    /// Staged inputs sorted by domain name.
    pub inputs: Vec<StagedInput>,
    /// Main recipe file stem (invocation-id basis).
    pub recipe_stem: String,
    /// Canonical host recipe directories (leak-check markers, never emitted).
    pub host_dirs: Vec<PathBuf>,
}

/// Discover and hash the full input set without copying anything yet.
/// `declared_objects` is the recipe's `NEEDS.storage_max_objects`: with a
/// filesystem-tree input, the entry cap is that or [`MAX_INPUT_ENTRIES`],
/// whichever is larger; otherwise [`MAX_INPUT_ENTRIES`].
pub fn discover_inputs(
    recipe: &Path,
    search_path: Option<&Path>,
    extra_inputs: &[ExtraInput],
    declared_objects: u64,
) -> Result<Discovered> {
    let max_entries = if extra_inputs
        .iter()
        .any(|extra| filesystem_tree(&extra.name))
    {
        MAX_INPUT_ENTRIES.max(declared_objects)
    } else {
        MAX_INPUT_ENTRIES
    };
    let mut collector = Collector::new(search_path, max_entries);

    let recipe_source = read_recipe_file(recipe)?;
    let recipe_stem = recipe
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .with_context(|| {
            format!(
                "input preparation failure: recipe path has no file name: {}",
                recipe.display()
            )
        })?;

    let recipe_canonical = canonical_host(recipe)?;
    collector.note_host_dir(&recipe_canonical);
    collector.stage_kind(MAIN_NAME, recipe, InputKind::File)?;
    collector
        .visited
        .insert((recipe_canonical.clone(), MAIN_NAME.to_owned()));
    collector.collect_executable(&recipe_canonical, MAIN_NAME, &recipe_source, 0)?;

    for extra in extra_inputs {
        crate::domain::validate_input_name(&extra.name)?;
        let meta = std::fs::symlink_metadata(&extra.path)?;
        if extra.live {
            discover_live_root(&mut collector, extra)?;
        } else if meta.is_dir() {
            let filesystem_tree = filesystem_tree(&extra.name);
            let system_root = extra.name == SYSTEM_ROOT_INPUT;
            let mut links = HashMap::new();
            for entry in walkdir::WalkDir::new(&extra.path)
                .follow_links(false)
                .sort_by_file_name()
                .into_iter()
                .filter_entry(|entry| {
                    if system_root {
                        return !left_out_of_system_root(entry);
                    }
                    filesystem_tree
                        || entry.depth() != 1
                        || !matches!(entry.file_name().to_str(), Some(".git" | "target"))
                })
            {
                let entry = entry
                    .context("input preparation failure: cannot traverse declared input tree")?;
                let relative = entry.path().strip_prefix(&extra.path)?;
                let relative = relative
                    .to_str()
                    .context("input preparation failure: non-Unicode source name")?;
                let name = if relative.is_empty() {
                    extra.name.clone()
                } else {
                    format!("{}/{relative}", extra.name)
                };
                let kind = if entry.file_type().is_dir() {
                    InputKind::Directory
                } else if entry.file_type().is_symlink() {
                    let target = std::fs::read_link(entry.path())?;
                    let target = target
                        .to_str()
                        .context("input preparation failure: non-Unicode symlink")?;
                    links.insert(relative.to_owned(), target.to_owned());
                    InputKind::Symlink(target.to_owned())
                } else {
                    InputKind::File
                };
                collector.stage_kind(&name, entry.path(), kind)?;
            }
            for relative in links.keys() {
                validate_tree_link(relative, &links, filesystem_tree)?;
            }
        } else {
            collector.stage_kind(&extra.name, &extra.path, InputKind::File)?;
        }
        if let Ok(canonical) = extra.path.canonicalize() {
            collector.note_host_dir(&canonical);
        }
    }

    collector.finish(recipe_stem)
}

/// Resolve the discovered link graph without consulting the live host tree.
/// Expand links before processing `..`: lexical containment alone is not enough
/// when an intermediate link redirects to a shallower directory.
fn validate_tree_link(
    relative: &str,
    links: &HashMap<String, String>,
    filesystem_tree: bool,
) -> Result<()> {
    use std::collections::VecDeque;
    let mut pending = relative
        .split('/')
        .map(str::to_owned)
        .collect::<VecDeque<_>>();
    let mut parts = Vec::<String>::new();
    let mut followed = 0;
    while let Some(part) = pending.pop_front() {
        match part.as_str() {
            "" | "." => continue,
            ".." => {
                if parts.pop().is_none() {
                    bail!("input preparation failure: symlink escapes selected tree: {relative}");
                }
            }
            _ => {
                parts.push(part);
                if let Some(target) = links.get(&parts.join("/")) {
                    followed += 1;
                    if followed > 40
                        || target.is_empty()
                        || target.len() > MAX_INPUT_NAME
                        || (target.starts_with('/') && !filesystem_tree)
                    {
                        bail!(
                            "input preparation failure: invalid, absolute or cyclic symlink: {relative}"
                        );
                    }
                    if target.starts_with('/') {
                        parts.clear();
                    } else {
                        parts.pop();
                    }
                    for component in target.split('/').rev() {
                        pending.push_front(component.to_owned());
                    }
                }
            }
        }
        if !filesystem_tree && matches!(parts.first().map(String::as_str), Some(".git" | "target"))
        {
            bail!(
                "input preparation failure: symlink targets excluded source directory: {relative}"
            );
        }
    }
    Ok(())
}

/// Read one recipe/module file: must exist, be a regular file (never a
/// symlink or directory), and be valid UTF-8 Rhai source.
fn read_recipe_file(path: &Path) -> Result<String> {
    let meta = std::fs::symlink_metadata(path).with_context(|| {
        format!(
            "input preparation failure: cannot stat recipe file {}",
            path.display()
        )
    })?;
    if meta.file_type().is_symlink() {
        bail!(
            "input preparation failure: recipe file {} is a symlink \
             (only regular files are staged; resolve it to a real path first)",
            path.display()
        );
    }
    if !meta.file_type().is_file() {
        bail!(
            "input preparation failure: recipe path {} is not a regular file \
             (directories are never copied recursively)",
            path.display()
        );
    }
    std::fs::read_to_string(path)
        .with_context(|| format!("input preparation failure: cannot read {}", path.display()))
}

/// Canonical host identity for cycle detection (internal only).
fn canonical_host(path: &Path) -> Result<PathBuf> {
    path.canonicalize().with_context(|| {
        format!(
            "input preparation failure: cannot resolve {}",
            path.display()
        )
    })
}

/// Collector stages each domain name at most once and tracks the byte total.
struct Collector<'a> {
    search_path: Option<&'a Path>,
    staged: Vec<StagedInput>,
    by_name: HashMap<String, PathBuf>,
    visited: HashSet<(PathBuf, String)>,
    total: u64,
    host_dirs: Vec<PathBuf>,
    max_entries: u64,
}

impl<'a> Collector<'a> {
    fn new(search_path: Option<&'a Path>, max_entries: u64) -> Self {
        Self {
            search_path,
            staged: Vec::new(),
            by_name: HashMap::new(),
            visited: HashSet::new(),
            total: 0,
            host_dirs: Vec::new(),
            max_entries,
        }
    }

    fn note_host_dir(&mut self, canonical: &Path) {
        if let Some(parent) = canonical.parent() {
            let parent = parent.to_path_buf();
            if !self.host_dirs.contains(&parent) {
                self.host_dirs.push(parent);
            }
        }
    }

    /// Stage `host` at `/inputs/{name}`, hashing and validating it.
    ///
    /// Restaging the same domain name with the same host bytes is a no-op
    /// (shared imports); the same name with different bytes is an explicit
    /// duplicate-name failure.
    fn stage_kind(&mut self, name: &str, host: &Path, kind: InputKind) -> Result<()> {
        crate::domain::validate_input_name(name)?;
        if let Some(first) = self.by_name.get(name) {
            if same_file(first, host) {
                return Ok(());
            }
            bail!(
                "input preparation failure: duplicate input name {name:?}: {} and {} \
                 (domain input names must be unique; rename the extra input)",
                first.display(),
                host.display()
            );
        }
        if self.staged.len() as u64 >= self.max_entries {
            bail!(
                "input preparation failure: more than {} input entries \
                 (a recipe with a larger filesystem-tree input declares NEEDS.storage_max_objects)",
                self.max_entries
            );
        }
        let staged = if kind == InputKind::File {
            validate_and_hash(name, host)?
        } else {
            reject_secret_bearing_host(host, name)?;
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::symlink_metadata(host)?;
            let bytes = match &kind {
                InputKind::Symlink(target) => target.as_bytes(),
                _ => &[],
            };
            StagedInput {
                name: name.to_owned(),
                host_path: host.to_path_buf(),
                size: bytes.len() as u64,
                sha256: Sha256::digest(bytes).into(),
                mode: meta.permissions().mode() & 0o777,
                kind,
            }
        };
        self.total = self.total.checked_add(staged.size).with_context(|| {
            format!("input preparation failure: input byte total overflowed at {name:?}")
        })?;
        if self.total > MAX_INPUTS_TOTAL {
            bail!(
                "input preparation failure: inputs total {total} B exceeds {MAX_INPUTS_TOTAL} B \
                 (declare a smaller source closure)",
                total = self.total,
            );
        }
        self.by_name.insert(name.to_string(), host.to_path_buf());
        self.staged.push(staged);
        Ok(())
    }

    /// Stage the effective merged dependency graph. Base declarations that a
    /// child overrides do not become extra input authority or missing files.
    fn collect_executable(
        &mut self,
        canonical: &Path,
        domain_name: &str,
        source: &str,
        depth: usize,
    ) -> Result<()> {
        self.collect_related(canonical, domain_name, source, depth)?;
        // The pins file beside a recipe is part of it (docs/recipe-format.md,
        // section 2.7): staged beside it, so a change is a different build.
        let pins = crate::pins::Pins::path_of(canonical);
        if pins.is_file() {
            let name = Path::new(domain_name).with_extension("pins");
            let name = name
                .to_str()
                .context("input preparation failure: non-Unicode pins path")?;
            self.stage_kind(name, &pins, InputKind::File)?;
        }
        let declaration = crate::needs::declaration(canonical, self.search_path)?;
        for dependency in declaration.dependencies {
            let file =
                crate::recipes::dependency_file(&dependency.name).map_err(anyhow::Error::msg)?;
            let name = Path::new(domain_name)
                .parent()
                .unwrap_or(Path::new(""))
                .join(file);
            let name = name.to_str().context("non-Unicode dependency path")?;
            crate::domain::validate_input_name(name)?;
            let source = read_recipe_file(&dependency.recipe)?;
            self.note_host_dir(&dependency.recipe);
            self.stage_kind(name, &dependency.recipe, InputKind::File)?;
            if self
                .visited
                .insert((dependency.recipe.clone(), name.to_owned()))
            {
                self.collect_executable(&dependency.recipe, name, &source, depth + 1)?;
            }
        }
        Ok(())
    }

    /// Stage the base a recipe file extends and the modules they import,
    /// each under the name the worker looks for it by (beside its referrer's
    /// staged name, or at the top of the inputs when it came from the search
    /// path). Which files and where they lie is [`crate::recipes::related`],
    /// the engine's own rule.
    fn collect_related(
        &mut self,
        canonical: &Path,
        domain_name: &str,
        source: &str,
        depth: usize,
    ) -> Result<()> {
        // Dependencies are selected once from the merged executable
        // declaration by collect_executable above.
        let related = crate::recipes::related(canonical, source, self.search_path)?;
        let mut pending = vec![(canonical.to_path_buf(), domain_name.to_owned(), depth)];
        if let Some(base) = &related.base {
            let (base, name) =
                self.stage_related(&base.target, &base.found, base.beside, domain_name)?;
            if self.visited.insert((base.clone(), name.clone())) {
                pending.push((base, name, depth + 1));
            }
        }
        while let Some((file, name, depth)) = pending.pop() {
            if depth > MAX_IMPORT_DEPTH {
                bail!(
                    "input preparation failure: import/dependency nesting exceeds {MAX_IMPORT_DEPTH}"
                );
            }
            for import in related.edges.get(&file).into_iter().flatten() {
                let (module, module_name) =
                    self.stage_related(&import.file, &import.found, import.beside, &name)?;
                if self.visited.insert((module.clone(), module_name.clone())) {
                    pending.push((module, module_name, depth + 1));
                }
            }
        }
        Ok(())
    }

    /// Stage the base or module `target` that its referrer (staged as
    /// `referrer_name`) names, found at `host`, `beside` the referrer or in
    /// the search path. Returns its canonical path and its domain name.
    /// Preserves each reference's directory layout instead of flattening
    /// names.
    fn stage_related(
        &mut self,
        target: &str,
        host: &Path,
        beside: bool,
        referrer_name: &str,
    ) -> Result<(PathBuf, String)> {
        let name = if beside {
            Path::new(referrer_name)
                .parent()
                .unwrap_or(Path::new(""))
                .join(target)
        } else {
            PathBuf::from(target)
        };
        let name = name
            .to_str()
            .context("input preparation failure: non-Unicode domain path")?
            .to_owned();
        crate::domain::validate_input_name(&name)?;
        // Validate the selected path before canonicalization, preserving symlink rejection.
        read_recipe_file(host)?;
        let canonical = canonical_host(host)?;
        self.note_host_dir(&canonical);
        self.stage_kind(&name, host, InputKind::File)?;
        Ok((canonical, name))
    }

    fn finish(self, recipe_stem: String) -> Result<Discovered> {
        let mut inputs = self.staged;
        inputs.sort_by(|left, right| left.name.cmp(&right.name));
        use std::os::unix::fs::MetadataExt;
        let mut inodes = HashMap::<(u64, u64), (String, [u8; 32], u32)>::new();
        for input in &mut inputs {
            if input.kind != InputKind::File {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&input.host_path)?;
            let inode = (metadata.dev(), metadata.ino());
            if let Some((name, hash, mode)) = inodes.get(&inode) {
                if *hash != input.sha256 || *mode != input.mode {
                    bail!("hardlinked input changed during discovery");
                }
                input.kind = InputKind::Hardlink(name.clone());
            } else {
                inodes.insert(inode, (input.name.clone(), input.sha256, input.mode));
            }
        }
        Ok(Discovered {
            inputs,
            recipe_stem,
            host_dirs: self.host_dirs,
        })
    }
}

/// Best-effort producer guard: well-known secret-bearing
/// locations and key material are never silently packaged. Explicit selection
/// plus regular-file-only staging is the mechanism; this is not a boundary.
fn reject_secret_bearing_host(host: &Path, name: &str) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = host.as_os_str().as_bytes();
    for component in bytes.split(|byte| *byte == b'/') {
        match component {
            b".ssh" | b".gnupg" | b"id_rsa" | b"id_ed25519" | b".netrc" | b".git-credentials"
            | b"credentials.json" => {
                bail!(
                    "input preparation failure: refusing secret-bearing input {name:?} \
                     (host path names a credential location; stage only declared build inputs)"
                )
            }
            _ => {}
        }
        if component.len() > 4
            && (component.ends_with(b".key")
                || (component.ends_with(b".pem") && !public_certificate_bundle(host)))
        {
            bail!(
                "input preparation failure: refusing secret-bearing input {name:?} \
                 (host path names key material; stage only declared build inputs)"
            );
        }
    }
    Ok(())
}

/// Public CA bundles are build data, unlike PEM private keys. Keep this
/// best-effort guard bounded and never follow a host symlink to classify it.
fn public_certificate_bundle(host: &Path) -> bool {
    use std::io::Read;
    const LIMIT: u64 = 4 * 1024 * 1024;
    let Ok(file) = crate::fsx::open_regular(host) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file.take(LIMIT + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > LIMIT {
        return false;
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return false;
    };
    text.contains("-----BEGIN CERTIFICATE-----") && !text.contains("PRIVATE KEY-----")
}

/// Validate one host file and hash it by streaming (never fully buffered).
fn validate_and_hash(name: &str, host: &Path) -> Result<StagedInput> {
    let meta = std::fs::symlink_metadata(host).with_context(|| {
        format!(
            "input preparation failure: cannot stat input {name:?} at {}",
            host.display()
        )
    })?;
    if meta.file_type().is_symlink() {
        bail!(
            "input preparation failure: input {name:?} at {} is a symlink \
             (only regular files are staged; stage the target file explicitly)",
            host.display()
        );
    }
    if !meta.file_type().is_file() {
        bail!(
            "input preparation failure: input {name:?} at {} is not a regular file \
             (directories are never copied recursively)",
            host.display()
        );
    }
    reject_secret_bearing_host(host, name)?;
    let size = meta.len();
    if size > MAX_INPUT_FILE {
        bail!(
            "input preparation failure: input {name:?} is {size} B, exceeding {MAX_INPUT_FILE} B \
             (declare a smaller source closure)"
        );
    }
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o777
    };
    let sha256 = hash_file(host, size)?;
    Ok(StagedInput {
        kind: InputKind::File,
        name: name.to_string(),
        host_path: host.to_path_buf(),
        size,
        sha256,
        mode,
    })
}

/// Stream `host` through sha256, failing if it grows while being read.
pub fn hash_file(host: &Path, expected: u64) -> Result<[u8; 32]> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut source = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(host)
        .with_context(|| format!("input preparation failure: cannot open {}", host.display()))?;
    let mut hasher = Sha256::new();
    let copied = std::io::copy(
        &mut Read::by_ref(&mut source).take(expected + 1),
        &mut hasher,
    )?;
    if copied != expected {
        bail!(
            "input preparation failure: {} changed while reading ({copied} B vs {expected} B)",
            host.display(),
        );
    }
    Ok(hasher.finalize().into())
}

/// Same host file reached under two names (e.g. via search path overlap).
fn same_file(first: &Path, second: &Path) -> bool {
    match (first.canonicalize(), second.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => first == second,
    }
}
