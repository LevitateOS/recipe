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
//!
//! `ABOUT.systems` lists the systems a recipe is made for, by os-release `ID`
//! (`levitateos`, `acornos`): it runs only where the root's `ID` is listed.
//! The same decision, with its own override (`--any-system`), covers it.
//!
//! The system that decides is the root the package is installed into: `/`, or
//! for a system change the root it is written into (`detect_root`). Recipe
//! hands that root to the producer it starts in `RECIPE_HOST_LIBC_ROOT`, so
//! `RUNTIME.host.libc` describes the target, not the machine Recipe runs on.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// The C libraries `ABOUT.libc` may list.
pub const KNOWN: [&str; 2] = ["glibc", "musl"];

/// The flag that installs a package whatever its C library.
pub const OVERRIDE_FLAG: &str = "--any-libc";

/// The flag that installs a package on a system `ABOUT.systems` does not list.
pub const SYSTEM_OVERRIDE_FLAG: &str = "--any-system";

/// Names the root to decide from instead of `/` (its loaders and os-release):
/// a system change sets it for the producer to the root it writes into; it
/// also tries the detection on a prepared tree.
pub const ROOT_ENV: &str = "RECIPE_HOST_LIBC_ROOT";

/// What Recipe found out about this system's C library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// `"glibc"`, `"musl"` or `"unknown"`.
    pub libc: &'static str,
    /// The loaders found, in words, for the user.
    pub found: String,
    /// The root's os-release `ID`, if it has one.
    pub os_id: Option<String>,
    /// Where the os-release `ID` came from, in words, for the user.
    pub os_found: String,
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
    detect_root(&root)
}

/// What decides for the root at `root` (a system change's target).
pub fn detect_root(root: &Path) -> Host {
    detect_in(root, std::env::consts::ARCH)
}

/// The os-release `ID` of the root at `root` (`etc/os-release`, else
/// `usr/lib/os-release`), and where it came from.
fn os_id(root: &Path) -> (Option<String>, String) {
    for name in ["etc/os-release", "usr/lib/os-release"] {
        let path = root.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let id = text.lines().find_map(|line| {
            let value = line.trim().strip_prefix("ID=")?;
            Some(value.trim().trim_matches(['"', '\'']).to_owned())
        });
        return match id {
            Some(id) if !id.is_empty() => {
                let found = format!("ID={id} in {}", path.display());
                (Some(id), found)
            }
            _ => (None, format!("no ID in {}", path.display())),
        };
    }
    (None, format!("no os-release in {}", root.display()))
}

fn detect_in(root: &Path, arch: &str) -> Host {
    let (os_id, os_found) = os_id(root);
    let host = detect_loaders(root, arch);
    Host {
        os_id,
        os_found,
        ..host
    }
}

fn detect_loaders(root: &Path, arch: &str) -> Host {
    let none = || (None, String::new());
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
            os_id: none().0,
            os_found: none().1,
        },
        (true, false) => Host {
            libc: "musl",
            found: format!("the musl loader {}", musl.join(", ")),
            os_id: none().0,
            os_found: none().1,
        },
        (false, false) => Host {
            libc: "unknown",
            found: format!(
                "both the glibc loader {} and the musl loader {}",
                glibc.join(", "),
                musl.join(", ")
            ),
            os_id: none().0,
            os_found: none().1,
        },
        (true, true) => Host {
            libc: "unknown",
            found: format!("neither a glibc nor a musl loader for {arch}"),
            os_id: none().0,
            os_found: none().1,
        },
    }
}

/// What a recipe declares about its C library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// `ABOUT.libc`: the C libraries the build is linked against (empty: any).
    pub libc: Vec<String>,
    /// `ABOUT.libc_carried`: the package brings its own C library and loader.
    pub carried: bool,
    /// `ABOUT.systems`: the os-release `ID`s it is made for (empty: any).
    pub systems: Vec<String>,
}

