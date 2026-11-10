//! Where recipes come from beyond the folders on this computer.
//!
//! A source is a Git repository of recipes. Recipe holds one in either of two
//! ways, named by a small file per source:
//!
//! - a remote source (`files`, `branch`): only its published index is kept,
//!   fetched over HTTPS by `refresh`; a recipe's own files are downloaded when
//!   it is installed;
//! - a folder (`path`): a checkout Recipe reads as it is.
//!
//! The files live in `/usr/share/recipe/sources` (what the image ships) and in
//! `sources` under the package root (what its owner added; the same name
//! replaces a shipped one). Git is the author's tool: nothing here speaks it.

use crate::catalog::{self, Published};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

/// The sources an image ships.
pub const SHIPPED: &str = "/usr/share/recipe/sources";
/// The name of the first folder of `--recipes-path` or the defaults.
pub const LOCAL: &str = "local";
/// The name of the catalog the image ships as a folder.
pub const IMAGE: &str = "image";

const INDEX_LIMIT: u64 = 32 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Declared {
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
    #[serde(default = "yes", skip_serializing_if = "Clone::clone")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone)]
pub enum Kind {
    /// `files` is an address with `{rev}` and `{path}` in it.
    Remote { files: String, branch: String },
    Folder { path: PathBuf },
}

#[derive(Debug, Clone)]
pub struct Source {
    pub name: String,
    pub kind: Kind,
}

/// A file that could not be used, for the person to repair.
pub struct Problem {
    pub path: PathBuf,
    pub problem: String,
}

/// What is kept of a remote source between refreshes.
pub struct Kept {
    pub index: Option<Published>,
    /// When the index was fetched, in seconds since 1970.
    pub checked: Option<u64>,
    /// Why the last refresh failed, or why the kept index cannot be used.
    pub problem: Option<String>,
}

/// One source as `list --json` shows it.
pub struct Listing {
    pub about: serde_json::Value,
    pub recipes: Vec<serde_json::Value>,
    pub problems: Vec<Problem>,
}

/// A recipe found by its bare name. A remote source has no path for it.
pub struct Found {
    pub source: String,
    pub path: Option<PathBuf>,
}

/// Where a fetched recipe came from; kept beside it and in the receipt.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub recipe: String,
    pub source: String,
    /// The commit of the source the recipe file was fetched at.
    pub commit: String,
    /// Of the recipe file.
    pub sha256: String,
}

const ORIGIN_FILE: &str = ".origin.json";

/// What a recipe of a remote source is fetched for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// To run and keep: the index is fetched first, and the copy is kept
    /// under the package root for as long as the package is installed.
    Install,
    /// To read: the kept index will do, and the copy goes into the cache.
    Look,
}

pub enum Removed {
    Gone,
    /// A shipped source cannot be deleted; this file switches it off.
    SwitchedOff(PathBuf),
}

pub struct Sources {
    store: PathBuf,
    shipped: PathBuf,
    fetched: PathBuf,
    cache: Option<PathBuf>,
}

/// One file of a fetched recipe: where it is downloaded from and what it
/// must be.
struct Wanted {
    address: String,
    sha256: String,
    bytes: u64,
}

fn reserved(name: &str) -> bool {
    name == LOCAL || name == IMAGE
}

fn is_rev(rev: &str) -> bool {
    !rev.is_empty()
        && !rev.contains("..")
        && rev
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte))
}

/// HTTPS, or plain HTTP to this computer for trying a source out.
fn check_address(address: &str) -> Result<()> {
    if address.starts_with("https://") {
        return Ok(());
    }
    let loopback = address.strip_prefix("http://").is_some_and(|rest| {
        let authority = rest.split('/').next().unwrap_or_default();
        let host = match authority.strip_prefix('[') {
            Some(bracketed) => bracketed.split(']').next().unwrap_or_default(),
            None => authority.split(':').next().unwrap_or_default(),
        };
        !authority.contains('@') && matches!(host, "127.0.0.1" | "localhost" | "::1")
    });
    ensure!(
        loopback,
        "a source is fetched over https:// (http:// only from this computer), got {address}"
    );
    Ok(())
}

