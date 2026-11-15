//! Recipe's system software contract: the receipt of what Recipe put in a root,
//! the plan for a change, and writing that plan into a root. The root written
//! is always the other (unlocked) root of an A/B system, or the one root of a
//! mutable system; the booted root of an A/B system is never written.
use crate::accounts;
pub use crate::accounts::Account;
use crate::target::{self, Entry};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
};
pub const RECEIPT: &str = "usr/lib/recipe/system.json";
pub const PROFILE: &str = "usr/lib/recipe/profile";
/// The reserved package a root's effects recipe (`usr/lib/recipe/effects.rhai`)
/// writes its caches as; no recipe installs under this name.
pub const EFFECTS_PACKAGE: &str = "system-effects";
/// The reserved package that is the root's base tree, the files of the system
/// release it runs (see `crate::release`). Only `recipe --system apply-release`
/// replaces it, and `record-base` writes it into an image tree at build time;
/// no recipe installs under this name and it is never removed.
pub const BASE_PACKAGE: &str = "base";
/// The reserved package that keeps the modules of the kernel a running root
/// booted when `apply-release` replaces its base (that kernel stays as the
/// second boot entry). Only one kernel's modules are kept: the next apply
/// gives them back to the base, which removes them unless they are the
/// running kernel's again.
pub const PREVIOUS_KERNEL_PACKAGE: &str = "previous-kernel";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub recipe: PathBuf,
    pub recipe_digest: String,
    pub architecture: String,
    pub target: String,
    pub dependencies: Vec<String>,
    pub files: BTreeMap<String, Entry>,
    /// Accounts this package's changes created in the root (kept when the
    /// package is removed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accounts: Vec<Account>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema_version: u32,
    pub packages: BTreeMap<String, Package>,
}
impl Default for Receipt {
    fn default() -> Self {
        Self {
            schema_version: 1,
            packages: BTreeMap::new(),
        }
    }
}
impl Receipt {
    pub fn parse(text: Option<&str>) -> Result<Self> {
        let result: Self = match text {
            Some(t) => serde_json::from_str(t)?,
            None => Self::default(),
        };
        ensure!(
            result.schema_version == 1,
            "unsupported system selection receipt"
        );
        ensure!(result.packages.len() <= 4096, "too many selected packages");
        for (name, p) in &result.packages {
            valid_name(name)?;
            ensure!(*name == p.name, "package identity mismatch");
            for path in p.files.keys() {
                allowed(path)?;
            }
        }
        Ok(result)
    }
    pub fn find(&self, requested: &Path) -> Result<&Package> {
        self.packages
            .values()
            .find(|p| {
                requested == Path::new(&p.name)
                    || requested == p.recipe
                    || requested.canonicalize().ok().as_ref() == Some(&p.recipe)
            })
            .context("system package is not selected")
    }
    pub fn affected(&self, packages: &[Payload], remove: Option<&str>) -> BTreeSet<String> {
        self.packages
            .values()
            .filter(|p| {
                Some(p.name.as_str()) == remove || packages.iter().any(|i| i.package.name == p.name)
            })
            .flat_map(|p| p.files.keys().cloned())
            .chain(
                packages
                    .iter()
                    .flat_map(|p| p.package.files.keys().cloned()),
            )
            .collect()
    }
}
#[derive(Clone, Debug)]
pub struct Payload {
    pub package: Package,
    pub root: PathBuf,
    pub paths: BTreeMap<String, String>,
    pub removals: Vec<String>,
    /// sysusers.d `u`/`g`/`m` lines the recipe declared (`target_finalize`'s
    /// `accounts`).
    pub accounts: Vec<String>,
    /// State seeds (`target_finalize`'s `seed: "var"`): paths under `var/`,
    /// created in the root's `/var` where missing (see [`seed`]). Not owned.
    pub seeds: BTreeMap<String, Entry>,
}
fn valid_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid system package name"
    );
    Ok(())
}
pub fn allowed(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && target::relative(Path::new(name))? == Path::new(name),
        "invalid system path"
    );
    ensure!(
        ![
            "boot",
            "var",
            "home",
            "root",
            "proc",
            "sys",
            "dev",
            "run",
            "tmp",
            "lost+found"
        ]
        .iter()
        .any(|p| name == *p || name.starts_with(&format!("{p}/"))),
        "system package cannot own shared/runtime data: {name}"
    );
    ensure!(
        !name.starts_with("usr/lib/recab/")
            && name != RECEIPT
            && name != format!("{PROFILE}/.installed.json")
            && name != "etc/fstab",
        "reserved system identity/receipt path: {name}"
    );
    Ok(())
}
/// Convert coordinator-verified output sets, including runtime dependencies.
/// Normal `package` outputs live in this root's profile; explicit `system`
/// outputs use installed root paths and require authored target metadata.
pub fn payloads(result: &serde_json::Value, target: &str) -> Result<Vec<Payload>> {
    ensure!(
        result["outputs"]["system"].is_string() || result["outputs"]["package"].is_string(),
        "selected recipe produced no installable system/package output"
    );
    let producers=result["system_producers"].as_array().context("Recipe did not return coordinator-owned system provenance; use a compatible Recipe runtime")?;
    let mut payloads = Vec::new();
    let mut identities = BTreeMap::new();
    // Dependencies the root already holds (not produced again) keep their
    // place in the written packages' `dependencies`.
    let mut held = BTreeMap::new();
    for present in result["system_present"].as_array().into_iter().flatten() {
        if let (Some(recipe), Some(name)) = (present["recipe"].as_str(), present["name"].as_str()) {
            held.insert(recipe.to_owned(), name.to_owned());
        }
    }
    for producer in producers {
        let outputs = &producer["outputs"];
        let (path, prefix, system) = if let Some(p) = outputs["system"].as_str() {
            (p, "", true)
        } else if let Some(p) = outputs["package"].as_str() {
            (p, PROFILE, false)
        } else {
            continue;
        };
        let root = PathBuf::from(path);
        ensure!(
            fs::symlink_metadata(&root)?.is_dir(),
            "system output must be a real directory"
        );
        if system {
            ensure!(
                root.join(target::MANIFEST).is_file(),
                "system outputs require target_finalize and intended metadata"
            );
        }
        let manifest = if root.join(target::MANIFEST).exists() {
            target::load(&root)?
        } else {
            target::inspect(&root)?
        };
        let proof = &producer["producer"];
        ensure!(proof["version"] == 1, "unsupported producer receipt");
        let name = producer["name"]
            .as_str()
            .context("system output needs ctx.name")?
            .to_owned();
        valid_name(&name)?;
        ensure!(
            target == "effects" || name != EFFECTS_PACKAGE,
            "{EFFECTS_PACKAGE} is reserved for the root's effects recipe; rename the recipe"
        );
        ensure!(
            name != BASE_PACKAGE && name != PREVIOUS_KERNEL_PACKAGE,
            "{BASE_PACKAGE} is reserved for the root's base tree, which only `recipe --system apply-release` replaces; rename the recipe"
        );
        ensure!(
            !identities.values().any(|n| n == &name),
            "duplicate producer package name"
        );
        identities.insert(
            proof["recipe"]
                .as_str()
                .context("producer recipe missing")?
                .to_owned(),
            name.clone(),
        );
        let mut files = BTreeMap::new();
        let mut paths = BTreeMap::new();
        let mut seeds = BTreeMap::new();
        let var_seed = system && manifest.var_seed;
        let mut seed_links = Vec::new();
        for (relative, mut entry) in manifest.entries {
            if relative.is_empty() {
                continue;
            }
            if var_seed && (relative == "var" || relative.starts_with("var/")) {
                if entry.kind == "hardlink" {
                    // A seed is copied, not linked: it takes its first name's bytes.
                    seed_links.push(relative.clone());
                }
                paths.insert(relative.clone(), relative.clone());
                seeds.insert(relative, entry);
                continue;
            }
            let path = if prefix.is_empty() {
                relative.clone()
            } else {
                format!("{prefix}/{relative}")
            };
            allowed(&path)?;
            // Owner names are resolved against the root the change is
            // written into (`resolve_owners`).
            if entry.kind == "hardlink" {
                entry.link = Some(if prefix.is_empty() {
                    entry.link.context("missing hardlink")?
                } else {
                    format!("{prefix}/{}", entry.link.context("missing hardlink")?)
                });
            }
            paths.insert(path.clone(), relative);
            files.insert(path, entry);
        }
        for (path, entry) in &files {
            if entry.kind == "hardlink" {
                let to = entry.link.as_ref().context("missing hardlink target")?;
                let first = files
                    .get(to)
                    .context("hardlink target is outside the package")?;
                ensure!(
                    first.kind == "file"
                        && first.sha256 == entry.sha256
                        && first.uid == entry.uid
                        && first.gid == entry.gid
                        && first.mode == entry.mode
                        && first.xattrs == entry.xattrs,
                    "inconsistent intended hardlink at {path}"
                );
            }
        }
        for path in seed_links {
            let entry = seeds.get_mut(&path).unwrap();
            let link = entry.link.take().context("missing hardlink")?;
            entry.kind = "file".into();
            let source = paths
                .get(&link)
                .cloned()
                .context("hardlink seed has no target")?;
            paths.insert(path, source);
        }
        ensure!(!files.is_empty(), "empty system output");
        let declared = manifest.accounts.clone();
        let removals = manifest
            .removals
            .into_iter()
            .map(|p| {
                if prefix.is_empty() {
                    p
                } else {
                    format!("{prefix}/{p}")
                }
            })
            .collect::<Vec<_>>();
        for p in &removals {
            allowed(p)?;
        }
        let package = Package {
            name,
            version: producer["version"]
                .as_str()
                .context("system output needs ctx.version")?
                .into(),
            recipe: PathBuf::from(proof["recipe"].as_str().unwrap()),
            recipe_digest: proof["recipe_digest"]
                .as_str()
                .context("producer digest missing")?
                .into(),
            architecture: proof["architecture"]
                .as_str()
                .context("producer architecture missing")?
                .into(),
            target: target.into(),
            dependencies: proof["dependencies"]
                .as_array()
                .context("producer dependency identities missing")?
                .iter()
                .map(|d| d.as_str().context("invalid dependency").map(str::to_owned))
                .collect::<Result<_>>()?,
            files,
            accounts: Vec::new(),
        };
        payloads.push(Payload {
            package,
            root,
            paths,
            removals,
            accounts: declared,
            seeds,
        });
    }
    ensure!(
        !payloads.is_empty(),
        "recipe produced no installable system/package output"
    );
    for payload in &mut payloads {
        payload.package.dependencies = payload
            .package
            .dependencies
            .iter()
            .filter_map(|p| identities.get(p).or_else(|| held.get(p)).cloned())
            .collect();
    }
    Ok(payloads)
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Plan {
    pub receipt: Receipt,
    pub wanted: BTreeMap<String, Entry>,
    pub removals: Vec<String>,
    /// Configuration files under `etc/` the administrator changed since they
    /// were installed: left in place, no longer removed or overwritten.
    #[serde(default)]
    pub kept: Vec<String>,
    /// Wanted files whose path is a kept configuration file: written beside
    /// it as `NAME.recipe-new`.
    #[serde(default)]
    pub beside: BTreeSet<String>,
    /// Directories the change stops owning; removed when left empty.
    #[serde(default)]
    pub dirs: Vec<String>,
    /// The root's new account files, when the change creates accounts.
    #[serde(default)]
    pub accounts: Option<accounts::Files>,
    /// `NAME.recipe-new` files an earlier update wrote that this change no
    /// longer needs, with the digest they were written with: removed when
    /// still unchanged.
    #[serde(default)]
    pub stale_new: Vec<(String, String)>,
    /// Kept configuration files the incoming version carries exactly as the
    /// package's previous version did: nothing new to put beside them, so
    /// they are neither written nor given a `.recipe-new`.
    #[serde(default)]
    pub untouched: BTreeSet<String>,
}

/// Where a configuration file the administrator changed is kept: a file or
/// link under `etc/` whose installed version differs from what is there (or
/// that is gone). Anything else that changed is refused.
fn edited_config(path: &str, prior: &Entry, current: Option<&Entry>) -> bool {
    path.starts_with("etc/") && prior.kind != "dir" && current.is_none_or(|c| c.kind != "dir")
}

/// Refuse ownership conflicts and administrator edits before publication. An
/// unchanged compatible file may have several owners; removing one retains it.
/// A configuration file under `etc/` the administrator changed is kept (see
/// [`Plan::kept`]); any other changed owned file refuses the change.
pub fn reconcile(
    old: &Receipt,
    payloads: &[Payload],
    remove: Option<&str>,
    observed: &BTreeMap<String, Entry>,
) -> Result<Plan> {
    let names = payloads
        .iter()
        .map(|p| p.package.name.as_str())
        .chain(remove)
        .collect::<BTreeSet<_>>();
    let mut receipt = old.clone();
    let mut wanted = BTreeMap::new();
    let mut removals = BTreeSet::new();
    let mut kept = BTreeSet::new();
    let mut dirs = BTreeSet::new();
    for name in &names {
        if let Some(package) = old.packages.get(*name) {
            for (path, prior) in &package.files {
                let mut current = observed.get(path).cloned();
                // Unspecified SELinux labels are assigned by installed kernel
                // policy; an authored label remains an exact constraint.
                if !prior.xattrs.contains_key("security.selinux")
                    && let Some(current) = &mut current
                {
                    current.xattrs.remove("security.selinux");
                }
                if current.as_ref() != Some(prior) {
                    ensure!(
                        edited_config(path, prior, current.as_ref()),
                        "administrator edit or damaged owned file at {path}; refusing to overwrite/remove it"
                    );
                    if current.is_some() {
                        kept.insert(path.clone());
                    }
                    continue;
                }
                if prior.kind == "dir" {
                    dirs.insert(path.clone());
                } else {
                    removals.insert(path.clone());
                }
            }
        } else {
            ensure!(Some(*name) != remove, "package is not selected");
        }
        receipt.packages.remove(*name);
    }
    let mut beside = BTreeSet::new();
    let mut untouched = BTreeSet::new();
    // A directory several packages of one change carry: a package that only
    // has it as the parent of its files carries it as root:root 0755 (what an
    // unpacked tree makes), while the package it belongs to states its owner
    // and mode. The stated one is the directory's; two stated ones that
    // differ are refused below.
    let plain = |e: &Entry| {
        e.kind == "dir"
            && e.mode & 0o7777 == 0o755
            && e.uid == 0
            && e.gid == 0
            && e.user.as_deref().is_none_or(|user| user == "root")
            && e.group.as_deref().is_none_or(|group| group == "root")
            && e.xattrs.keys().all(|key| key == "security.selinux")
    };
    let mut stated = BTreeMap::<String, Option<Entry>>::new();
    for payload in payloads.iter() {
        for (path, entry) in &payload.package.files {
            if entry.kind != "dir" || plain(entry) {
                continue;
            }
            match stated.get(path) {
                None => {
                    stated.insert(path.clone(), Some(entry.clone()));
                }
                Some(Some(prior)) if prior != entry => {
                    stated.insert(path.clone(), None);
                }
                _ => {}
            }
        }
    }
    for payload in payloads {
        let mut package = payload.package.clone();
        let mut base_dirs = Vec::new();
        for (path, entry) in &mut package.files {
            if plain(entry)
                && let Some(Some(owner)) = stated.get(path)
            {
                *entry = owner.clone();
            }
            if entry.kind == "dir" {
                if let Some(current) = observed.get(path) {
                    if current.kind == "dir" {
                        // A directory the root already had and Recipe does not
                        // own is the base's: it is neither recorded nor changed.
                        if !old.packages.values().any(|p| p.files.contains_key(path)) {
                            base_dirs.push(path.clone());
                            continue;
                        }
                        *entry = current.clone();
                    } else {
                        ensure!(
                            old.packages
                                .values()
                                .any(|p| names.contains(p.name.as_str())
                                    && p.files.contains_key(path)),
                            "directory conflicts with an unowned path: {path}"
                        );
                    }
                }
            } else if let Some(existing) = observed.get(path) {
                let owned = old.packages.values().any(|p| p.files.contains_key(path));
                if !owned && package.name == BASE_PACKAGE {
                    // A file no package owns where the base now carries one:
                    // one the installation made or copied in (a service link,
                    // a keyboard file, the installer's recab). Under etc/ the
                    // machine's version stays and the base's goes beside it;
                    // elsewhere the release's version replaces it.
                    if path.starts_with("etc/") && existing != entry {
                        kept.insert(path.clone());
                        beside.insert(path.clone());
                    }
                } else {
                    ensure!(owned, "unowned administrator/base file conflict at {path}");
                    if kept.contains(path) && existing != entry {
                        let same = old
                            .packages
                            .get(&package.name)
                            .and_then(|p| p.files.get(path))
                            == Some(&*entry);
                        if same {
                            untouched.insert(path.clone());
                        } else {
                            beside.insert(path.clone());
                        }
                    }
                }
            }
            for other in receipt.packages.values() {
                if let Some(prior) = other.files.get(path) {
                    ensure!(
                        prior == entry,
                        "incompatible system package ownership at {path}"
                    );
                }
            }
            if let Some(prior) = wanted.insert(path.clone(), entry.clone()) {
                ensure!(prior == *entry, "incompatible incoming ownership at {path}");
            }
            removals.remove(path);
        }
        for path in base_dirs {
            package.files.remove(&path);
        }
        for path in &payload.removals {
            ensure!(
                old.packages
                    .get(&package.name)
                    .is_some_and(|p| p.files.contains_key(path)),
                "removal is not owned by {}: {path}",
                package.name
            );
        }
        // Accounts an earlier version created stay recorded.
        if let Some(prior) = old.packages.get(&package.name) {
            let mut all = prior.accounts.clone();
            all.extend(std::mem::take(&mut package.accounts));
            package.accounts = all;
        }
        receipt.packages.insert(package.name.clone(), package);
    }
    for package in receipt.packages.values() {
        for dependency in &package.dependencies {
            ensure!(
                receipt.packages.contains_key(dependency),
                "{} still requires {dependency}",
                package.name
            );
        }
        for path in package.files.keys() {
            removals.remove(path);
            dirs.remove(path);
        }
    }
    // A `.recipe-new` an earlier update wrote beside an `etc/` file of these
    // packages goes when this change does not write it again.
    let mut stale_new = Vec::new();
    for name in &names {
        if let Some(package) = old.packages.get(*name) {
            for (path, prior) in &package.files {
                if path.starts_with("etc/")
                    && prior.kind == "file"
                    && !beside.contains(path)
                    && let Some(sha) = &prior.sha256
                {
                    stale_new.push((format!("{path}.recipe-new"), sha.clone()));
                }
            }
        }
    }
    // A kept file that is wanted again belongs to its package as before.
    let kept = kept
        .into_iter()
        .filter(|p| !wanted.contains_key(p) || beside.contains(p) || untouched.contains(p))
        .collect();
    Ok(Plan {
        receipt,
        wanted,
        removals: removals.into_iter().collect(),
        kept,
        beside,
        dirs: dirs.into_iter().collect(),
        accounts: None,
        stale_new,
        untouched,
    })
}

/// The account files of the root at `root` (empty text for a missing file).
pub(crate) fn account_files(root: &Path) -> Result<accounts::Files> {
    let read = |name: &str| -> Result<String> {
        let path = confined(root, name, false)?;
        match fs::read_to_string(&path) {
            Ok(text) => Ok(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    };
    Ok(accounts::Files {
        passwd: read("etc/passwd")?,
        group: read("etc/group")?,
        shadow: read("etc/shadow")?,
        gshadow: read("etc/gshadow")?,
    })
}

const ACCOUNT_FILES: [&str; 4] = ["etc/passwd", "etc/group", "etc/shadow", "etc/gshadow"];

/// Create the accounts the payloads need in the root at `root` and resolve
/// their file owner names to that root's ids. The lines are the payloads'
/// own `usr/lib/sysusers.d/*.conf` files (by file name, as one directory
/// lists them), then the accounts their recipes declared. Returns the root's
/// new account files when accounts are created; each payload's package
/// records the accounts it created.
pub fn resolve_owners(root: &Path, payloads: &mut [Payload]) -> Result<Option<accounts::Files>> {
    let mut files_lines: Vec<(String, usize, String)> = Vec::new();
    for (index, payload) in payloads.iter().enumerate() {
        for (path, entry) in &payload.package.files {
            let Some(name) = path.strip_prefix("usr/lib/sysusers.d/") else {
                continue;
            };
            if name.contains('/') || !name.ends_with(".conf") {
                continue;
            }
            let source = match entry.kind.as_str() {
                "file" => payload.paths.get(path),
                "hardlink" => entry.link.as_ref().and_then(|l| payload.paths.get(l)),
                _ => None,
            };
            let Some(source) = source else { continue };
            let text = fs::read_to_string(payload.root.join(source))
                .with_context(|| format!("reading {path} of {}", payload.package.name))?;
            files_lines.push((name.to_owned(), index, text));
        }
    }
    files_lines.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()).then(a.1.cmp(&b.1)));
    let mut lines: Vec<(usize, String)> = Vec::new();
    // The base's accounts come first: they are its own account files' lines,
    // with the ids its files were made with, which a sysusers.d `-` line
    // must not take first.
    for (index, payload) in payloads.iter().enumerate() {
        if payload.package.name == BASE_PACKAGE {
            lines.extend(payload.accounts.iter().map(|l| (index, l.clone())));
        }
    }
    for (_, index, text) in &files_lines {
        lines.extend(text.split('\n').map(|l| (*index, l.to_owned())));
    }
    for (index, payload) in payloads.iter().enumerate() {
        if payload.package.name != BASE_PACKAGE {
            lines.extend(payload.accounts.iter().map(|l| (index, l.clone())));
        }
    }
    let named = payloads.iter().any(|p| {
        p.package
            .files
            .values()
            .chain(p.seeds.values())
            .any(|e| e.user.is_some() || e.group.is_some())
    });
    if lines.is_empty() && !named {
        return Ok(None);
    }
    let before = account_files(root)?;
    let (after, created) = accounts::create(before.clone(), &lines)?;
    let changed = !created.is_empty();
    if changed {
        for payload in payloads.iter() {
            for name in ACCOUNT_FILES {
                ensure!(
                    !payload.package.files.contains_key(name),
                    "{} brings its own {name}, and the change also creates accounts in it; \
                     a system package cannot carry the root's account files",
                    payload.package.name
                );
            }
        }
    }
    let mut users = accounts::ids(&after.passwd);
    let mut groups = accounts::ids(&after.group);
    users.entry("root".into()).or_insert(0);
    groups.entry("root".into()).or_insert(0);
    for (index, payload) in payloads.iter_mut().enumerate() {
        let package = &mut payload.package;
        package.accounts = created.get(&index).cloned().unwrap_or_default();
        for (path, entry) in package.files.iter_mut().chain(payload.seeds.iter_mut()) {
            if let Some(user) = entry.user.take() {
                entry.uid = *users.get(&user).with_context(|| {
                    format!(
                        "{path} of {} belongs to user {user}, which {} does not have and which \
                         no sysusers.d file of the change creates; the recipe can declare it: \
                         target_finalize(root, #{{ owners: \"apply\", accounts: [\"u {user} -\"] }})",
                        package.name,
                        root.display()
                    )
                })?;
            }
            if let Some(group) = entry.group.take() {
                entry.gid = *groups.get(&group).with_context(|| {
                    format!(
                        "{path} of {} belongs to group {group}, which {} does not have and which \
                         no sysusers.d file of the change creates; the recipe can declare it: \
                         target_finalize(root, #{{ owners: \"apply\", accounts: [\"g {group} -\"] }})",
                        package.name,
                        root.display()
                    )
                })?;
            }
        }
    }
    Ok(changed.then_some(after))
}

/// The whole plan of a change to the root at `root`: accounts and owners,
/// what the root holds now, and the reconciled result.
pub fn plan(
    root: &Path,
    old: &Receipt,
    payloads: &mut [Payload],
    remove: Option<&str>,
) -> Result<Plan> {
    let accounts = resolve_owners(root, payloads)?;
    let observed = observe(root, old, &old.affected(payloads, remove))?;
    let mut plan = reconcile(old, payloads, remove, &observed)?;
    plan.accounts = accounts;
    Ok(plan)
}

/// What planning a new base did besides the plan itself.
pub struct BasePlan {
    pub plan: Plan,
    /// Paths of the old base an interrupted apply had already brought to the
    /// new release (written as the release has them, or removed when the
    /// release drops them): taken as done instead of as damage.
    pub adopted: Vec<String>,
    /// Paths another package of the root owns with other contents: that
    /// package keeps them, and the base's version is not written.
    pub left_to_others: Vec<(String, String)>,
    /// Files of the running kernel's modules kept as
    /// [`PREVIOUS_KERNEL_PACKAGE`].
    pub kept_modules: usize,
}

/// The plan that replaces the root's `base` with `payload` (the new release's
/// base tree, see `crate::release`). Accounts and owners as for any change,
/// then two things only a base has: a file an interrupted apply already wrote
/// as the release has it (or removed as the release drops it) counts as done,
/// so the same command finishes the job; and a file another package owns with
/// other contents (the effects package's caches, a group's file) stays that
/// package's.
///
/// `keep_modules` is the release of the running kernel when the root is the
/// running one: the old base's files under `usr/lib/modules/RELEASE/` the
/// new base does not carry move to [`PREVIOUS_KERNEL_PACKAGE`] and stay. The
/// modules an earlier apply kept go back to the base first, so they are
/// removed unless they are kept again.
pub fn plan_base(
    root: &Path,
    old: &Receipt,
    payload: &mut Payload,
    keep_modules: Option<&str>,
) -> Result<BasePlan> {
    ensure!(
        payload.package.name == BASE_PACKAGE,
        "a base plan needs the base package"
    );
    let mut old = old.clone();
    let mut kept_modules = 0;
    if let Some(previous) = old.packages.remove(PREVIOUS_KERNEL_PACKAGE)
        && let Some(base) = old.packages.get_mut(BASE_PACKAGE)
    {
        for (path, entry) in previous.files {
            base.files.entry(path).or_insert(entry);
        }
    }
    if let Some(release) = keep_modules
        && !release.is_empty()
        && !release.contains('/')
        && let Some(base) = old.packages.get_mut(BASE_PACKAGE)
    {
        let dir = format!("usr/lib/modules/{release}");
        let prefix = format!("{dir}/");
        let keep: BTreeMap<String, Entry> = base
            .files
            .iter()
            .filter(|(path, _)| {
                (**path == dir || path.starts_with(&prefix))
                    && !payload.package.files.contains_key(*path)
            })
            .map(|(p, e)| (p.clone(), e.clone()))
            .collect();
        if !keep.is_empty() {
            kept_modules = keep.len();
            for path in keep.keys() {
                base.files.remove(path);
            }
            let package = Package {
                name: PREVIOUS_KERNEL_PACKAGE.into(),
                version: release.into(),
                recipe: base.recipe.clone(),
                recipe_digest: base.recipe_digest.clone(),
                architecture: base.architecture.clone(),
                target: "release".into(),
                dependencies: Vec::new(),
                files: keep,
                accounts: Vec::new(),
            };
            old.packages.insert(PREVIOUS_KERNEL_PACKAGE.into(), package);
        }
    }
    let accounts = resolve_owners(root, std::slice::from_mut(payload))?;
    let mut left_to_others = Vec::new();
    payload.package.files.retain(|path, entry| {
        if entry.kind == "dir" {
            return true;
        }
        match old
            .packages
            .values()
            .filter(|p| p.name != BASE_PACKAGE)
            .find(|p| p.files.get(path).is_some_and(|e| e != entry))
        {
            Some(other) => {
                left_to_others.push((path.clone(), other.name.clone()));
                false
            }
            None => true,
        }
    });
    // A hardlink whose first name went with it goes too.
    let files = payload.package.files.clone();
    payload.package.files.retain(|_, entry| {
        entry.kind != "hardlink" || entry.link.as_ref().is_some_and(|l| files.contains_key(l))
    });
    let adopted = adopt_written(root, &mut old, &payload.package)?;
    let payloads = std::slice::from_ref(&*payload);
    let observed = observe(root, &old, &old.affected(payloads, None))?;
    let mut plan = reconcile(&old, payloads, None, &observed)?;
    plan.accounts = accounts;
    Ok(BasePlan {
        plan,
        adopted,
        left_to_others,
        kept_modules,
    })
}

/// In `old`'s copy of `incoming`'s package, take every path the root already
/// holds as `incoming` has it as recorded, and forget every path `incoming`
/// drops that is already gone. Returns those paths. Only exact matches count,
/// so nothing an administrator changed is taken; directories are left out.
fn adopt_written(root: &Path, old: &mut Receipt, incoming: &Package) -> Result<Vec<String>> {
    let Some(prior) = old.packages.get(&incoming.name) else {
        return Ok(Vec::new());
    };
    let changed: BTreeSet<String> = incoming
        .files
        .iter()
        .filter(|(path, entry)| entry.kind != "dir" && prior.files.get(*path) != Some(*entry))
        .map(|(path, _)| path.clone())
        .collect();
    let dropped: BTreeSet<String> = prior
        .files
        .iter()
        .filter(|(path, entry)| entry.kind != "dir" && !incoming.files.contains_key(*path))
        .map(|(path, _)| path.clone())
        .collect();
    let mut probe = Receipt::default();
    probe
        .packages
        .insert(incoming.name.clone(), incoming.clone());
    let now = observe(root, &probe, &changed)?;
    let still = observe(root, old, &dropped)?;
    let package = old.packages.get_mut(&incoming.name).unwrap();
    let mut adopted = Vec::new();
    for path in changed {
        let want = &incoming.files[&path];
        let mut have = now.get(&path).cloned();
        if !want.xattrs.contains_key("security.selinux")
            && let Some(have) = &mut have
        {
            have.xattrs.remove("security.selinux");
        }
        if have.as_ref() == Some(want) {
            package.files.insert(path.clone(), want.clone());
            adopted.push(path);
        }
    }
    for path in dropped {
        if !still.contains_key(&path) {
            package.files.remove(&path);
            adopted.push(path);
        }
    }
    Ok(adopted)
}

/// `relative` inside `root`, with every directory above it a real directory in
/// that root (a link there could lead out of the root). With `create`, missing
/// directories above it are made.
fn confined(root: &Path, relative: &str, create: bool) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    let parts: Vec<&str> = relative.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        ensure!(
            !part.is_empty() && *part != "." && *part != "..",
            "invalid system path"
        );
        if i + 1 == parts.len() {
            path.push(part);
            break;
        }
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => anyhow::bail!(
                "{} in the target root is not a plain directory, so {relative} cannot be written there",
                path.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
                fs::create_dir(&path)?;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
            }
            // Nothing exists below a missing directory; the caller sees "not found".
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                path.extend(&parts[i + 1..]);
                break;
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }
    Ok(path)
}

