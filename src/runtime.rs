//! Installed Ginux defaults, shared by the engine and OS runtime packaging:
//! where the `ginux` CLI, its root-owned helper tools and the runtime image
//! layouts are, which state root Recipe's instances live in, and which
//! environment variables select others (docs/ginux-cutover-design.md,
//! section 4). Explicit environment selections take precedence.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The `ginux` CLI this install ships (under the prefix, else here).
pub const GINUX_CLI: &str = "/usr/libexec/recipe/ginux";
/// The privileged helper the CLI runs through `sudo` (the CLI's
/// `DEFAULT_HELPER`). It, the image tool and the runtime must be root-owned
/// files that no other user can write: they run as root.
pub const GINUX_HELPER: &str = "/usr/libexec/ginux/ginux-helper";
/// Recipe's guard, the only program the sudoers rule names
/// (`recipe-ginux-guard`). It allows Recipe's instance verbs on instances
/// named `recipe-<uid>-*`, sanitises the import config, clears the
/// environment and then runs [`GINUX_HELPER`]. Like the helper it must be a
/// root-owned file that no other user can write.
pub const GUARD_HELPER: &str = "/usr/libexec/recipe/recipe-ginux-guard";
/// The runtime images Recipe's guard lets a caller other than root build an
/// instance from (the guard's `IMAGE_ALLOWLIST`): a root-owned 0644 file,
/// one `REGISTRY/REPO@sha256:<64 hex>` per line, `#` comments and blank lines
/// ignored. The helper and the image tool apply an image as root, so only
/// images root installed are listed: `recipe runtime stage` writes it with
/// the references it ships, `recipe runtime fetch` (as root) adds the one it
/// imports, Recipe's installer writes it with the installed references.
pub const IMAGE_ALLOWLIST: &str = "/usr/libexec/recipe/runtime-images";
/// The guard reads at most this much of [`IMAGE_ALLOWLIST`].
const MAX_ALLOWLIST_BYTES: usize = 64 * 1024;
/// The image tool the helper runs (`ginux-oci`).
pub const GINUX_IMAGE_TOOL: &str = "/usr/libexec/ginux/ginux-image";
/// The runtime the helper runs (`ginux-init --bin ginux`).
pub const GINUX_RUNTIME: &str = "/usr/libexec/ginux/ginux-runtime";
/// The allocator library the helper copies into every root when present.
pub const GINUX_ALLOCATOR: &str = "/usr/lib64/ginux/libginux_allocator.so";
/// Recipe's shared Ginux state root: one image cache per host; instance
/// names carry the caller's uid.
pub const STATE_ROOT: &str = "/var/lib/recipe/ginux";
/// Recipe's Ginux run root.
pub const RUN_ROOT: &str = "/run/recipe/ginux";
/// The sudoers rule that lets the administrators' group run Recipe's guard
/// on Recipe's roots (`recipe runtime stage --sudoers GROUP`).
pub const SUDOERS: &str = "/etc/sudoers.d/recipe-ginux";

/// Selects the `ginux` CLI.
pub const GINUX_ENV: &str = "RECIPE_GINUX";
/// Selects the Ginux state root.
pub const STATE_ROOT_ENV: &str = "RECIPE_GINUX_STATE_ROOT";
/// Selects the Ginux run root.
pub const RUN_ROOT_ENV: &str = "RECIPE_GINUX_RUN_ROOT";
/// The CLI's own variable naming the helper; Recipe passes it through
/// unchanged (only the OS builder and the e2e fixtures set it).
pub const HELPER_ENV: &str = "GINUX_INSTANCE_HELPER";

pub const IMAGE_LAYOUT: &str = "/usr/share/recipe/runtime/compiler.oci.tar";
pub const ROOT_REFERENCE: &str = "/usr/share/recipe/runtime/root-reference";
pub const MINIMAL_IMAGE_LAYOUT: &str = "/usr/share/recipe/runtime/minimal.oci.tar";
pub const MINIMAL_REFERENCE: &str = "/usr/share/recipe/runtime/minimal-reference";
/// Where a standalone install keeps the sources it ships, under the prefix.
pub const SOURCES: &str = "/usr/share/recipe/sources";
const MAX_CONFIG_PATH: usize = 128;

/// Variables of the fork's launcher that are gone, each with what replaced
/// it. A stale one in the environment stops Recipe ([`refuse_gone_variables`]).
const GONE: [(&str, &str); 5] = [
    ("GINUX_BIN", GINUX_ENV),
    ("GINUX_ROOT", "RECIPE_IMAGE"),
    ("GINUX_ROOTFS", "RECIPE_IMAGE_LAYOUT"),
    ("GINUX_MINIMAL_ROOT", "RECIPE_MINIMAL_IMAGE"),
    ("GINUX_MINIMAL_ROOTFS", "RECIPE_MINIMAL_IMAGE_LAYOUT"),
];

