//! Recipe's system selection contract. The recab coordinator supplies confined
//! observations and owns the inactive mount; this module never writes a system.
use crate::target::{self, Entry};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
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
                if !prior.xattrs.contains_key("security.selinux") {
                    if let Some(current) = &mut current {
                        current.xattrs.remove("security.selinux");
                    }
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
