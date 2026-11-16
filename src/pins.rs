//! The pins file `NAME.pins` beside a recipe with `PACKAGES`
//! (docs/recipe-format.md, section 2.7): the distribution packages the group
//! is made of, one line per package, in sections per C library. Written only
//! by the generator (`cargo xtask base groups`,
//! `recipes/generators/desktop-runtime-pins.py`); Recipe only reads it.
//!
//! ```text
//! # comment
//! version 2026.11.16
//!
//! [glibc rpm]
//! NAME SHA256 BYTES VAR URL LICENCE
//! ```

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// The C library a section is for. `Any` is a set pinned whatever the host's
/// C library (`desktop-runtime`, a plain `PACKAGES` list).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SectionLibc {
    Glibc,
    Musl,
    Any,
}

/// The package format of a section; `PACKAGE_FORMAT` is the format of the
/// section in use.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Rpm,
    Apk,
}

impl SectionLibc {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Glibc => "glibc",
            Self::Musl => "musl",
            Self::Any => "any",
        }
    }
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rpm => "rpm",
            Self::Apk => "apk",
        }
    }
}

/// One pinned package.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Pin {
    pub name: String,
    pub sha256: String,
    pub bytes: u64,
    /// Its `var/` is kept as state seeds.
    pub var: bool,
    pub url: String,
    /// `None` when the generator did not know it (`-`).
    pub licence: Option<String>,
}

impl Pin {
    /// The downloaded file's name: the last part of the URL.
    pub fn file(&self) -> &str {
        self.url.rsplit('/').next().unwrap_or_default()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Section {
    pub libc: SectionLibc,
    pub format: Format,
    pub packages: Vec<Pin>,
}

impl Section {
    pub fn bytes(&self) -> u64 {
        self.packages.iter().map(|pin| pin.bytes).sum()
    }
}

/// A read pins file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Pins {
    pub path: PathBuf,
    /// The group's version (`version V`).
    pub version: String,
    pub sections: Vec<Section>,
}

impl Pins {
    /// The pins file of `recipe`: `NAME.pins` beside `NAME.rhai`.
    pub fn path_of(recipe: &Path) -> PathBuf {
        recipe.with_extension("pins")
    }

    /// The section in use on a system whose C library is `libc`
    /// (`HOST_LIBC`): its own, else the `any` section.
    pub fn section(&self, libc: &str) -> Option<&Section> {
        let own = match libc {
            "glibc" => Some(SectionLibc::Glibc),
            "musl" => Some(SectionLibc::Musl),
            _ => None,
        };
        self.sections
            .iter()
            .find(|section| Some(section.libc) == own)
            .or_else(|| {
                self.sections
                    .iter()
                    .find(|section| section.libc == SectionLibc::Any)
            })
    }

    pub fn read(path: &Path) -> Result<Self> {
        ensure!(
            !std::fs::symlink_metadata(path)
                .with_context(|| format!("cannot read {}", path.display()))?
                .file_type()
                .is_symlink(),
            "{} must be a regular file, not a symlink",
            path.display()
        );
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        Self::parse(path, &text)
    }

    fn parse(path: &Path, text: &str) -> Result<Self> {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut version = None;
        let mut sections: Vec<Section> = Vec::new();
        let mut urls = std::collections::BTreeSet::new();
        for (number, line) in text.lines().enumerate() {
            let at = || format!("{name}:{}", number + 1);
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(value) = line.strip_prefix("version ") {
                ensure!(
                    version.is_none() && sections.is_empty(),
                    "{}: one `version` line, before the first section",
                    at()
                );
                let value = value.trim();
                ensure!(
                    !value.is_empty() && !value.contains(char::is_whitespace),
                    "{}: `version V` needs one word",
                    at()
                );
                version = Some(value.to_owned());
                continue;
            }
            if let Some(header) = line.strip_prefix('[') {
                let header = header
                    .strip_suffix(']')
                    .with_context(|| format!("{}: a section header is [LIBC FORMAT]", at()))?;
                let (libc, format) = header
                    .split_once(' ')
                    .with_context(|| format!("{}: a section header is [LIBC FORMAT]", at()))?;
                let libc = match libc {
                    "glibc" => SectionLibc::Glibc,
                    "musl" => SectionLibc::Musl,
                    "any" => SectionLibc::Any,
                    _ => bail!("{}: LIBC is glibc, musl or any, not {libc:?}", at()),
                };
                let format = match format {
                    "rpm" => Format::Rpm,
                    "apk" => Format::Apk,
                    _ => bail!("{}: FORMAT is rpm or apk, not {format:?}", at()),
                };
                ensure!(
                    !matches!(
                        (libc, format),
                        (SectionLibc::Glibc, Format::Apk) | (SectionLibc::Musl, Format::Rpm)
                    ),
                    "{}: glibc packages are rpm and musl packages are apk",
                    at()
                );
                ensure!(
                    sections.iter().all(|section| section.libc != libc),
                    "{}: a second [{} ...] section",
                    at(),
                    header.split(' ').next().unwrap_or_default()
                );
                ensure!(
                    version.is_some(),
                    "{}: the `version` line comes first",
                    at()
                );
                sections.push(Section {
                    libc,
                    format,
                    packages: Vec::new(),
                });
                continue;
            }
            let section = sections
                .last_mut()
                .with_context(|| format!("{}: a package line before any [LIBC FORMAT]", at()))?;
            let fields: Vec<&str> = line.splitn(6, ' ').collect();
            ensure!(
                fields.len() == 6,
                "{}: a package line is NAME SHA256 BYTES VAR URL LICENCE",
                at()
            );
            let [name, sha256, bytes, var, url, licence] = fields[..] else {
                unreachable!("six fields")
            };
            ensure!(
                !name.is_empty(),
                "{}: the package name is empty (single spaces separate the fields)",
                at()
            );
            ensure!(
                sha256.len() == 64
                    && sha256
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "{}: SHA256 is 64 lower-case hex digits",
                at()
            );
            let bytes: u64 = bytes
                .parse()
                .with_context(|| format!("{}: BYTES is a number", at()))?;
            let var = match var {
                "var" => true,
                "-" => false,
                _ => bail!("{}: VAR is `var` or `-`, not {var:?}", at()),
            };
            ensure!(
                url.starts_with("https://") && !url.ends_with('/'),
                "{}: URL is an https:// address of a file",
                at()
            );
            ensure!(
                urls.insert(url.to_owned()),
                "{}: {url} is pinned twice",
                at()
            );
            let licence = licence.trim();
            ensure!(!licence.is_empty(), "{}: LICENCE is empty; write -", at());
            section.packages.push(Pin {
                name: name.to_owned(),
                sha256: sha256.to_owned(),
                bytes,
                var,
                url: url.to_owned(),
                licence: (licence != "-").then(|| licence.to_owned()),
            });
        }
        Ok(Self {
            path: path.to_owned(),
            version: version.with_context(|| format!("{name} has no `version` line"))?,
            sections,
        })
    }
}
