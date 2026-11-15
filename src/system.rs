//! Recipe's system software contract: the receipt of what Recipe put in a root,
//! the plan for a change, and writing that plan into a root. The root written
//! is always the other (unlocked) root of an A/B system, or the one root of a
//! mutable system; the booted root of an A/B system is never written.
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
            ensure!(
                entry.user.is_none() && entry.group.is_none(),
                "unresolved installed account metadata"
            );
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
        };
        payloads.push(Payload {
            package,
            root,
            paths,
            removals,
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
}
/// Refuse ownership conflicts and administrator edits before publication. An
/// unchanged compatible file may have several owners; removing one retains it.
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
                ensure!(
                    current.as_ref() == Some(prior),
                    "administrator edit or damaged owned file at {path}; refusing to overwrite/remove it"
                );
                if prior.kind != "dir" {
                    removals.insert(path.clone());
                }
            }
        } else {
            ensure!(Some(*name) != remove, "package is not selected");
        }
        receipt.packages.remove(*name);
    }
    for payload in payloads {
        let mut package = payload.package.clone();
        for (path, entry) in &mut package.files {
            if entry.kind == "dir" {
                if let Some(current) = observed.get(path) {
                    if current.kind == "dir" {
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
            } else if let Some(_existing) = observed.get(path) {
                let owned = old.packages.values().any(|p| p.files.contains_key(path));
                ensure!(owned, "unowned administrator/base file conflict at {path}");
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
        for path in &payload.removals {
            ensure!(
                old.packages
                    .get(&package.name)
                    .is_some_and(|p| p.files.contains_key(path)),
                "removal is not owned by {}: {path}",
                package.name
            );
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
        }
    }
    Ok(Plan {
        receipt,
        wanted,
        removals: removals.into_iter().collect(),
    })
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
                "{} in the other root is not a plain directory, so {relative} cannot be written there",
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

/// What the root at `root` holds now for each of `paths`.
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
        found.insert(
            name.clone(),
            Entry {
                kind: kind.into(),
                mode: md.mode() & 0o7777,
                // Owners are only set when Recipe runs as root (see `own`).
                uid: if root_user { md.uid() } else { 0 },
                gid: if root_user { md.gid() } else { 0 },
                user: None,
                group: None,
                xattrs: BTreeMap::new(),
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

/// Write `plan` into the root at `root`: remove what it removes, write what it
/// wants from the payloads, then record the new receipt. The caller chose
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
        let dest = confined(root, name, true)?;
        let existing = fs::symlink_metadata(&dest).ok();
        match entry.kind.as_str() {
            "dir" => {
                if existing.is_none() {
                    fs::create_dir(&dest)?;
                } else if !existing.is_some_and(|m| m.is_dir()) {
                    anyhow::bail!("{} is not a directory in the other root", dest.display());
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
                    output.sync_all()?;
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
    let receipt = confined(root, RECEIPT, true)?;
    let temp = receipt.with_extension("json.new");
    let mut out = fs::File::create(&temp)?;
    out.write_all(&serde_json::to_vec(&plan.receipt)?)?;
    out.sync_all()?;
    fs::rename(temp, receipt)?;
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