/// Stop when a variable of the fork's launcher is set: it would silently
/// select nothing now.
pub fn refuse_gone_variables() -> Result<()> {
    for (gone, now) in GONE {
        if std::env::var_os(gone).is_some() {
            bail!(crate::failure::Category::Startup.error(format!(
                "{gone} is gone; set {now} instead (or unset {gone})"
            )));
        }
    }
    Ok(())
}

/// The two runtime images a Ginux run's root is made from.
///
/// `Full` is the compiler image (Rocky with GCC, Rust, Git and the ISO
/// tools, about 1.3 GB). `Minimal` is a few megabytes of the same Rocky base:
/// a shell, GNU coreutils, findutils, sed and grep and the libraries they load
/// (`bootstrap/ginux-minimal-image.json`), for a run whose own programs are
/// the tools of a root it is given, such as a root's effects recipe. A recipe
/// asks for it with `NEEDS.runtime: "minimal"`; every other recipe runs in
/// the full image. Each image is a one-layer OCI image named by its manifest
/// digest (`local/recipe-compiler@sha256:<manifest>`), imported into Ginux's
/// cache once per host; each run gets its own instance root made from it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Image {
    #[default]
    Full,
    Minimal,
}

impl Image {
    pub fn is_full(&self) -> bool {
        *self == Image::Full
    }

    /// The variable that selects this image's reference.
    pub fn reference_env(self) -> &'static str {
        match self {
            Image::Full => "RECIPE_IMAGE",
            Image::Minimal => "RECIPE_MINIMAL_IMAGE",
        }
    }

    /// The variable that selects this image's layout: the uncompressed OCI
    /// layout tar (`image.oci.tar`; Ginux refuses a directory), set together
    /// with [`Self::reference_env`].
    pub fn layout_env(self) -> &'static str {
        match self {
            Image::Full => "RECIPE_IMAGE_LAYOUT",
            Image::Minimal => "RECIPE_MINIMAL_IMAGE_LAYOUT",
        }
    }

    /// The repository part of this image's reference.
    pub fn repository(self) -> &'static str {
        match self {
            Image::Full => "local/recipe-compiler",
            Image::Minimal => "local/recipe-minimal",
        }
    }

    /// The file naming the image of this kind this install was made for.
    pub fn reference_file(self) -> PathBuf {
        match self {
            Image::Full => under_prefix("share/recipe/runtime/root-reference", ROOT_REFERENCE),
            Image::Minimal => {
                under_prefix("share/recipe/runtime/minimal-reference", MINIMAL_REFERENCE)
            }
        }
    }

    /// The layout tar of this image this install ships; it may not exist.
    pub fn shipped_layout(self) -> PathBuf {
        match self {
            Image::Full => under_prefix("share/recipe/runtime/compiler.oci.tar", IMAGE_LAYOUT),
            Image::Minimal => {
                under_prefix("share/recipe/runtime/minimal.oci.tar", MINIMAL_IMAGE_LAYOUT)
            }
        }
    }
}

/// The full image's reference: `RECIPE_IMAGE` when set, else the installed
/// `root-reference`. Neither is a `startup` failure with the way forward.
pub fn root_reference() -> Result<String> {
    match selected_reference(Image::Full)? {
        Some(root) => Ok(root),
        None => bail!(crate::failure::Category::Startup.error(format!(
            "no runtime image is named: set RECIPE_IMAGE=local/recipe-compiler@sha256:<manifest digest> \
             (with RECIPE_IMAGE_LAYOUT=<its layout> when it is not imported yet), \
             or install Recipe's runtime ({} names the image)",
            Image::Full.reference_file().display()
        ))),
    }
}

/// The minimal image's reference: `RECIPE_MINIMAL_IMAGE` when set, else the
/// installed `minimal-reference`; None when neither names one.
pub fn minimal_reference() -> Result<Option<String>> {
    selected_reference(Image::Minimal)
}

/// The image a run that asks for `image` runs in, and its reference. A run
/// that asks for the minimal image where none is named runs in the full
/// image, which has every program the minimal one has.
pub fn reference_for(image: Image) -> Result<(Image, String)> {
    if image == Image::Minimal {
        if let Some(reference) = minimal_reference()? {
            return Ok((Image::Minimal, reference));
        }
        crate::diagnostics::diag!(
            "recipe: no minimal runtime image is named (RECIPE_MINIMAL_IMAGE, or {}); this run uses the full image",
            Image::Minimal.reference_file().display()
        );
    }
    Ok((Image::Full, root_reference()?))
}