/// Check `ABOUT.libc`, `ABOUT.libc_carried` and `ABOUT.systems` of a literal
/// ABOUT map. None when the recipe declares neither a C library nor systems.
pub fn declared(about: &serde_json::Value) -> Result<Option<Declared>> {
    let carried = match about.get("libc_carried") {
        None => false,
        Some(serde_json::Value::Bool(carried)) => *carried,
        Some(other) => bail!("ABOUT.libc_carried is true or false, not {other}"),
    };
    let systems = match about.get("systems") {
        None => Vec::new(),
        Some(value) => parse_systems(value)?,
    };
    let libc = match about.get("libc") {
        None if carried => {
            bail!("ABOUT.libc_carried names no C library; add ABOUT.libc, e.g. libc: [\"glibc\"]")
        }
        None => Vec::new(),
        Some(value) => parse_declared(value)?,
    };
    Ok(
        (!libc.is_empty() || !systems.is_empty()).then_some(Declared {
            libc,
            carried,
            systems,
        }),
    )
}

/// Check an `ABOUT.systems` value: a nonempty list of os-release `ID`s.
fn parse_systems(value: &serde_json::Value) -> Result<Vec<String>> {
    let Some(list) = value.as_array().filter(|list| !list.is_empty()) else {
        bail!(
            "ABOUT.systems is a list of the os-release IDs the recipe is made for, \
             for example systems: [\"levitateos\", \"acornos\"]"
        );
    };
    let mut names = Vec::new();
    for item in list {
        match item.as_str() {
            Some(id)
                if !id.is_empty()
                    && id.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)
                    }) =>
            {
                if !names.iter().any(|seen| seen == id) {
                    names.push(id.to_owned());
                }
            }
            _ => bail!(
                "ABOUT.systems names {item}; an os-release ID is lowercase letters, digits, \
                 '.', '_' and '-' (levitateos, acornos)"
            ),
        }
    }
    Ok(names)
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

/// What a mismatch is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `ABOUT.libc` does not list the system's C library.
    Libc,
    /// `ABOUT.systems` does not list the system's os-release `ID`.
    System,
}

impl Kind {
    /// The flag that installs it anyway.
    pub fn flag(self) -> &'static str {
        match self {
            Kind::Libc => OVERRIDE_FLAG,
            Kind::System => SYSTEM_OVERRIDE_FLAG,
        }
    }
}