fn check_pattern(files: &str) -> Result<()> {
    check_address(files)?;
    ensure!(
        files.matches("{rev}").count() == 1
            && files.matches("{path}").count() == 1
            && files.matches(['{', '}']).count() == 4,
        "a source's address holds {{rev}} and {{path}} once each, as in https://example.org/recipes/raw/{{rev}}/{{path}}; got {files}"
    );
    Ok(())
}

/// The address pattern for what `source add` was given: a pattern as it is,
/// or the address of a repository on a forge whose file addresses are known.
fn pattern(address: &str) -> Result<String> {
    if address.contains("{rev}") || address.contains("{path}") {
        check_pattern(address)?;
        return Ok(address.to_owned());
    }
    let repository = address.trim_end_matches('/');
    let repository = repository.strip_suffix(".git").unwrap_or(repository);
    let files = if let Some(rest) = repository.strip_prefix("https://github.com/") {
        ensure!(
            rest.split('/').count() == 2,
            "a GitHub repository is https://github.com/OWNER/NAME, got {address}"
        );
        format!("https://raw.githubusercontent.com/{rest}/{{rev}}/{{path}}")
    } else if repository.starts_with("https://gitlab.com/") {
        format!("{repository}/-/raw/{{rev}}/{{path}}")
    } else {
        bail!(
            "recipe knows where files are on github.com and gitlab.com only\nNext: give the address files are fetched from, with {{rev}} and {{path}} in it, such as https://example.org/OWNER/NAME/raw/{{rev}}/{{path}}"
        );
    };
    check_pattern(&files)?;
    Ok(files)
}

/// At most `limit` bytes of what is at an address, and one more when there
/// is more, so the caller can tell.
fn fetch(address: &str, limit: u64) -> Result<Vec<u8>> {
    check_address(address)?;
    let response = ureq::AgentBuilder::new()
        .https_only(address.starts_with("https://"))
        .redirects(5)
        .timeout_connect(Duration::from_secs(30))
        .timeout(Duration::from_secs(120))
        .build()
        .get(address)
        .set("User-Agent", "recipe")
        .call()
        .map_err(|error| match error {
            ureq::Error::Status(404, _) => anyhow::anyhow!("{address} does not exist"),
            // ureq's message starts with the address.
            error => anyhow::anyhow!("cannot fetch {error}"),
        })?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("cannot read {address}"))?;
    Ok(bytes)
}

fn fetch_index(files: &str, branch: &str) -> Result<(String, Published)> {
    let address = files
        .replace("{rev}", branch)
        .replace("{path}", catalog::INDEX_FILE);
    let bytes = fetch(&address, INDEX_LIMIT)?;
    ensure!(
        bytes.len() as u64 <= INDEX_LIMIT,
        "{address} is larger than {INDEX_LIMIT} bytes"
    );
    let text = String::from_utf8(bytes).with_context(|| format!("{address} is not text"))?;
    let published = catalog::published(&text).with_context(|| {
        format!("{address} is not a usable index; a source publishes one with 'recipe index'")
    })?;
    Ok((text, published))
}

fn read(path: &Path) -> Result<Declared> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("{} does not name a source", path.display()))
}

fn kind(declared: Declared) -> Result<Option<Kind>> {
    if !declared.enabled {
        return Ok(None);
    }
    match (declared.files, declared.path) {
        (Some(files), None) => {
            check_pattern(&files)?;
            let branch = declared.branch.unwrap_or_else(|| "main".to_owned());
            ensure!(is_rev(&branch), "branch {branch:?} cannot go into an address");
            Ok(Some(Kind::Remote { files, branch }))
        }
        (None, Some(path)) => {
            ensure!(declared.branch.is_none(), "a folder has no branch");
            ensure!(path.is_absolute(), "path must be absolute, got {}", path.display());
            Ok(Some(Kind::Folder { path }))
        }
        _ => bail!("a source has either `files` (a remote source) or `path` (a folder)"),
    }
}