/// Whether a run that asks for the minimal image gets it: a minimal image is
/// named (`RECIPE_MINIMAL_IMAGE` or the installed minimal-reference).
/// An invalid `RECIPE_MINIMAL_IMAGE` or installed reference is an error, as it
/// is for the run itself.
pub fn minimal_named() -> Result<bool> {
    Ok(minimal_reference()?.is_some())
}

/// The image's reference from its variable, else from its installed file.
/// Mirrors the instance parser's reference rule (no leading `/`, no `..`,
/// charset `[A-Za-z0-9_.:/@-]`), plus the §2 ≤ 128 B bound, and requires
/// `REGISTRY/REPO@sha256:<manifest>` ([`root_digest`]).
fn selected_reference(image: Image) -> Result<Option<String>> {
    refuse_gone_variables()?;
    let name = image.reference_env();
    let root = match std::env::var(name) {
        Ok(root) => root,
        Err(std::env::VarError::NotPresent) => {
            if std::env::var_os(image.layout_env()).is_some() {
                bail!(crate::failure::Category::Startup.error(format!(
                    "{} is set without {name}: set both (the layout and its reference)",
                    image.layout_env()
                )));
            }
            match installed_reference(image)? {
                Some(root) => root,
                None => return Ok(None),
            }
        }
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("input preparation failure: {name} is not valid UTF-8")
        }
    };
    if root.is_empty() || root.len() > MAX_CONFIG_PATH {
        bail!(
            "input preparation failure: instance root {root:?} has {} B, want 1..={MAX_CONFIG_PATH} B (§2)",
            root.len(),
        );
    }
    validate_reference(&root)?;
    root_digest(&root)?;
    Ok(Some(root))
}

/// The program that runs Recipe again. A base written into `/` replaces
/// `/usr/bin/recipe` while this process runs; Linux then reports the running
/// binary as `/usr/bin/recipe (deleted)`, a path that does not exist. The file
/// that replaced it is the release's newer recipe, which is the right program
/// to run next; when even that is gone, `/proc/self/exe` still execs the old
/// image.
pub fn own_program() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if exe.exists() {
            return exe;
        }
        if let Some(name) = exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
            let replaced = PathBuf::from(name);
            if replaced.exists() {
                return replaced;
            }
        }
    }
    PathBuf::from("/proc/self/exe")
}

/// The install prefix of the running executable: `/usr` for `/usr/bin/recipe`,
/// `~/.local` for `~/.local/bin/recipe`.
pub fn prefix() -> Option<PathBuf> {
    let exe = own_program().canonicalize().ok()?;
    Some(exe.parent()?.parent()?.to_path_buf())
}

/// `relative` under the install prefix when it exists there, else `fallback`.
fn under_prefix(relative: &str, fallback: &str) -> PathBuf {
    prefix()
        .map(|prefix| prefix.join(relative))
        .filter(|path| path.exists())
        .unwrap_or_else(|| PathBuf::from(fallback))
}

/// The `ginux` CLI this install ships (before `RECIPE_GINUX` is considered).
pub fn ginux_cli() -> PathBuf {
    under_prefix("libexec/recipe/ginux", GINUX_CLI)
}

/// The helper the CLI runs: `GINUX_INSTANCE_HELPER` when set, else the
/// installed one.
pub fn ginux_helper() -> PathBuf {
    std::env::var_os(HELPER_ENV)
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from(GINUX_HELPER), PathBuf::from)
}

/// The installed guard ([`GUARD_HELPER`]): the one program the sudoers rule
/// names, and the one Recipe's Ginux calls run through `sudo`. Unlike the
/// helper it has no environment override: a user-chosen program behind a
/// passwordless rule would be a root escalation.
pub fn guard_helper() -> PathBuf {
    PathBuf::from(GUARD_HELPER)
}

/// Where Recipe's Ginux lives for this process: the CLI, the state root and
/// the run root (`crate::ginux::Ginux::from_env` builds on it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GinuxSetup {
    pub cli: PathBuf,
    pub state_root: String,
    pub run_root: String,
}