/// The system receipt of the root at `root`; a root without one has no system software.
pub fn read_receipt(root: &Path) -> Result<Receipt> {
    let path = confined(root, RECEIPT, false)?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    Receipt::parse(text.as_deref())
        .with_context(|| format!("the system receipt of {} is not usable", root.display()))
}

/// The value of the extended attribute `name` of `path` (not followed).
pub(crate) fn xattr(path: &Path, name: &str) -> Result<Option<Vec<u8>>> {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let n = std::ffi::CString::new(name)?;
    let mut buffer = vec![0u8; 65536];
    let len = unsafe {
        libc::lgetxattr(
            c.as_ptr(),
            n.as_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
        )
    };
    if len < 0 {
        let error = std::io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ENODATA) | Some(libc::ENOTSUP) => Ok(None),
            _ => Err(error).with_context(|| format!("reading {name} of {}", path.display())),
        };
    }
    buffer.truncate(len as usize);
    Ok(Some(buffer))
}

/// What the root at `root` holds now for each of `paths`. Owners and extended
/// attributes are only set when Recipe runs as root (see `own`); another
/// user's run reports what the receipt recorded for them.
pub fn observe(
    root: &Path,
    old: &Receipt,
    paths: &BTreeSet<String>,
) -> Result<BTreeMap<String, Entry>> {
    use std::os::unix::fs::MetadataExt;
    let root_user = unsafe { libc::geteuid() } == 0;
    let mut found = BTreeMap::new();
    let mut to_hash: Vec<(String, PathBuf)> = Vec::new();
    for name in paths {
        let path = confined(root, name, false)?;
        let md = match fs::symlink_metadata(&path) {
            Ok(md) => md,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let (kind, link, sha256) = if md.is_dir() {
            ("dir", None, None)
        } else if md.file_type().is_symlink() {
            (
                "symlink",
                Some(fs::read_link(&path)?.to_string_lossy().into_owned()),
                None,
            )
        } else if md.is_file() {
            // Hashed below, all at once across the CPUs.
            to_hash.push((name.clone(), path.clone()));
            ("file", None, None)
        } else {
            anyhow::bail!("unsupported inode at {}", path.display())
        };
        let prior = old.packages.values().find_map(|p| p.files.get(name));
        let (uid, gid, xattrs) = if root_user {
            let mut xattrs = BTreeMap::new();
            for key in prior.map(|p| p.xattrs.keys()).into_iter().flatten() {
                if let Some(value) = xattr(&path, key)? {
                    xattrs.insert(key.clone(), hex::encode(value));
                }
            }
            (md.uid(), md.gid(), xattrs)
        } else {
            prior.map_or((0, 0, BTreeMap::new()), |p| {
                (p.uid, p.gid, p.xattrs.clone())
            })
        };
        found.insert(
            name.clone(),
            Entry {
                kind: kind.into(),
                mode: md.mode() & 0o7777,
                uid,
                gid,
                user: None,
                group: None,
                xattrs,
                link,
                sha256,
            },
        );
    }
    let digests = target::hash_all(&to_hash.iter().map(|(_, p)| p.as_path()).collect::<Vec<_>>())?;
    for ((name, _), digest) in to_hash.iter().zip(digests) {
        if let Some(entry) = found.get_mut(name) {
            entry.sha256 = Some(digest);
        }
    }
    // A later path of one file is recorded as a hardlink to the first path.
    for (name, entry) in &mut found {
        let prior = old.packages.values().find_map(|p| p.files.get(name));
        if let Some(prior) = prior
            && prior.kind == "hardlink"
            && entry.kind == "file"
            && entry.sha256 == prior.sha256
        {
            entry.kind = "hardlink".into();
            entry.link = prior.link.clone();
        }
    }
    Ok(found)
}

fn own(path: &Path, entry: &Entry) -> Result<()> {
    // Ownership and extended attributes are set when Recipe runs as root, which
    // a system change does. Another user's run (a check, not a system) skips them.
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    ensure!(
        unsafe { libc::lchown(c.as_ptr(), entry.uid, entry.gid) } == 0,
        "cannot set the owner of {}: {}",
        path.display(),
        std::io::Error::last_os_error()
    );
    for (name, value) in &entry.xattrs {
        let value = hex::decode(value)?;
        let name = std::ffi::CString::new(name.as_bytes())?;
        ensure!(
            unsafe {
                libc::lsetxattr(
                    c.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            } == 0,
            "cannot set an attribute of {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

/// Replace the root's account file `name` with `text`, keeping the mode and
/// owner the file has (`mode` for a new one).
fn write_account_file(root: &Path, name: &str, text: &str, mode: u32) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let dest = confined(root, name, true)?;
    let (mode, owner) = match fs::symlink_metadata(&dest) {
        Ok(md) => {
            ensure!(
                md.is_file(),
                "{} is not a regular file, so accounts cannot be added to it",
                dest.display()
            );
            (md.mode() & 0o7777, Some((md.uid(), md.gid())))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (mode, None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dest.display())),
    };
    let temp = dest.with_file_name(format!(
        ".recipe-write-{}",
        dest.file_name().unwrap().to_string_lossy()
    ));
    let _ = fs::remove_file(&temp);
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    out.write_all(text.as_bytes())?;
    drop(out);
    fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
    if let Some((uid, gid)) = owner
        && unsafe { libc::geteuid() } == 0
    {
        std::os::unix::fs::lchown(&temp, Some(uid), Some(gid))?;
    }
    fs::rename(&temp, &dest)?;
    Ok(())
}

/// Check the payload file `source` against `entry` (its digest) and move it
/// to `dest` with the entry's mode and owner. False when it cannot be moved
/// (another filesystem, or another name still uses it); the caller copies it.
fn move_payload(
    source: &Path,
    dest: &Path,
    entry: &Entry,
    name: &str,
    digest: Option<&String>,
) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let md = fs::symlink_metadata(source)?;
    let parent = dest.parent().context("system path has no directory")?;
    if !md.is_file() || md.nlink() != 1 || fs::metadata(parent)?.dev() != md.dev() {
        return Ok(false);
    }
    ensure!(
        digest.is_some() && digest == entry.sha256.as_ref(),
        "{name} changed after the recipe published it"
    );
    fs::set_permissions(source, fs::Permissions::from_mode(entry.mode))?;
    own(source, entry)?;
    match fs::rename(source, dest) {
        Ok(()) => Ok(true),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => Ok(false),
        Err(e) => Err(e).with_context(|| format!("moving {name} into place")),
    }
}

/// Flush the whole filesystem holding `root`: one `syncfs` for every file an
/// apply wrote, instead of one sync per file. (On a filesystem other writers
/// keep busy it also waits for their data; a sync per written file was tried
/// and is slower still on such a disk.)
fn sync_root(root: &Path) -> Result<()> {
    use std::os::fd::AsRawFd;
    let dir = fs::File::open(root)?;
    ensure!(
        unsafe { libc::syncfs(dir.as_raw_fd()) } == 0,
        "cannot flush {}: {}",
        root.display(),
        std::io::Error::last_os_error()
    );
    Ok(())
}

/// Write `plan` into the root at `root`: remove what it removes, create the
/// accounts it creates, write what it wants from the payloads, remove the
/// directories it left empty, then record the new receipt. The caller chose
/// `root`; it is never the booted root of an A/B system.
pub fn apply(root: &Path, payloads: &[Payload], plan: &Plan) -> Result<()> {
    use sha2::{Digest, Sha256};
    for name in &plan.removals {
        let path = confined(root, name, false)?;
        match fs::symlink_metadata(&path) {
            Ok(m) if !m.is_dir() => fs::remove_file(&path)?,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
        }
    }
    if let Some(files) = &plan.accounts {
        // Group files first: a user line names its group.
        write_account_file(root, "etc/group", &files.group, 0o644)?;
        write_account_file(root, "etc/gshadow", &files.gshadow, 0o600)?;
        write_account_file(root, "etc/passwd", &files.passwd, 0o644)?;
        write_account_file(root, "etc/shadow", &files.shadow, 0o600)?;
    }
    let sources: BTreeMap<&str, (&Payload, &str)> = payloads
        .iter()
        .flat_map(|p| {
            p.paths
                .iter()
                .map(move |(to, from)| (to.as_str(), (p, from.as_str())))
        })
        .collect();
    // Every payload file's digest, read across the CPUs before anything is
    // moved; a file is only moved when its digest is the recorded one.
    let files: Vec<(&str, PathBuf)> = plan
        .wanted
        .iter()
        .filter(|(_, e)| e.kind == "file")
        .filter_map(|(name, _)| {
            sources
                .get(name.as_str())
                .map(|(payload, from)| (name.as_str(), payload.root.join(from)))
        })
        .collect();
    let digests: BTreeMap<&str, String> = files
        .iter()
        .map(|(name, _)| *name)
        .zip(target::hash_all(
            &files.iter().map(|(_, p)| p.as_path()).collect::<Vec<_>>(),
        )?)
        .collect();
    let mut links = Vec::new();
    for (name, entry) in &plan.wanted {
        if plan.untouched.contains(name) {
            continue;
        }
        let dest = if plan.beside.contains(name) {
            confined(root, &format!("{name}.recipe-new"), true)?
        } else {
            confined(root, name, true)?
        };
        let existing = fs::symlink_metadata(&dest).ok();
        match entry.kind.as_str() {
            "dir" => {
                if existing.is_none() {
                    fs::create_dir(&dest)?;
                } else if !existing.is_some_and(|m| m.is_dir()) {
                    anyhow::bail!("{} is not a directory in the target root", dest.display());
                }
                fs::set_permissions(&dest, fs::Permissions::from_mode(entry.mode))?;
                own(&dest, entry)?;
            }
            "symlink" => {
                if existing.is_some() {
                    fs::remove_file(&dest)?;
                }
                std::os::unix::fs::symlink(entry.link.as_ref().context("missing symlink")?, &dest)?;
                own(&dest, entry)?;
            }
            "file" => {
                let (payload, from) = sources
                    .get(name.as_str())
                    .context("payload file is missing")?;
                let source = payload.root.join(from);
                ensure!(
                    fs::symlink_metadata(&source)?.is_file(),
                    "payload {} is not a regular file",
                    source.display()
                );
                // The payload is the producer's published output, deleted
                // after the change: on the same filesystem its file is
                // checked and moved into place instead of copied.
                if move_payload(&source, &dest, entry, name, digests.get(name.as_str()))? {
                    continue;
                }
                let temp = dest.with_file_name(format!(
                    ".recipe-write-{}",
                    dest.file_name().unwrap().to_string_lossy()
                ));
                let _ = fs::remove_file(&temp);
                let mut hash = Sha256::new();
                {
                    let mut input = fs::File::open(&source)?;
                    let mut output = fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&temp)?;
                    let mut buffer = vec![0; 1 << 16];
                    loop {
                        let n = std::io::Read::read(&mut input, &mut buffer)?;
                        if n == 0 {
                            break;
                        }
                        hash.update(&buffer[..n]);
                        output.write_all(&buffer[..n])?;
                    }
                }
                ensure!(
                    Some(hex::encode(hash.finalize())) == entry.sha256,
                    "{name} changed after the recipe published it"
                );
                fs::set_permissions(&temp, fs::Permissions::from_mode(entry.mode))?;
                own(&temp, entry)?;
                fs::rename(&temp, &dest)?;
            }
            "hardlink" => links.push((dest, entry)),
            other => anyhow::bail!("unsupported kind {other} for {name}"),
        }
    }
    for (dest, entry) in links {
        let target = confined(
            root,
            entry.link.as_ref().context("missing hardlink")?,
            false,
        )?;
        if fs::symlink_metadata(&dest).is_ok() {
            fs::remove_file(&dest)?;
        }
        fs::hard_link(target, &dest)?;
    }
    for (name, sha) in &plan.stale_new {
        let Ok(path) = confined(root, name, false) else {
            continue;
        };
        if !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            continue;
        }
        let mut hash = Sha256::new();
        std::io::copy(&mut fs::File::open(&path)?, &mut hash)?;
        if hex::encode(hash.finalize()) == *sha {
            fs::remove_file(&path)?;
        }
    }
    // Deepest first, so a parent left empty by its children goes too.
    let mut dirs: Vec<&String> = plan.dirs.iter().collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.matches('/').count()));
    for name in dirs {
        let path = confined(root, name, false)?;
        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
            // Not empty (someone else's files) or already gone: left alone.
            let _ = fs::remove_dir(&path);
        }
    }
    let flushing = std::time::Instant::now();
    sync_root(root)?;
    if std::env::var_os("RECIPE_TIMING").is_some() {
        eprintln!(
            "recipe: timing: apply: flushing what it wrote took {:.1} s",
            flushing.elapsed().as_secs_f64()
        );
    }
    write_receipt(root, &plan.receipt)
}