impl Sources {
    /// `package_root` is whose software this is: root's store or a person's.
    pub fn new(package_root: &Path) -> Self {
        Self {
            store: package_root.join("sources"),
            fetched: package_root.join("fetched"),
            shipped: PathBuf::from(SHIPPED),
            cache: dirs::cache_dir().map(|cache| cache.join("recipe/sources")),
        }
    }

    /// Every source that is switched on, by name, and the files that could
    /// not be read. One bad file does not hide the others.
    pub fn all(&self) -> (Vec<Source>, Vec<Problem>) {
        let mut files = BTreeMap::new();
        for folder in [&self.shipped, &self.store] {
            for item in std::fs::read_dir(folder).into_iter().flatten().flatten() {
                let path = item.path();
                if path.extension().is_some_and(|extension| extension == "toml")
                    && let Some(name) = path.file_stem().and_then(|stem| stem.to_str())
                {
                    files.insert(name.to_owned(), path);
                }
            }
        }
        let mut sources = Vec::new();
        let mut problems = Vec::new();
        for (name, path) in files {
            let read = || -> Result<Option<Kind>> {
                ensure!(
                    catalog::is_source_name(&name) && !reserved(&name),
                    "{name:?} cannot name a source"
                );
                kind(read(&path)?)
            };
            match read() {
                Ok(Some(kind)) => sources.push(Source { name, kind }),
                Ok(None) => {}
                Err(error) => problems.push(Problem {
                    path,
                    problem: format!("{error:#}"),
                }),
            }
        }
        (sources, problems)
    }

    pub fn get(&self, name: &str) -> Result<Source> {
        let (sources, problems) = self.all();
        if let Some(source) = sources.into_iter().find(|source| source.name == name) {
            return Ok(source);
        }
        if let Some(problem) = problems
            .iter()
            .find(|problem| problem.path.file_stem().is_some_and(|stem| stem == name))
        {
            bail!("{}: {}", problem.path.display(), problem.problem);
        }
        bail!("no source named {name}\nNext: 'recipe source list' shows the sources.");
    }

    /// Where a remote source's index is kept. The address is part of the
    /// place, so two stores naming different sources alike do not mix.
    fn kept_folder(&self, source: &Source) -> Result<PathBuf> {
        let Kind::Remote { files, branch } = &source.kind else {
            bail!("{} is a folder; recipe reads it as it is", source.name);
        };
        let cache = self
            .cache
            .as_ref()
            .context("cannot locate a cache folder to keep the index in")?;
        let key = hex::encode(Sha256::digest(format!("{files}\n{branch}")));
        Ok(cache.join(format!("{}-{}", source.name, &key[..12])))
    }