/// Resolve [`GinuxSetup`] from `RECIPE_GINUX`, `RECIPE_GINUX_STATE_ROOT` and
/// `RECIPE_GINUX_RUN_ROOT`, else the installed CLI and Recipe's roots.
///
/// A missing, empty, nonexistent or unexecutable CLI is an explicit
/// `startup` failure, never a silent fallback to host execution. A set
/// variable of the fork's launcher (`GINUX_BIN`, ...) is refused.
pub fn ginux_setup() -> Result<GinuxSetup> {
    refuse_gone_variables()?;
    Ok(GinuxSetup {
        cli: resolve_ginux_cli()?,
        state_root: state_root()?,
        run_root: run_root()?,
    })
}

fn resolve_ginux_cli() -> Result<PathBuf> {
    use crate::failure::Category::Startup;
    use std::os::unix::fs::PermissionsExt;
    let (raw, from) = match std::env::var(GINUX_ENV) {
        Ok(value) => (value, GINUX_ENV.to_owned()),
        Err(std::env::VarError::NotPresent) => (
            ginux_cli().to_string_lossy().into_owned(),
            "the installed CLI".to_owned(),
        ),
        Err(error) => return Err(error).context(format!("{GINUX_ENV} is not valid UTF-8")),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!(Startup.error(format!("{GINUX_ENV} is empty (no ginux CLI to run)")));
    }
    let path = PathBuf::from(trimmed);
    let way_forward = format!(
        "set {GINUX_ENV} to the ginux CLI, or install Recipe's runtime ({})",
        GINUX_CLI
    );
    let meta = std::fs::metadata(&path).with_context(|| {
        Startup.error(format!(
            "the ginux CLI {} ({from}) is missing or inaccessible; {way_forward}",
            path.display()
        ))
    })?;
    if !meta.is_file() {
        bail!(Startup.error(format!(
            "the ginux CLI {} ({from}) is not a file; {way_forward}",
            path.display()
        )));
    }
    if meta.permissions().mode() & 0o111 == 0 {
        bail!(Startup.error(format!(
            "the ginux CLI {} ({from}) is not executable; {way_forward}",
            path.display()
        )));
    }
    Ok(path)
}

/// The helper's bound on a state or run root path.
const MAX_ROOT_BYTES: usize = 256;

/// A Ginux state or run root as the helper accepts it: absolute, at most
/// [`MAX_ROOT_BYTES`], made of `A-Z a-z 0-9 _ . / -`, without empty, `.` or
/// `..` components. Whether every level is a root-owned directory nobody
/// else can write is the helper's check (it creates missing levels).
fn ginux_root(variable: &str, default: &str) -> Result<String> {
    use crate::failure::Category::Startup;
    let value = match std::env::var(variable) {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => return Ok(default.to_owned()),
        Err(_) => bail!(Startup.error(format!("{variable} is not valid UTF-8"))),
    };
    let valid = value.starts_with('/')
        && value.len() > 1
        && value.len() <= MAX_ROOT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./-".contains(&byte))
        && value
            .split('/')
            .skip(1)
            .all(|part| !part.is_empty() && part != "." && part != "..");
    if !valid {
        bail!(Startup.error(format!(
            "{variable}={value:?} is not a usable Ginux root: it must be an absolute path below / \
             of at most {MAX_ROOT_BYTES} bytes made of A-Z a-z 0-9 _ . / -, without empty, `.` \
             or `..` components (and every level must be a root-owned directory nobody else \
             can write; the helper creates missing levels)"
        )));
    }
    Ok(value)
}

/// The Ginux state root (`RECIPE_GINUX_STATE_ROOT`, else [`STATE_ROOT`]).
pub fn state_root() -> Result<String> {
    ginux_root(STATE_ROOT_ENV, STATE_ROOT)
}

/// The Ginux run root (`RECIPE_GINUX_RUN_ROOT`, else [`RUN_ROOT`]).
pub fn run_root() -> Result<String> {
    ginux_root(RUN_ROOT_ENV, RUN_ROOT)
}

/// Whether the image whose manifest digest is `manifest` (hex) is imported
/// into the Ginux cache of `state_root` (the cache is readable by anyone).
pub fn imported(state_root: &str, manifest: &str) -> bool {
    Path::new(state_root)
        .join("cache/oci/blobs/sha256")
        .join(manifest)
        .is_file()
}