/// One recipe whose declared C library or systems differ from this system's.
#[derive(Debug)]
pub struct Mismatch {
    pub kind: Kind,
    pub recipe: String,
    /// `ABOUT.systems` (System mismatches).
    pub systems: Vec<String>,
    /// The system's os-release `ID`, and where it came from (System mismatches).
    pub host_os: Option<String>,
    pub host_os_found: String,
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
        if self.kind == Kind::System {
            let systems = self.systems.join(", ");
            return match &self.host_os {
                Some(id) => format!(
                    "{}: it is made for the systems {systems} (os-release ID); this system is \
                     {id} ({}), which it does not list.",
                    self.recipe, self.host_os_found
                ),
                None => format!(
                    "{}: it is made for the systems {systems} (os-release ID); this system has \
                     {}, so it is not one of them.",
                    self.recipe, self.host_os_found
                ),
            };
        }
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
        if self.kind == Kind::System {
            return format!(
                "{}: installing with {SYSTEM_OVERRIDE_FLAG}; it is made for {} and this system is {}.",
                self.recipe,
                self.systems.join(", "),
                self.host_os.as_deref().unwrap_or("unknown")
            );
        }
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
        if self.kind == Kind::System {
            return serde_json::json!({"failure": {
                "kind": "system_mismatch",
                "recipe": self.recipe,
                "systems": self.systems,
                "host_system": self.host_os,
                "override_flag": SYSTEM_OVERRIDE_FLAG,
                "needed_by": self.needed_by,
                "message": self.to_string(),
            }});
        }
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

/// The one rule: a recipe whose declared C libraries do not include the host's
/// does not match (an unknown C library matches everything; the caller notes
/// it), and a recipe whose declared systems do not include the root's
/// os-release `ID` does not match (a root without one matches none).
/// `command(flag)` is the command line that installs it anyway.
fn mismatch(
    recipe: &str,
    declared: &Declared,
    host: &Host,
    command: &dyn Fn(&str) -> String,
    parent: Option<&str>,
) -> Option<Mismatch> {
    let libc = !declared.libc.is_empty()
        && host.libc != "unknown"
        && !declared.libc.iter().any(|libc| libc == host.libc);
    let system = !declared.systems.is_empty()
        && !host
            .os_id
            .as_ref()
            .is_some_and(|id| declared.systems.contains(id));
    let kind = if system {
        Kind::System
    } else if libc {
        Kind::Libc
    } else {
        return None;
    };
    Some(Mismatch {
        kind,
        recipe: recipe.to_owned(),
        systems: declared.systems.clone(),
        host_os: host.os_id.clone(),
        host_os_found: host.os_found.clone(),
        build_libc: declared.libc.clone(),
        host_libc: host.libc.to_owned(),
        carried: declared.carried,
        command: command(kind.flag()),
        needed_by: parent.map(str::to_owned),
    })
}

/// The command line for an install of `requested` with `flag`: `recipe VERB
/// FLAG NAME`, with `prefix` (e.g. `--system --root /mnt`) before the verb.
pub fn command_for(prefix: &str, verb: &str, requested: &str) -> impl Fn(&str) -> String + use<> {
    let prefix = if prefix.is_empty() {
        String::new()
    } else {
        format!(" {prefix}")
    };
    let (verb, requested) = (verb.to_owned(), requested.to_owned());
    move |flag| format!("recipe{prefix} {verb} {flag} {requested}")
}

/// Add `runs_here` (can this system run the recipe) and `not_runnable_reason`
/// (the message the install would give, else null) to a catalog entry, from
/// the same comparison the install makes. A recipe file on this machine is
/// checked with its runtime dependencies; an entry from a published index,
/// whose files are not here, is checked by its own `ABOUT` alone.
pub fn annotate(entry: &mut serde_json::Value, host: &Host) {
    let name = entry["name"].as_str().unwrap_or_default().to_owned();
    let path = entry["path"].as_str().map(PathBuf::from);
    let found = match path.filter(|path| path.is_file()) {
        Some(path) => check(
            &path,
            path.parent(),
            host,
            &command_for("", "install", &name),
        )
        .ok()
        .and_then(|verdict| verdict.mismatches.into_iter().next()),
        None => entry
            .get("about")
            .and_then(|about| declared(about).ok().flatten())
            .and_then(|declared| {
                mismatch(
                    &name,
                    &declared,
                    host,
                    &command_for("", "install", &name),
                    None,
                )
            }),
    };
    entry["runs_here"] = found.is_none().into();
    entry["not_runnable_override"] = found.as_ref().map(|found| found.kind.flag()).into();
    entry["not_runnable_reason"] = found.map(|found| found.to_string()).into();
}

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
    command: &dyn Fn(&str) -> String,
) -> Result<Verdict> {
    let mut verdict = Verdict {
        mismatches: Vec::new(),
        notes: Vec::new(),
    };
    let mut seen = Vec::new();
    visit(
        recipe,
        search_path,
        host,
        command,
        None,
        &mut seen,
        &mut verdict,
    )?;
    Ok(verdict)
}

#[allow(clippy::too_many_arguments)]
fn visit(
    recipe: &Path,
    search_path: Option<&Path>,
    host: &Host,
    command: &dyn Fn(&str) -> String,
    parent: Option<&str>,
    seen: &mut Vec<PathBuf>,
    verdict: &mut Verdict,
) -> Result<()> {
    if seen.len() >= crate::recipes::MAX_DEPTH * 64 || seen.iter().any(|path| path == recipe) {
        return Ok(());
    }
    seen.push(recipe.to_owned());
    // The name is the file name (docs/recipe-format.md, decision 3).
    let name = recipe
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_owned();
    // A recipe this check cannot read is left to the command itself, which
    // reports why in its own order (options first, then the recipe).
    let (Ok(libc), Ok(declaration)) = (
        crate::catalog::declared_libc(recipe, search_path),
        crate::needs::declaration(recipe, search_path),
    ) else {
        return Ok(());
    };
    if let Some(declared) = libc {
        if host.libc == "unknown" && !declared.libc.is_empty() {
            verdict.notes.push(format!(
                "{name}: its build is linked against {}; Recipe found {} on this system, so \
                 this system's C library is unknown and the install goes on without a comparison.",
                declared.libc.join(" or "),
                host.found
            ));
        }
        if let Some(mismatch) = mismatch(&name, &declared, host, command, parent) {
            verdict.mismatches.push(mismatch);
        }
    }
    for dependency in declaration.dependencies {
        if !dependency.build_only {
            visit(
                &dependency.recipe,
                search_path,
                host,
                command,
                Some(name.as_str()),
                seen,
                verdict,
            )?;
        }
    }
    Ok(())
}
