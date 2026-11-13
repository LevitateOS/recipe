//! The C library a recipe's build is linked against, and the C library of the
//! system its package will be installed on.
//!
//! A recipe says what its build works with in `ABOUT.libc` (a list of
//! `"glibc"` and `"musl"`; absent means any). Recipe learns the system's C
//! library from the dynamic loaders present on it and shows it to recipes as
//! `RUNTIME.host.libc`. Before an install acquires anything, the two are
//! compared: a mismatch is reported as facts, with the flag that installs the
//! package anyway. `ABOUT.libc_carried: true` says the package brings its own
//! C library and loader (carry_libraries from a library recipe): the program
//! then finds its loader on any system, and the facts say where it was tried.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// The C libraries `ABOUT.libc` may list.
pub const KNOWN: [&str; 2] = ["glibc", "musl"];

/// The flag that installs a package whatever its C library.
pub const OVERRIDE_FLAG: &str = "--any-libc";

/// Names a folder to look for loaders in instead of `/`, for trying the
/// detection on a prepared tree.
pub const ROOT_ENV: &str = "RECIPE_HOST_LIBC_ROOT";

/// What Recipe found out about this system's C library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// `"glibc"`, `"musl"` or `"unknown"`.
    pub libc: &'static str,
    /// The loaders found, in words, for the user.
    pub found: String,
}

fn loaders(arch: &str) -> (Vec<String>, Vec<String>) {
    let glibc = match arch {
        "x86_64" => vec!["/lib64/ld-linux-x86-64.so.2".to_owned()],
        "aarch64" => vec!["/lib/ld-linux-aarch64.so.1".to_owned()],
        "riscv64" => vec!["/lib/ld-linux-riscv64-lp64d.so.1".to_owned()],
        _ => Vec::new(),
    };
    (glibc, vec![format!("/lib/ld-musl-{arch}.so.1")])
}

/// Decide from the dynamic loaders on the system: the glibc loader of the
/// architecture, or `/lib/ld-musl-<arch>.so.1`. Both present, or neither, is
/// `"unknown"`.
pub fn detect() -> Host {
    let root = std::env::var_os(ROOT_ENV).map_or_else(|| PathBuf::from("/"), PathBuf::from);
    detect_in(&root, std::env::consts::ARCH)
}

fn detect_in(root: &Path, arch: &str) -> Host {
    let (glibc, musl) = loaders(arch);
    let present = |names: &[String]| -> Vec<String> {
        names
            .iter()
            .filter(|name| root.join(name.trim_start_matches('/')).exists())
            .cloned()
            .collect()
    };
    let (glibc, musl) = (present(&glibc), present(&musl));
    match (glibc.is_empty(), musl.is_empty()) {
        (false, true) => Host {
            libc: "glibc",
            found: format!("the glibc loader {}", glibc.join(", ")),
        },
        (true, false) => Host {
            libc: "musl",
            found: format!("the musl loader {}", musl.join(", ")),
        },
        (false, false) => Host {
            libc: "unknown",
            found: format!(
                "both the glibc loader {} and the musl loader {}",
                glibc.join(", "),
                musl.join(", ")
            ),
        },
        (true, true) => Host {
            libc: "unknown",
            found: format!("neither a glibc nor a musl loader for {arch}"),
        },
    }
}

/// What a recipe declares about its C library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// `ABOUT.libc`: the C libraries the build is linked against.
    pub libc: Vec<String>,
    /// `ABOUT.libc_carried`: the package brings its own C library and loader.
    pub carried: bool,
}

/// Check `ABOUT.libc` and `ABOUT.libc_carried` of a literal ABOUT map. None
/// when the recipe declares no C library.
pub fn declared(about: &serde_json::Value) -> Result<Option<Declared>> {
    let carried = match about.get("libc_carried") {
        None => false,
        Some(serde_json::Value::Bool(carried)) => *carried,
        Some(other) => bail!("ABOUT.libc_carried is true or false, not {other}"),
    };
    match about.get("libc") {
        None if carried => bail!("ABOUT.libc_carried names no C library; add ABOUT.libc, e.g. libc: [\"glibc\"]"),
        None => Ok(None),
        Some(value) => Ok(Some(Declared { libc: parse_declared(value)?, carried })),
    }
}

/// Check an `ABOUT.libc` value: a nonempty list of known C library names.
fn parse_declared(value: &serde_json::Value) -> Result<Vec<String>> {
    let Some(list) = value.as_array().filter(|list| !list.is_empty()) else {
        bail!(
            "ABOUT.libc is a list naming the C libraries the build works with, from {KNOWN:?}, \
             for example libc: [\"glibc\"]"
        );
    };
    let mut names = Vec::new();
    for item in list {
        match item.as_str() {
            Some(name) if KNOWN.contains(&name) => {
                if !names.iter().any(|seen| seen == name) {
                    names.push(name.to_owned());
                }
            }
            _ => bail!("ABOUT.libc names {item}; the C libraries it knows are {KNOWN:?}"),
        }
    }
    Ok(names)
}