/// The references listed in the allowlist at `path`, read under the guard's
/// trust rules: every parent directory a real directory owned by root and not
/// writable by group or others, the file itself opened without following a
/// link, regular, owned by root, not writable by group or others, at most
/// 64 KiB of UTF-8. The error says why it is not trusted (or is missing).
pub fn allowed_images(path: &Path) -> Result<Vec<String>> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    if !path.is_absolute() {
        bail!("{} is not an absolute path", path.display());
    }
    for directory in path.ancestors().skip(1) {
        let meta = std::fs::symlink_metadata(directory)
            .with_context(|| format!("cannot inspect {}", directory.display()))?;
        if !meta.file_type().is_dir() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            bail!(
                "{} is not a root-owned directory closed to group and others",
                directory.display()
            );
        }
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| {
            format!(
                "cannot open {} (missing, or a symbolic link)",
                path.display()
            )
        })?;
    let meta = file.metadata()?;
    if !meta.file_type().is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
        bail!(
            "{} is not a regular file owned by root and closed to group and others",
            path.display()
        );
    }
    let mut text = String::new();
    file.take(MAX_ALLOWLIST_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .with_context(|| format!("cannot read {}", path.display()))?;
    if text.len() > MAX_ALLOWLIST_BYTES {
        bail!(
            "{} is larger than {MAX_ALLOWLIST_BYTES} bytes",
            path.display()
        );
    }
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

/// Replace the allowlist at `path` with `references` (each checked, kept in
/// order, repeats dropped), mode 0644: written to a fresh dot-name in the
/// same directory (never through an existing path) and renamed over `path`.
/// A symbolic link or other non-file at `path` is refused. Run as root it is
/// root-owned, as the guard requires.
pub fn write_image_allowlist(path: &Path, references: &[String]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mut lines: Vec<&str> = Vec::new();
    for reference in references {
        if reference.len() > MAX_CONFIG_PATH {
            bail!("{reference:?} is longer than {MAX_CONFIG_PATH} bytes");
        }
        validate_reference(reference)?;
        root_digest(reference)?;
        if !lines.contains(&reference.as_str()) {
            lines.push(reference);
        }
    }
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => bail!(
            "{} is a symbolic link or not a regular file; not writing through it (move it away)",
            path.display()
        ),
        _ => {}
    }
    let directory = path
        .parent()
        .context("the allowlist path has no directory")?;
    let name = path
        .file_name()
        .context("the allowlist path has no file name")?
        .to_string_lossy();
    let temporary = directory.join(format!(".{name}.{}.new", std::process::id()));
    // A leftover of a killed writer (a link is removed, not followed).
    match std::fs::remove_file(&temporary) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(error).with_context(|| format!("cannot remove {}", temporary.display()));
        }
        _ => {}
    }
    let mut text = String::from(
        "# Runtime images Recipe's guard lets callers other than root build instances from.\n\
         # Written by `recipe runtime stage`, `recipe runtime fetch` (as root) or Recipe's installer;\n\
         # one REGISTRY/REPO@sha256:<64 hex> per line. Root-owned, mode 0644.\n",
    );
    for line in &lines {
        text.push_str(line);
        text.push('\n');
    }
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("cannot write {}", path.display()));
    }
    if let Ok(directory) = std::fs::File::open(directory) {
        let _ = directory.sync_all();
    }
    Ok(())
}

/// Add `reference` to the installed allowlist [`IMAGE_ALLOWLIST`], keeping
/// its other lines (only when it is trusted: an untrusted file's lines are
/// dropped, with a warning). For root only (`recipe runtime fetch`).
pub fn allow_image(reference: &str) -> Result<()> {
    let path = Path::new(IMAGE_ALLOWLIST);
    let mut references = match allowed_images(path) {
        Ok(references) => references,
        Err(_) if std::fs::symlink_metadata(path).is_err() => Vec::new(),
        Err(error) => {
            crate::diagnostics::diag!(
                "recipe: warning: {} is not trusted ({error:#}); it is written anew",
                path.display()
            );
            Vec::new()
        }
    };
    if references.iter().any(|listed| listed == reference) {
        return Ok(());
    }
    references.push(reference.to_owned());
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("cannot create {}", directory.display()))?;
    }
    write_image_allowlist(path, &references)
}

/// The sources this install ships.
pub fn shipped_sources() -> PathBuf {
    under_prefix("share/recipe/sources", SOURCES)
}

/// Where `image`'s selected reference came from, for `recipe runtime status`.
pub fn reference_origin(image: Image) -> String {
    if std::env::var_os(image.reference_env()).is_some() {
        image.reference_env().to_owned()
    } else if installed_reference(image).ok().flatten().is_some() {
        format!("installed ({})", image.reference_file().display())
    } else {
        "none".to_owned()
    }
}

