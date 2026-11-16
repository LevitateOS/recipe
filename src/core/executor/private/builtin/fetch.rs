//! The download step and the unpacking of the sources (docs/recipe-format.md,
//! sections 2.4, 2.6 and 6.1).
//!
//! The download step fetches every `SOURCES` entry for this system's C
//! library and every pin of the section in use into the download store
//! (`crate::domain::DOWNLOADS`), named by its SHA-256 (a `git` source by its
//! commit). A file already there whose SHA-256 matches is not fetched again;
//! the offline cache is used before the network (`download`). The build step
//! then unpacks each archive into its own folder (`crate::domain::SOURCES`),
//! leaving a single top folder out unless `strip: false`, and hands `src` to
//! `build` and `configure`.

use super::{helper, remove_any, text};
use crate::core::output;
use crate::declarations::{Declared, Origin, Source};
use crate::helpers::{acquire, confine};
use crate::pins::{Format, Pin};
use anyhow::{Context, Result, bail, ensure};
use rhai::{Dynamic, Map};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One source the download step fetched.
pub(crate) struct FetchedSource {
    pub name: String,
    pub source: Source,
    /// The download in the store: the file, or the checkout of a `git` source.
    pub path: PathBuf,
}

/// One pinned package the download step fetched.
pub(crate) struct FetchedPin {
    pub pin: Pin,
    /// The package file in the store.
    pub path: PathBuf,
}

/// What the download step left in the store for this system.
#[derive(Default)]
pub(crate) struct Fetched {
    /// The `SOURCES` for this system's C library, by name.
    pub sources: Vec<FetchedSource>,
    /// The pins of the section in use, in pin order.
    pub packages: Vec<FetchedPin>,
    /// The section's package format (`PACKAGE_FORMAT`).
    pub format: Option<Format>,
}

/// The download step (section 2.4, section 2.6 step 1).
pub(crate) fn download(declared: &Declared, host_libc: &str) -> Result<Fetched> {
    let name = &declared.package.name;
    let section = match (&declared.packages, &declared.pins) {
        (None, _) => None,
        (Some(_), None) => bail!("{name} declares PACKAGES but its pins file was not read"),
        (Some(_), Some(pins)) => match pins.section(host_libc) {
            Some(section) if !section.packages.is_empty() => Some(section),
            _ => {
                let has: Vec<&str> = pins
                    .sections
                    .iter()
                    .filter(|section| !section.packages.is_empty())
                    .map(|section| section.libc.as_str())
                    .collect();
                bail!(
                    "{name} has no build for this system: its pins file {} has packages for {} \
                     and this system's C library is {host_libc}. `cargo xtask base groups <distro>` \
                     writes a variant's pins once the recipe names seeds for its C library",
                    pins.path.display(),
                    if has.is_empty() {
                        "no C library".to_owned()
                    } else {
                        has.join(" and ")
                    }
                );
            }
        },
    };
    let selected: Vec<(&String, &Source)> = declared
        .sources
        .iter()
        .filter(|(_, source)| source.libc.is_none_or(|libc| libc.as_str() == host_libc))
        .collect();
    if section.is_none() && selected.is_empty() {
        ensure!(
            declared.sources.is_empty(),
            "{name} has no build for this system: every source is for another C library than \
             this system's ({host_libc}), as ABOUT.libc says"
        );
        return Ok(Fetched::default());
    }

    let store = crate::domain::local(crate::domain::DOWNLOADS);
    confine::mkdir_all(&store)
        .with_context(|| format!("cannot create the download store {}", store.display()))?;
    let total = (selected.len() + section.map_or(0, |section| section.packages.len())) as u64;
    let mut done = 0;
    output::progress_report(done, total, "downloads");
    let mut fetched = Fetched {
        format: section.map(|section| section.format),
        ..Fetched::default()
    };
    for (source_name, source) in selected {
        let path = match &source.origin {
            Origin::Url { url, sha256 } => {
                let file = source
                    .file
                    .as_deref()
                    .context("a url source has a file name")?;
                fetch_file(&store, url, sha256, file)
                    .with_context(|| format!("downloading SOURCES.{source_name} from {url}"))?
            }
            Origin::Git { git, commit, .. } => {
                // The checkout is the download; git_checkout reuses a clean
                // checkout of the same origin at the same commit.
                let dest = store.join(commit);
                acquire::git_checkout(git, text(&dest)?, commit)
                    .map_err(helper)
                    .with_context(|| {
                        format!("checking out SOURCES.{source_name} from {git} at {commit}")
                    })?;
                dest
            }
        };
        fetched.sources.push(FetchedSource {
            name: source_name.clone(),
            source: source.clone(),
            path,
        });
        done += 1;
        output::progress_report(done, total, "downloads");
    }
    for pin in section
        .map(|section| section.packages.as_slice())
        .unwrap_or_default()
    {
        let path = fetch_file(&store, &pin.url, &pin.sha256, pin.file())
            .with_context(|| format!("downloading the pinned package {}", pin.name))?;
        fetched.packages.push(FetchedPin {
            pin: pin.clone(),
            path,
        });
        done += 1;
        output::progress_report(done, total, "downloads");
    }
    Ok(fetched)
}

