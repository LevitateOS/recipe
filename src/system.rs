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
    /// sysusers.d `u`/`g` lines the recipe declared (`target_finalize`'s
    /// `accounts`).
    pub accounts: Vec<String>,
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
        for (relative, mut entry) in manifest.entries {
            if relative.is_empty() {
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
            .filter_map(|p| identities.get(p).cloned())
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
    for payload in payloads {
        let mut package = payload.package.clone();
        let mut base_dirs = Vec::new();
        for (path, entry) in &mut package.files {
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
                ensure!(owned, "unowned administrator/base file conflict at {path}");
                if kept.contains(path) && existing != entry {
                    beside.insert(path.clone());
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
    // A kept file that is wanted again belongs to its package as before.
    let kept = kept
        .into_iter()
        .filter(|p| !wanted.contains_key(p) || beside.contains(p))
        .collect();
    Ok(Plan {
        receipt,
        wanted,
        removals: removals.into_iter().collect(),
        kept,
        beside,
        dirs: dirs.into_iter().collect(),
        accounts: None,
    })
}

/// The account files of the root at `root` (empty text for a missing file).
fn account_files(root: &Path) -> Result<accounts::Files> {
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
    for (_, index, text) in &files_lines {
        lines.extend(text.split('\n').map(|l| (*index, l.to_owned())));
    }
    for (index, payload) in payloads.iter().enumerate() {
        lines.extend(payload.accounts.iter().map(|l| (index, l.clone())));
    }
    let named = payloads.iter().any(|p| {
        p.package
            .files
            .values()
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
        for (path, entry) in &mut package.files {
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
fn xattr(path: &Path, name: &str) -> Result<Option<Vec<u8>>> {
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
            use sha2::{Digest, Sha256};
            let mut hash = Sha256::new();
            std::io::copy(&mut fs::File::open(&path)?, &mut hash)?;
            ("file", None, Some(hex::encode(hash.finalize())))
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

/// Flush the whole filesystem holding `root`: one `syncfs` for every file an
/// apply wrote, instead of one sync per file.
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
    let mut links = Vec::new();
    for (name, entry) in &plan.wanted {
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
    sync_root(root)?;
    let receipt = confined(root, RECEIPT, true)?;
    let temp = receipt.with_extension("json.new");
    let mut out = fs::File::create(&temp)?;
    out.write_all(&serde_json::to_vec(&plan.receipt)?)?;
    out.sync_all()?;
    fs::rename(temp, &receipt)?;
    fs::File::open(receipt.parent().context("receipt has no directory")?)?.sync_all()?;
    Ok(())
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