/// Mirror of the instance parser's reference rule for `root`/`storage`/`devices`.
pub fn validate_reference(value: &str) -> Result<()> {
    if value.starts_with('/')
        || value.contains("..")
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/' | b'@')
        })
    {
        bail!(
            "input preparation failure: instance reference {value:?} is not a reference \
             (no leading `/`, no `..`, charset `[A-Za-z0-9_.:/@-]`; host paths never enter config)"
        );
    }
    Ok(())
}

/// The manifest digest (64 lowercase hex) of an image reference
/// `REGISTRY/REPO@sha256:<manifest>` (Ginux takes the hex after `@sha256:`;
/// its reference parser needs a registry component, such as `local/`).
pub fn root_digest(reference: &str) -> Result<&str> {
    const SHAPE: &str = "a runtime image reference is REGISTRY/REPO@sha256:<manifest digest>, \
                         such as local/recipe-compiler@sha256:<64 hex>";
    let (image, digest) = reference
        .split_once("@sha256:")
        .with_context(|| format!("{reference:?}: {SHAPE}"))?;
    let named = image.split_once('/').is_some_and(|(registry, repository)| {
        !registry.is_empty()
            && !repository.is_empty()
            && !registry.contains(':')
            && repository.split('/').all(|part| !part.is_empty())
    });
    if !named
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{reference:?}: {SHAPE}");
    }
    Ok(digest)
}

fn installed_reference(image: Image) -> Result<Option<String>> {
    match std::fs::read_to_string(image.reference_file()) {
        Ok(value) => Ok(Some(value.trim().to_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("cannot read installed Recipe runtime reference"),
    }
}

/// The layout of `reference`, the reference of `image` (the image the run
/// asked for and got, [`reference_for`]): that image's variable
/// (`RECIPE_IMAGE_LAYOUT`, `RECIPE_MINIMAL_IMAGE_LAYOUT`), else the layout
/// tar the install ships, else the download cache. Needed only when the
/// image is not imported yet; `ginux import-image` verifies every blob, and
/// the manifest digest it prints must be the reference's.
pub fn layout_path(reference: &str, image: Image) -> Result<PathBuf> {
    let variable = image.layout_env();
    if let Some(path) = std::env::var_os(variable) {
        if path.is_empty() {
            bail!(
                "{variable} is empty; select an OCI image layout (the uncompressed layout tar, image.oci.tar)"
            );
        }
        return Ok(PathBuf::from(path));
    }
    let digest = root_digest(reference)?;
    if installed_reference(image)?.as_deref() == Some(reference) {
        let shipped = image.shipped_layout();
        if shipped.is_file() {
            return Ok(shipped);
        }
    }
    Ok(images_directory()?.join(format!("{digest}.oci.tar")))
}

/// Whether `ensure_layout` reports its download as `fetch` events. The
/// coordinator turns this on with `--machine-events`.
static FETCH_EVENTS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_fetch_events(enabled: bool) {
    FETCH_EVENTS.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// `{short}` of the URL template: the first 16 hex digits of the digest.
fn runtime_url(digest: &str) -> String {
    let template = std::env::var("RECIPE_RUNTIME_URL").ok().filter(|t| !t.is_empty()).unwrap_or_else(|| {
        "https://github.com/LevitateOS/prebuilt/releases/download/recipe-runtime-{short}/image.oci.tar.zst"
            .to_owned()
    });
    template
        .replace("{digest}", digest)
        .replace("{short}", &digest[..16])
}

/// The layout of `reference`, downloaded on first use. A layout selected
/// with its variable, shipped with the install or already downloaded is
/// returned as it is.
pub fn ensure_layout(reference: &str, image: Image) -> Result<PathBuf> {
    let path = layout_path(reference, image)?;
    let variable = image.layout_env();
    if std::env::var_os(variable).is_some() || path.exists() {
        return Ok(path);
    }
    let digest = root_digest(reference)?;
    fetch_image(digest, &path, variable)?;
    Ok(path)
}

/// Removes the partial download unless it was published.
struct Partial<'a> {
    path: &'a Path,
    keep: bool,
}

impl Drop for Partial<'_> {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(self.path);
        }
    }
}

/// Counts the compressed bytes read, drawing and reporting how far they are.
struct Counting<R> {
    inner: R,
    bytes: u64,
    total: Option<u64>,
    source: String,
    bar: indicatif::ProgressBar,
    last_event: std::time::Instant,
}