/// `url` into the store as `<sha256>/<file>`, unless a file with that
/// SHA-256 is there already.
fn fetch_file(store: &Path, url: &str, sha256: &str, file: &str) -> Result<PathBuf> {
    ensure!(
        !file.is_empty()
            && file != "."
            && file != ".."
            && !file.contains('/')
            && !file.chars().any(char::is_control),
        "the download's file name {file:?} cannot be saved"
    );
    let dest = store.join(sha256).join(file);
    if confine::lstat(&dest).is_ok_and(|metadata| metadata.is_file())
        && acquire::verify_sha256(text(&dest)?, sha256).is_ok()
    {
        output::detail(&format!("{file} is downloaded already"));
        return Ok(dest);
    }
    let folder = store.join(sha256);
    confine::mkdir_all(&folder).with_context(|| format!("cannot create {}", folder.display()))?;
    acquire::download(url, text(&dest)?, sha256, false).map_err(helper)?;
    Ok(dest)
}

/// How `src.NAME` holds its source, which decides how `FILES` takes from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Held {
    /// An archive unpacked for this run: `FILES` moves from it.
    Unpacked,
    /// The download itself in the store (a plain file, a `git` checkout):
    /// `FILES` copies from it, because the store keeps the download until
    /// Recipe's clean-up decides (the keep option, a rerun after a failure,
    /// another program naming the same download).
    Stored,
}

/// The sources as `build` and `configure` see them.
#[derive(Default)]
pub(crate) struct Unpacked {
    /// Each source name to `src.NAME` and how it is held.
    pub sources: BTreeMap<String, (PathBuf, Held)>,
}

/// Unpack every archive source into its own folder and say where each
/// source is. On a run restored from a checkpoint the restored work folders
/// hold the unpacked sources already (`crate::core::saved::resumed`).
pub(crate) fn unpack_sources(fetched: &Fetched, resumed: bool) -> Result<Unpacked> {
    let mut unpacked = Unpacked::default();
    for fetched in &fetched.sources {
        let held = match &fetched.source.format {
            Some(format) => {
                let dir = crate::domain::local(crate::domain::SOURCES).join(&fetched.name);
                if !resumed {
                    unpack(
                        &fetched.path,
                        &dir,
                        format,
                        fetched.source.strip,
                        &fetched.name,
                    )
                    .with_context(|| format!("unpacking SOURCES.{}", fetched.name))?;
                }
                (dir, Held::Unpacked)
            }
            None => (fetched.path.clone(), Held::Stored),
        };
        unpacked.sources.insert(fetched.name.clone(), held);
    }
    Ok(unpacked)
}

/// Unpack `archive` (of `format`) into `dir`, replacing what is there; a
/// single top folder is left out when `strip` (decision 4).
fn unpack(archive: &Path, dir: &Path, format: &str, strip: bool, name: &str) -> Result<()> {
    let root = crate::domain::local(crate::domain::SOURCES);
    confine::mkdir_all(&root).with_context(|| format!("cannot create {}", root.display()))?;
    remove_any(dir)?;
    let staging = root.join(format!(".{name}.unpacking"));
    remove_any(&staging)?;
    if format == "rpm" {
        // Natively, without scripts, as target_extract_rpm does; the target
        // metadata it records is for system trees, not for a source.
        crate::helpers::target::extract_rpm(text(archive)?, text(&staging)?).map_err(helper)?;
        crate::target::flush_pending()?;
        remove_any(&staging.join(crate::target::MANIFEST))?;
    } else {
        crate::helpers::build::extract_with_format(text(archive)?, text(&staging)?, format)
            .map_err(helper)?;
    }
    let top = if strip {
        single_folder(&staging)?
    } else {
        None
    };
    let rename = |from: &Path| {
        confine::rename(from, dir).with_context(|| {
            format!(
                "cannot move the unpacked {} to {}",
                from.display(),
                dir.display()
            )
        })
    };
    match top {
        Some(folder) => {
            rename(&folder)?;
            remove_any(&staging)?;
        }
        None => rename(&staging)?,
    }
    Ok(())
}

/// The one entry of `dir` when it is a folder (not a link to one).
fn single_folder(dir: &Path) -> Result<Option<PathBuf>> {
    let mut entries = std::fs::read_dir(dir)
        .with_context(|| format!("cannot list {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    if entries.len() != 1 {
        return Ok(None);
    }
    let only = entries.remove(0).path();
    Ok(confine::lstat(&only)
        .is_ok_and(|metadata| metadata.is_dir())
        .then_some(only))
}

/// `src` for `build(src, out)` and `configure`: each source name to its
/// path, and with `PACKAGES` and `build` the list `src.packages`, in pin
/// order, of `#{ name, path, sha256, bytes, var, licence }` (decision 8).
pub(crate) fn src_map(declared: &Declared, fetched: &Fetched, unpacked: &Unpacked) -> Result<Map> {
    let mut src = Map::new();
    for (name, (path, _)) in &unpacked.sources {
        src.insert(name.into(), text(path)?.into());
    }
    if declared.packages.is_some() && declared.functions.contains(&"build") {
        let mut packages = rhai::Array::new();
        for fetched in &fetched.packages {
            let pin = &fetched.pin;
            let mut entry = Map::new();
            entry.insert("name".into(), pin.name.clone().into());
            entry.insert("path".into(), text(&fetched.path)?.into());
            entry.insert("sha256".into(), pin.sha256.clone().into());
            entry.insert(
                "bytes".into(),
                i64::try_from(pin.bytes)
                    .context("a pinned package is too large")?
                    .into(),
            );
            entry.insert("var".into(), pin.var.into());
            entry.insert(
                "licence".into(),
                pin.licence.clone().map_or(Dynamic::UNIT, Dynamic::from),
            );
            packages.push(entry.into());
        }
        src.insert(crate::declarations::PACKAGES_SOURCE.into(), packages.into());
    }
    Ok(src)
}