/// One recipe whose declared C library differs from this system's.
#[derive(Debug)]
pub struct Mismatch {
    pub recipe: String,
    pub build_libc: Vec<String>,
    pub host_libc: String,
    /// The package brings its own C library and loader.
    pub carried: bool,
    /// The command line that installs it anyway.
    pub command: String,
    /// The recipe whose runtime dependency this is, when it is not the one named.
    pub needed_by: Option<String>,
}

impl Mismatch {
    /// The facts, without the way forward.
    fn facts(&self) -> String {
        let build = self.build_libc.join(" or ");
        if self.carried {
            format!(
                "{}: its build is linked against {build}, and its package brings its own {build} \
                 and loader; this system's C library is {}. The package was made and tried on \
                 {build} systems; on {} it has not been tried.",
                self.recipe, self.host_libc, self.host_libc
            )
        } else {
            format!(
                "{}: its build is linked against {build}; this system's C library is {}, so as \
                 packaged its program would find no loader.",
                self.recipe, self.host_libc
            )
        }
    }

    /// The one line shown when the install goes ahead with `--any-libc`.
    pub fn note(&self) -> String {
        format!(
            "{}: installing with {OVERRIDE_FLAG}; its build is linked against {} and this \
             system's C library is {}.",
            self.recipe,
            self.build_libc.join(" or "),
            self.host_libc
        )
    }

    /// The machine-readable record of this failure.
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({"failure": {
            "kind": "libc_mismatch",
            "recipe": self.recipe,
            "build_libc": self.build_libc,
            "host_libc": self.host_libc,
            "carried_libc": self.carried,
            "override_flag": OVERRIDE_FLAG,
            "needed_by": self.needed_by,
            "message": self.to_string(),
        }})
    }
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.facts())?;
        if let Some(parent) = &self.needed_by {
            write!(f, " {} is a dependency of {parent}.", self.recipe)?;
        }
        write!(f, " To install it anyway: {}", self.command)
    }
}

impl std::error::Error for Mismatch {}

/// The result of comparing a recipe, and its runtime dependencies, with the host.
pub struct Verdict {
    /// Every recipe whose build does not match the host, the requested one first.
    pub mismatches: Vec<Mismatch>,
    /// Lines to show when nothing stops the install.
    pub notes: Vec<String>,
}

/// Compare `recipe` and its runtime dependencies (not `build_deps`, which run
/// in the sandbox image) with `host`.
pub fn check(
    recipe: &Path,
    search_path: Option<&Path>,
    host: &Host,
    verb: &str,
    requested: &str,
) -> Result<Verdict> {
    let mut verdict = Verdict {
        mismatches: Vec::new(),
        notes: Vec::new(),
    };
    let mut seen = Vec::new();
    visit(recipe, search_path, host, verb, requested, None, &mut seen, &mut verdict)?;
    Ok(verdict)
}

#[allow(clippy::too_many_arguments)]
fn visit(
    recipe: &Path,
    search_path: Option<&Path>,
    host: &Host,
    verb: &str,
    requested: &str,
    parent: Option<&str>,
    seen: &mut Vec<PathBuf>,
    verdict: &mut Verdict,
) -> Result<()> {
    if seen.len() >= crate::recipes::MAX_DEPTH * 64 || seen.iter().any(|path| path == recipe) {
        return Ok(());
    }
    seen.push(recipe.to_owned());
    let name = crate::declared_metadata(recipe, search_path)
        .ok()
        .and_then(|declared| {
            (!declared.dynamic_fields.contains("name"))
                .then(|| declared.fields.get("name").cloned())
                .flatten()
        })
        .unwrap_or_else(|| {
            recipe
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_owned()
        });
    if let Some(crate::clibrary::Declared { libc: build, carried }) =
        crate::catalog::declared_libc(recipe, search_path)?
    {
        if host.libc == "unknown" {
            verdict.notes.push(format!(
                "{name}: its build is linked against {}; Recipe found {} on this system, so \
                 this system's C library is unknown and the install goes on without a comparison.",
                build.join(" or "),
                host.found
            ));
        } else if !build.iter().any(|libc| libc == host.libc) {
            verdict.mismatches.push(Mismatch {
                recipe: name.clone(),
                build_libc: build,
                host_libc: host.libc.to_owned(),
                carried,
                command: format!("recipe {verb} {OVERRIDE_FLAG} {requested}"),
                needed_by: parent.map(str::to_owned),
            });
        }
    }
    for dependency in crate::needs::declaration(recipe, search_path)?.dependencies {
        if !dependency.build_only {
            visit(
                &dependency.recipe,
                search_path,
                host,
                verb,
                requested,
                Some(name.as_str()),
                seen,
                verdict,
            )?;
        }
    }
    Ok(())
}