impl<R: std::io::Read> std::io::Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        crate::cancellation::check_io()?;
        let n = self.inner.read(buf)?;
        self.bytes += n as u64;
        self.bar.set_position(self.bytes);
        if FETCH_EVENTS.load(std::sync::atomic::Ordering::Relaxed)
            && (n == 0 || self.last_event.elapsed() >= std::time::Duration::from_millis(500))
        {
            self.last_event = std::time::Instant::now();
            crate::diagnostics::diag!(
                "{}",
                serde_json::json!({"event": "fetch", "recipe": "runtime-image",
                    "source": self.source, "bytes": self.bytes, "total": self.total})
            );
        }
        Ok(n)
    }
}

/// Download the image `digest` to `destination`; `variable` is the one that
/// would select a local copy instead, for the way forward.
fn fetch_image(digest: &str, destination: &Path, variable: &str) -> Result<()> {
    use crate::failure::Category::Startup;
    use fs2::FileExt;
    use std::io::{Read, Write};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let directory = destination
        .parent()
        .context("the image cache path has no directory")?;
    if let Some(grand) = directory.parent() {
        std::fs::create_dir_all(grand)
            .with_context(|| format!("cannot create {}", grand.display()))?;
    }
    crate::fsx::private_dir(directory, false)?;
    let lock = crate::fsx::open_lease(&directory.join(format!(".fetch-{digest}.lock")), true)
        .context("cannot open the image download lock")?;
    lock.lock_exclusive()
        .context("cannot lock the image download")?;
    // Another process may have finished while this one waited.
    if destination.exists() {
        return Ok(());
    }
    // Partial files of killed downloads: the lock says nobody owns them.
    let stale = format!(".partial-{digest}-");
    for entry in std::fs::read_dir(directory)?.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&stale) {
            let _ = std::fs::remove_file(entry.path());
        }
    }

    let url = runtime_url(digest);
    crate::diagnostics::diag!("recipe: downloading the runtime image {digest:.16} from {url}");
    let way_forward = format!(
        "Next: run 'recipe runtime fetch' later, or select a local layout with {variable}=/path/image.oci.tar (its manifest digest must be {digest}); RECIPE_RUNTIME_URL names another address"
    );
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(30))
        .timeout_read(std::time::Duration::from_secs(60))
        .build();
    let response = match agent.get(&url).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(404, _)) => bail!(Startup.error(format!(
            "no runtime image is published for digest {digest} at {url}\n{way_forward}"
        ))),
        Err(ureq::Error::Status(code, _)) => bail!(Startup.error(format!(
            "downloading {url} failed with HTTP status {code}\n{way_forward}"
        ))),
        Err(error) => bail!(Startup.error(format!(
            "cannot download the runtime image from {url}: {error}\n{way_forward}"
        ))),
    };
    let total = response
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok());
    let bar =
        indicatif::ProgressBar::with_draw_target(total, crate::diagnostics::progress_target());
    bar.set_style(
        indicatif::ProgressStyle::with_template(
            "{bar:40} {bytes}/{total_bytes} {bytes_per_sec} eta {eta}",
        )
        .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar()),
    );
    let counting = Counting {
        inner: response.into_reader(),
        bytes: 0,
        total,
        source: url.clone(),
        bar: bar.clone(),
        last_event: std::time::Instant::now(),
    };
    let mut decoder = zstd::stream::read::Decoder::new(counting)
        .map_err(|error| Startup.error(format!("cannot start decompressing {url}: {error}")))?;

    let partial_path = directory.join(format!(".partial-{digest}-{}", std::process::id()));
    let mut partial = Partial {
        path: &partial_path,
        keep: false,
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&partial_path)
        .with_context(|| format!("cannot create {}", partial_path.display()))?;
    let mut buffer = vec![0u8; 1 << 20];
    let mut written = 0u64;
    loop {
        let n = match decoder.read(&mut buffer) {
            Ok(n) => n,
            Err(_) if crate::cancellation::signal() != 0 => bail!(crate::cancellation::cancelled()),
            Err(error) => bail!(Startup.error(format!(
                "the download from {url} failed or is not a zstd stream: {error}\n{way_forward}"
            ))),
        };
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > crate::image::MAX_ARCHIVE_BYTES {
            bail!(Startup.error(format!(
                "the image from {url} is larger than {} bytes; it is not the runtime image",
                crate::image::MAX_ARCHIVE_BYTES
            )));
        }
        file.write_all(&buffer[..n])
            .with_context(|| format!("cannot write {}", partial_path.display()))?;
    }
    bar.finish_and_clear();
    file.sync_all()?;
    // Only the manifest is checked here; `ginux import-image` verifies
    // every blob against it.
    match crate::image::layout_manifest_digest(&partial_path) {
        Ok(actual) if actual == digest => {}
        Ok(actual) => bail!(Startup.error(format!(
            "the image layout downloaded from {url} has manifest digest {actual}, not {digest}; it was discarded\n{way_forward}"
        ))),
        Err(error) => bail!(Startup.error(format!(
            "the download from {url} is not an OCI image layout: {error:#}; it was discarded\n{way_forward}"
        ))),
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&partial_path, destination)
        .with_context(|| format!("cannot move the image to {}", destination.display()))?;
    partial.keep = true;
    std::fs::File::open(directory)?.sync_all()?;
    crate::diagnostics::diag!("recipe: runtime image stored at {}", destination.display());
    Ok(())
}