/// Record `receipt` as the system receipt of the root at `root`, durably.
pub fn write_receipt(root: &Path, receipt: &Receipt) -> Result<()> {
    let path = confined(root, RECEIPT, true)?;
    let temp = path.with_extension("json.new");
    let mut out = fs::File::create(&temp)?;
    out.write_all(&serde_json::to_vec(receipt)?)?;
    out.sync_all()?;
    fs::rename(temp, &path)?;
    fs::File::open(path.parent().context("receipt has no directory")?)?.sync_all()?;
    Ok(())
}

/// Create the payloads' state seeds in `state_root` (whose `var/` is the
/// system's `/var`: the root itself, or on an A/B system the running root,
/// where the shared `/var` is mounted). A seed is made only where nothing
/// exists; an existing path is never changed, and nothing is recorded or ever
/// removed. A seed below a link or a non-directory (`var/run -> ../run`) is
/// left out. Returns the paths made.
pub fn seed(state_root: &Path, payloads: &[Payload]) -> Result<Vec<String>> {
    let mut made = Vec::new();
    // Parents sort before their children.
    let all: BTreeMap<&String, (&Payload, &Entry)> = payloads
        .iter()
        .flat_map(|p| p.seeds.iter().map(move |(path, e)| (path, (p, e))))
        .collect();
    'seeds: for (name, (payload, entry)) in all {
        let parts: Vec<&str> = name.split('/').collect();
        let mut dest = state_root.to_path_buf();
        for (i, part) in parts.iter().enumerate() {
            ensure!(
                !part.is_empty() && *part != "." && *part != "..",
                "invalid seed path {name}"
            );
            dest.push(part);
            if i + 1 == parts.len() {
                break;
            }
            match fs::symlink_metadata(&dest) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => continue 'seeds,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    fs::create_dir(&dest)?;
                    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
                }
                Err(e) => return Err(e).with_context(|| format!("reading {}", dest.display())),
            }
        }
        if fs::symlink_metadata(&dest).is_ok() {
            continue;
        }
        match entry.kind.as_str() {
            "dir" => {
                fs::create_dir(&dest)?;
                fs::set_permissions(&dest, fs::Permissions::from_mode(entry.mode))?;
            }
            "symlink" => {
                std::os::unix::fs::symlink(entry.link.as_ref().context("missing symlink")?, &dest)?;
            }
            "file" => {
                let source = payload
                    .root
                    .join(payload.paths.get(name).context("seed source is missing")?);
                ensure!(
                    fs::symlink_metadata(&source)?.is_file(),
                    "seed {} is not a regular file",
                    source.display()
                );
                fs::copy(&source, &dest)?;
                fs::set_permissions(&dest, fs::Permissions::from_mode(entry.mode))?;
            }
            other => anyhow::bail!("unsupported seed kind {other} for {name}"),
        }
        own(&dest, entry)?;
        made.push(name.clone());
    }
    for name in &made {
        let path = state_root.join(name);
        if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_symlink()) {
            fs::File::open(&path)?.sync_all()?;
        }
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
    }
    Ok(made)
}

/// Kernel process I/O counters, including streams and reaped children. These
/// are not filesystem/SMART totals; the final result serialization follows it.
pub fn process_io() -> BTreeMap<String, u64> {
    std::fs::read_to_string("/proc/self/io")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (k, v) = line.split_once(':')?;
            Some((k.to_owned(), v.trim().parse().ok()?))
        })
        .collect()
}
pub fn io_delta(before: &BTreeMap<String, u64>) -> serde_json::Value {
    serde_json::json!(
        process_io()
            .into_iter()
            .map(|(k, v)| {
                let delta = v.saturating_sub(*before.get(&k).unwrap_or(&0));
                (k, delta)
            })
            .collect::<BTreeMap<_, _>>()
    )
}
