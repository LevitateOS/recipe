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

pub enum Removed {
    Gone,
    /// A shipped source cannot be deleted; this file switches it off.
    SwitchedOff(PathBuf),
}

pub struct Sources {
    store: PathBuf,
    shipped: PathBuf,
    cache: Option<PathBuf>,
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
    ensure!(bytes.len() as u64 <= limit, "{address} is larger than {limit} bytes");
    Ok(bytes)
}

fn fetch_index(files: &str, branch: &str) -> Result<(String, Published)> {
    let address = files
        .replace("{rev}", branch)
        .replace("{path}", catalog::INDEX_FILE);
    let text = String::from_utf8(fetch(&address, INDEX_LIMIT)?)
        .with_context(|| format!("{address} is not text"))?;
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

    /// Every source that has a recipe of this name.
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
                    if self.kept(&source).index.is_some_and(|index| index.recipe(recipe).is_some()) {
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