/// The runtime images Recipe holds (`recipe storage`, kind `build_image`):
/// the downloaded layouts in the download cache, plus each layout installed
/// with Recipe (full and minimal) when it is the one in use. A layout
/// selected with `RECIPE_IMAGE_LAYOUT` or `RECIPE_MINIMAL_IMAGE_LAYOUT` is
/// the caller's file, not Recipe's, and is not counted. The imported images
/// in Ginux's root-owned cache are counted apart (`system_bytes`): no public
/// Ginux verb deletes an imported image, so nothing here can free them.
pub struct BuildImage {
    /// The download cache (`~/.cache/recipe/images`).
    pub path: PathBuf,
    pub bytes: u64,
    /// Whether `recipe runtime remove` can delete any of it: false when all
    /// there is is the layout installed with the system.
    pub removable: bool,
    /// The blobs imported into Ginux's cache under the state root.
    pub system_bytes: u64,
}

/// The download cache of runtime layouts.
fn images_directory() -> Result<PathBuf> {
    Ok(dirs::cache_dir()
        .context("cannot locate the image cache; set RECIPE_IMAGE_LAYOUT to an image layout")?
        .join("recipe/images"))
}

/// The regular files of a directory (layouts and partial downloads; the
/// 0-byte download locks count as nothing).
fn cached_images(directory: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            (metadata.is_file() && !name.ends_with(".lock")).then(|| (entry.path(), metadata.len()))
        })
        .collect()
}

pub fn build_image() -> Result<BuildImage> {
    let path = images_directory()?;
    let removable_bytes = cached_images(&path)
        .iter()
        .map(|(_, bytes)| bytes)
        .sum::<u64>();
    let full = root_reference().ok();
    let minimal = minimal_reference().ok().flatten();
    let shipped_bytes = [(Image::Full, full), (Image::Minimal, minimal)]
        .into_iter()
        .filter_map(|(image, reference)| {
            let shipped = std::env::var_os(image.layout_env()).is_none()
                && layout_path(&reference?, image)
                    .is_ok_and(|selected| selected == image.shipped_layout());
            shipped.then(|| std::fs::metadata(image.shipped_layout()).map_or(0, |m| m.len()))
        })
        .sum::<u64>();
    let state_root = state_root().unwrap_or_else(|_| STATE_ROOT.into());
    let system_bytes = cached_images(&Path::new(&state_root).join("cache/oci/blobs/sha256"))
        .iter()
        .map(|(_, bytes)| bytes)
        .sum::<u64>();
    Ok(BuildImage {
        path,
        bytes: removable_bytes + shipped_bytes,
        removable: removable_bytes > 0 || shipped_bytes == 0,
        system_bytes,
    })
}

/// `recipe runtime remove`: delete the downloaded runtime layouts. A
/// download in progress (its lock held) is left alone; the layouts installed
/// with Recipe and those selected with `RECIPE_IMAGE_LAYOUT` or
/// `RECIPE_MINIMAL_IMAGE_LAYOUT` are never touched, nor are images imported
/// into Ginux's cache. A layout is downloaded again when an image that is
/// not imported needs it. Returns the bytes freed.
pub fn remove_build_image() -> Result<u64> {
    use fs2::FileExt;
    let directory = images_directory()?;
    let mut freed = 0;
    for (path, bytes) in cached_images(&directory) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        // `<digest>.oci.tar`, or `.partial-<digest>-…` of a download.
        let digest = name
            .strip_suffix(".oci.tar")
            .or_else(|| {
                name.strip_prefix(".partial-")
                    .and_then(|rest| rest.split('-').next())
            })
            .unwrap_or_default();
        let lock = directory.join(format!(".fetch-{digest}.lock"));
        let downloading = crate::fsx::open_lease(&lock, false)
            .is_ok_and(|lock| lock.try_lock_exclusive().is_err());
        if downloading {
            continue;
        }
        std::fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))?;
        freed += bytes;
    }
    Ok(freed)
}