    fn keep(&self, source: &Source, text: &str) -> Result<()> {
        let folder = self.kept_folder(source)?;
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(&folder)?;
            let fresh = folder.join("index.json.new");
            std::fs::write(&fresh, text)?;
            std::fs::rename(fresh, folder.join(catalog::INDEX_FILE))?;
            match std::fs::remove_file(folder.join("problem")) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            }
        };
        write().with_context(|| format!("cannot keep the index in {}", folder.display()))
    }

    /// Fetches a remote source's index again. A failure is kept beside the
    /// last good index, which stays in use.
    pub fn refresh(&self, source: &Source) -> Result<Published> {
        let Kind::Remote { files, branch } = &source.kind else {
            bail!("{} is a folder; recipe reads it as it is", source.name);
        };
        let fetched = fetch_index(files, branch).and_then(|(text, published)| {
            ensure!(
                published.source.name == source.name,
                "the source at {files} calls itself {}, not {}",
                published.source.name,
                source.name
            );
            Ok((text, published))
        });
        match fetched {
            Ok((text, published)) => {
                self.keep(source, &text)?;
                Ok(published)
            }
            Err(error) => {
                if let Ok(folder) = self.kept_folder(source) {
                    let _ = std::fs::create_dir_all(&folder)
                        .and_then(|()| std::fs::write(folder.join("problem"), format!("{error:#}")));
                }
                Err(error)
            }
        }
    }

    pub fn kept(&self, source: &Source) -> Kept {
        let mut kept = Kept {
            index: None,
            checked: None,
            problem: None,
        };
        let Ok(folder) = self.kept_folder(source) else {
            return kept;
        };
        let file = folder.join(catalog::INDEX_FILE);
        match std::fs::read_to_string(&file) {
            Ok(text) => match catalog::published(&text) {
                Ok(published) => {
                    kept.checked = std::fs::metadata(&file)
                        .and_then(|metadata| metadata.modified())
                        .ok()
                        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                        .map(|since| since.as_secs());
                    kept.index = Some(published);
                }
                Err(error) => kept.problem = Some(format!("{}: {error:#}", file.display())),
            },
            Err(_) => {
                kept.problem = Some(format!(
                    "no index fetched yet; run 'recipe refresh {}'",
                    source.name
                ));
            }
        }
        if let Ok(problem) = std::fs::read_to_string(folder.join("problem")) {
            kept.problem = Some(problem);
        }
        kept
    }

    /// A source and its recipes for a front end. Nothing is fetched: a remote
    /// source is listed from the index kept by the last refresh.
    pub fn listing(&self, source: &Source) -> Listing {
        let mut recipes = Vec::new();
        let mut problems = Vec::new();
        let about = match &source.kind {
            Kind::Remote { files, branch } => {
                let kept = self.kept(source);
                let published = kept.index.as_ref().map(|index| &index.source);
                recipes = kept.index.as_ref().map(|index| index.recipes.clone()).unwrap_or_default();
                serde_json::json!({
                    "name": source.name,
                    "kind": "remote",
                    "title": published.map(|source| &source.title),
                    "about": published.map(|source| &source.about),
                    "address": published.map(|source| &source.address),
                    "keeper": published.map(|source| &source.keeper),
                    "files": files,
                    "branch": branch,
                    "commit": kept.index.as_ref().map(|index| &index.commit),
                    "checked": kept.checked,
                    "problem": kept.problem,
                })
            }
            Kind::Folder { path } => {
                let described = path
                    .join(catalog::SOURCE_FILE)
                    .is_file()
                    .then(|| catalog::source(path));
                let mut names: Vec<_> = std::fs::read_dir(path)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|item| item.path())
                    .filter(|path| path.extension().is_some_and(|extension| extension == "rhai"))
                    .collect();
                names.sort();
                for recipe in names {
                    match catalog::entry(&recipe, Some(path)) {
                        Ok(entry) => recipes.push(entry),
                        Err(error) => problems.push(Problem {
                            path: recipe,
                            problem: format!("{error:#}"),
                        }),
                    }
                }
                let described = match described {
                    Some(Err(error)) => {
                        problems.push(Problem {
                            path: path.join(catalog::SOURCE_FILE),
                            problem: format!("{error:#}"),
                        });
                        None
                    }
                    Some(Ok(described)) => Some(described),
                    None => None,
                };
                serde_json::json!({
                    "name": source.name,
                    "kind": "folder",
                    "title": described.as_ref().map(|source| &source.title),
                    "about": described.as_ref().map(|source| &source.about),
                    "address": described.as_ref().map(|source| &source.address),
                    "keeper": described.as_ref().map(|source| &source.keeper),
                    "path": path,
                    "checked": null,
                    "problem": (!path.is_dir()).then(|| format!("{} is not a folder", path.display())),
                })
            }
        };
        for recipe in &mut recipes {
            recipe["source"] = source.name.clone().into();
        }
        Listing {
            about,
            recipes,
            problems,
        }
    }

    /// A remote source's index: fetched now when `fresh` or when none is kept,
    /// and the kept one when the source cannot be reached.
    fn index(&self, source: &Source, fresh: bool) -> Result<Published> {
        let kept = self.kept(source).index;
        if !fresh && let Some(kept) = kept {
            return Ok(kept);
        }
        match (self.refresh(source), kept) {
            (Ok(index), _) => Ok(index),
            (Err(error), Some(kept)) => {
                crate::core::output::warning(&format!(
                    "{}: {error:#}; using the index fetched before",
                    source.name
                ));
                Ok(kept)
            }
            (Err(error), None) => Err(error),
        }
    }

    /// Downloads a recipe of a remote source and every recipe it needs into
    /// one folder, each file checked against the index, and returns the
    /// recipe's file there. A needed recipe comes from the recipe's own
    /// source, or from a source that one names under `uses`.
    pub fn fetch(&self, source: &Source, recipe: &str, purpose: Purpose) -> Result<PathBuf> {
        let Kind::Remote { files, .. } = &source.kind else {
            bail!("{} is a folder; recipe reads it as it is", source.name);
        };
        let fresh = purpose == Purpose::Install;
        let index = self.index(source, fresh)?;
        let own = index.recipe(recipe).with_context(|| {
            format!(
                "{} has no recipe named {recipe}\nNext: 'recipe list' shows what it holds.",
                source.name
            )
        })?;
        let origin = Origin {
            recipe: recipe.to_owned(),
            source: source.name.clone(),
            commit: index.commit.clone(),
            sha256: own["files"][0]["sha256"].as_str().unwrap_or_default().to_owned(),
        };

        // Every recipe needed, by name: the folder is flat, as a source is.
        let mut wanted = BTreeMap::<String, Wanted>::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut used = BTreeMap::<String, (String, Published)>::new();
        let mut pending = vec![(recipe.to_owned(), source.name.clone())];
        used.insert(source.name.clone(), (files.clone(), index));
        while let Some((name, from)) = pending.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            let (pattern, index) = &used[&from];
            let entry = index.recipe(&name).expect("looked up before it was queued");
            for file in entry["files"].as_array().into_iter().flatten() {
                let path = file["path"].as_str().unwrap_or_default();
                let fresh = Wanted {
                    address: pattern.replace("{rev}", &index.commit).replace("{path}", path),
                    sha256: file["sha256"].as_str().unwrap_or_default().to_owned(),
                    bytes: file["bytes"].as_u64().unwrap_or_default(),
                };
                if let Some(other) = wanted.get(path) {
                    ensure!(
                        other.sha256 == fresh.sha256,
                        "{path} is needed from two sources with different contents"
                    );
                }
                wanted.insert(path.to_owned(), fresh);
            }
            let uses = index.source.uses.clone();
            let needs: Vec<String> = ["deps", "build_deps"]
                .iter()
                .flat_map(|field| entry[*field].as_array().into_iter().flatten())
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect();
            'needed: for needed in needs {
                if seen.contains(&needed) {
                    continue;
                }
                if used[&from].1.recipe(&needed).is_some() {
                    pending.push((needed, from.clone()));
                    continue;
                }
                for other in &uses {
                    if !used.contains_key(other) {
                        let other_source = self.get(other).with_context(|| {
                            format!("{from} uses the source {other}, which is not added here")
                        })?;
                        let Kind::Remote { files, .. } = &other_source.kind else {
                            bail!("{from} uses the source {other}, which is a folder here; recipe fetches needed recipes from remote sources only");
                        };
                        let index = self.index(&other_source, fresh)?;
                        used.insert(other.clone(), (files.clone(), index));
                    }
                    if used[other].1.recipe(&needed).is_some() {
                        pending.push((needed, other.clone()));
                        continue 'needed;
                    }
                }
                bail!(
                    "{name} needs {needed}, which {from} does not have{}",
                    if uses.is_empty() {
                        String::new()
                    } else {
                        format!(", nor the sources it uses ({})", uses.join(", "))
                    }
                );
            }
        }

        // The folder is named by everything in it, so a change to any needed
        // recipe gives a new folder and an installed one is never rewritten.
        let mut contents = Sha256::new();
        for (path, file) in &wanted {
            contents.update(format!("{path} {}\n", file.sha256));
        }
        let key = hex::encode(contents.finalize());
        let base = match purpose {
            Purpose::Install => self.fetched.join(&source.name),
            Purpose::Look => self.kept_folder(source)?.join("look"),
        };
        let folder = base.join(format!("{recipe}-{}", &key[..12]));
        std::fs::create_dir_all(&folder)
            .with_context(|| format!("cannot create {}", folder.display()))?;
        for (path, file) in &wanted {
            let target = folder.join(path);
            if std::fs::read(&target).is_ok_and(|kept| hex::encode(Sha256::digest(&kept)) == file.sha256) {
                continue;
            }
            let bytes = fetch(&file.address, file.bytes)?;
            ensure!(
                bytes.len() as u64 == file.bytes && hex::encode(Sha256::digest(&bytes)) == file.sha256,
                "{} is not the file the index of {} lists (SHA-256 or size differs); nothing was run\nNext: 'recipe refresh {}' and try again; if it stays, the source's index is out of date.",
                file.address,
                source.name,
                source.name
            );
            let fresh = folder.join(format!("{path}.new"));
            std::fs::write(&fresh, bytes)
                .and_then(|()| std::fs::rename(&fresh, &target))
                .with_context(|| format!("cannot write {}", target.display()))?;
        }
        std::fs::write(folder.join(ORIGIN_FILE), serde_json::to_string(&origin)?)
            .with_context(|| format!("cannot write in {}", folder.display()))?;
        Ok(folder.join(format!("{recipe}.rhai")))
    }

    /// Removes the copies kept of a source's recipe, except `keep`: all of
    /// them once the package is removed, the older ones once it is updated.
    /// A copy left by a run that failed goes the same way.
    pub fn forget(&self, source: &str, recipe: &str, keep: Option<&Path>) {
        let prefix = format!("{recipe}-");
        for item in std::fs::read_dir(self.fetched.join(source)).into_iter().flatten().flatten() {
            let folder = item.path();
            let is_copy = item.file_name().to_str().is_some_and(|name| {
                name.strip_prefix(&prefix).is_some_and(|key| {
                    key.len() == 12 && key.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            });
            if is_copy && keep.and_then(Path::parent) != Some(folder.as_path()) {
                let _ = std::fs::remove_dir_all(folder);
            }
        }
    }

    /// Every source that has a recipe of this name. A remote source whose
    /// index was never fetched is fetched once, so a first use finds it.
    pub fn find(&self, recipe: &str) -> Vec<Found> {
        let mut found = Vec::new();
        for source in self.all().0 {
            match &source.kind {
                Kind::Folder { path } => {
                    let file = path.join(format!("{recipe}.rhai"));
                    if file.is_file() {
                        found.push(Found {
                            source: source.name,
                            path: Some(file),
                        });
                    }
                }
                Kind::Remote { .. } => {
                    if self.index(&source, false).is_ok_and(|index| index.recipe(recipe).is_some()) {
                        found.push(Found {
                            source: source.name,
                            path: None,
                        });
                    }
                }
            }
        }
        found
    }

    fn file(&self, name: &str) -> PathBuf {
        self.store.join(format!("{name}.toml"))
    }

    fn write(&self, name: &str, declared: &Declared) -> Result<PathBuf> {
        ensure!(
            catalog::is_source_name(name),
            "a source's name is lower-case letters, digits and '-', got {name:?}"
        );
        ensure!(!reserved(name), "{name} names a folder recipe already reads");
        let file = self.file(name);
        for taken in [&file, &self.shipped.join(format!("{name}.toml"))] {
            ensure!(
                !taken.exists(),
                "a source named {name} exists already ({})",
                taken.display()
            );
        }
        let write = || -> Result<()> {
            std::fs::create_dir_all(&self.store)?;
            std::fs::write(&file, toml::to_string(declared)?)?;
            Ok(())
        };
        write().with_context(|| format!("cannot write {}", file.display()))?;
        Ok(file)
    }

    /// Adds a remote source: fetches its index, which names it, and keeps it.
    pub fn add_remote(&self, address: &str, branch: &str) -> Result<(Source, Published)> {
        let files = pattern(address)?;
        ensure!(is_rev(branch), "branch {branch:?} cannot go into an address");
        let (text, published) = fetch_index(&files, branch)?;
        let source = Source {
            name: published.source.name.clone(),
            kind: Kind::Remote {
                files: files.clone(),
                branch: branch.to_owned(),
            },
        };
        self.write(
            &source.name,
            &Declared {
                files: Some(files),
                branch: Some(branch.to_owned()),
                path: None,
                enabled: true,
            },
        )?;
        self.keep(&source, &text)?;
        Ok((source, published))
    }

    /// Adds a checkout. Its name is the one its `repo.toml` gives; a folder
    /// without one takes `name`, or the folder's own name.
    pub fn add_folder(&self, folder: &Path, name: Option<&str>) -> Result<Source> {
        let folder = std::path::absolute(folder)?;
        ensure!(folder.is_dir(), "{} is not a folder", folder.display());
        let name = if folder.join(catalog::SOURCE_FILE).is_file() {
            let described = catalog::source(&folder)?.name;
            ensure!(
                name.is_none_or(|name| name == described),
                "{} calls itself {described}",
                folder.join(catalog::SOURCE_FILE).display()
            );
            described
        } else {
            name.or_else(|| folder.file_name().and_then(|name| name.to_str()))
                .context("give the source a name with --name")?
                .to_owned()
        };
        self.write(
            &name,
            &Declared {
                files: None,
                branch: None,
                path: Some(folder.clone()),
                enabled: true,
            },
        )?;
        Ok(Source {
            name,
            kind: Kind::Folder { path: folder },
        })
    }

    /// Removes a source this store added, or switches a shipped one off.
    pub fn remove(&self, name: &str) -> Result<Removed> {
        let file = self.file(name);
        let shipped = self.shipped.join(format!("{name}.toml")).is_file();
        let own = file.is_file();
        ensure!(
            own || shipped,
            "no source named {name}\nNext: 'recipe source list' shows the sources."
        );
        if own && shipped && !read(&file)?.enabled {
            bail!(
                "{name} is switched off already\nNext: delete {} to bring it back.",
                file.display()
            );
        }
        if let Ok(source) = self.get(name)
            && let Ok(kept) = self.kept_folder(&source)
        {
            let _ = std::fs::remove_dir_all(kept);
        }
        if own {
            std::fs::remove_file(&file)
                .with_context(|| format!("cannot remove {}", file.display()))?;
        }
        if !shipped {
            return Ok(Removed::Gone);
        }
        let off = Declared {
            files: None,
            branch: None,
            path: None,
            enabled: false,
        };
        std::fs::create_dir_all(&self.store)
            .and_then(|()| std::fs::write(&file, toml::to_string(&off).unwrap_or_default()))
            .with_context(|| format!("cannot write {}", file.display()))?;
        Ok(Removed::SwitchedOff(file))
    }
}

/// How long ago, for a person.
pub fn ago(checked: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(checked, |since| since.as_secs());
    let (count, unit) = match now.saturating_sub(checked) {
        seconds if seconds < 60 => return "just now".to_owned(),
        seconds if seconds < 3600 => (seconds / 60, "minute"),
        seconds if seconds < 86400 => (seconds / 3600, "hour"),
        seconds => (seconds / 86400, "day"),
    };
    format!("{count} {unit}{} ago", if count == 1 { "" } else { "s" })
}

/// Where a recipe file was fetched from, when it is a kept copy of a remote
/// source's recipe.
pub fn origin(recipe: &Path) -> Option<Origin> {
    let text = std::fs::read_to_string(recipe.parent()?.join(ORIGIN_FILE)).ok()?;
    let origin: Origin = serde_json::from_str(&text).ok()?;
    (recipe.file_stem()? == origin.recipe.as_str()).then_some(origin)
}
