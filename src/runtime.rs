//! Installed Ginux defaults, shared by the engine and OS runtime packaging.
//! Explicit environment selections take precedence; payload verification and
//! mandatory managed execution remain unchanged.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const LAUNCHER: &str = "/usr/libexec/recipe/ginux";
pub const ROOTFS: &str = "/usr/share/recipe/runtime/rootfs.tar";
pub const ROOT_REFERENCE: &str = "/usr/share/recipe/runtime/root-reference";
pub const MINIMAL_ROOTFS: &str = "/usr/share/recipe/runtime/minimal.tar";
pub const MINIMAL_REFERENCE: &str = "/usr/share/recipe/runtime/minimal-reference";
/// Where a standalone install keeps the sources it ships, under the prefix.
pub const SOURCES: &str = "/usr/share/recipe/sources";
const MAX_CONFIG_PATH: usize = 128;

/// Default immutable image reference: the pinned Rocky rootfs content,
/// named by digest rather than by host path (host `.artifacts` paths never
/// enter instance config). Override with `GINUX_ROOT`; the override must pass
/// the same reference validation. The launcher resolves the reference.
const DEFAULT_ROOT_REF: &str = "oci:rocky-10.2.20260525.0-amd64-rootfs@sha256:b93bfd0f6deea2a7a96216655cda3ebbd87a445231548ff491ee30aed20a36ab";

/// The two runtime images a Ginux run's private root is unpacked from.
///
/// `Full` is the compiler image (Rocky with GCC, Rust, Git and the ISO
/// tools, about 1.3 GB). `Minimal` is a few megabytes of the same Rocky base:
/// a shell, GNU coreutils, findutils, sed and grep and the libraries they load
/// (`bootstrap/ginux-minimal-image.json`), for a run whose own programs are
/// the tools of a root it is given, such as a root's effects recipe. A recipe
/// asks for it with `NEEDS.runtime: "minimal"`; every other recipe runs in
/// the full image. Each run still gets its own private copy of the image it
/// runs in, verified against the digest of its reference.
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
    pub fn root_env(self) -> &'static str {
        match self {
            Image::Full => "GINUX_ROOT",
            Image::Minimal => "GINUX_MINIMAL_ROOT",
        }
    }

    /// The variable that selects this image's tar.
    pub fn rootfs_env(self) -> &'static str {
        match self {
            Image::Full => "GINUX_ROOTFS",
            Image::Minimal => "GINUX_MINIMAL_ROOTFS",
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

    /// The tar of this image this install ships; it may not exist.
    pub fn shipped_rootfs(self) -> PathBuf {
        match self {
            Image::Full => under_prefix("share/recipe/runtime/rootfs.tar", ROOTFS),
            Image::Minimal => under_prefix("share/recipe/runtime/minimal.tar", MINIMAL_ROOTFS),
        }
    }
}

/// The full image's reference: `GINUX_ROOT` when set, else the installed
/// `root-reference`, else the built-in pin.
pub fn root_reference() -> Result<String> {
    match selected_reference(Image::Full)? {
        Some(root) => Ok(root),
        None => Ok(DEFAULT_ROOT_REF.to_string()),
    }
}

/// The minimal image's reference: `GINUX_MINIMAL_ROOT` when set, else the
/// installed `minimal-reference`; None when neither names one.
pub fn minimal_reference() -> Result<Option<String>> {
    selected_reference(Image::Minimal)
}

/// The reference a run that asks for `image` runs in. A run that asks for
/// the minimal image where none is named runs in the full image, which has
/// every program the minimal one has.
pub fn reference_for(image: Image) -> Result<String> {
    if image == Image::Minimal {
        if let Some(reference) = minimal_reference()? {
            return Ok(reference);
        }
        crate::diagnostics::diag!(
            "recipe: no minimal runtime image is named (GINUX_MINIMAL_ROOT or {}); this run uses the full image",
            Image::Minimal.reference_file().display()
        );
    }
    root_reference()
}

/// The image's reference from its variable, else from its installed file.
/// Mirrors the instance parser's reference rule (no leading `/`, no `..`,
/// charset `[A-Za-z0-9_.:/@-]`), plus the §2 ≤ 128 B bound.
fn selected_reference(image: Image) -> Result<Option<String>> {
    let name = image.root_env();
    let root = match std::env::var(name) {
        Ok(root) => root,
        Err(std::env::VarError::NotPresent) => match installed_reference(image)? {
            Some(root) => root,
            None => return Ok(None),
        },
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
    Ok(Some(root))
}

/// Which image `reference` is: the minimal one when it is the selected
/// minimal reference, else the full one. Its tar is looked up where that
/// image's is (its variable, its shipped file); a reference both name is one
/// image, the same bytes either way.
fn image_of(reference: &str) -> Image {
    if minimal_reference().ok().flatten().as_deref() == Some(reference) {
        Image::Minimal
    } else {
        Image::Full
    }
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

/// The Ginux launcher this install ships (before `GINUX_BIN` is considered).
pub fn launcher() -> PathBuf {
    under_prefix("libexec/recipe/ginux", LAUNCHER)
}

/// The sources this install ships.
pub fn shipped_sources() -> PathBuf {
    under_prefix("share/recipe/sources", SOURCES)
}

/// Where `image`'s selected reference came from, for `recipe runtime status`.
pub fn reference_origin(image: Image) -> String {
    if std::env::var_os(image.root_env()).is_some() {
        image.root_env().to_owned()
    } else if installed_reference(image).ok().flatten().is_some() {
        format!("installed ({})", image.reference_file().display())
    } else if image == Image::Full {
        "built-in pin".to_owned()
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

pub fn root_digest(reference: &str) -> Result<&str> {
    let (image, digest) = reference.split_once("@sha256:").context(
        "domain.root must identify the assembled rootfs tar as oci:NAME@sha256:CONTENT_DIGEST",
    )?;
    if !image.starts_with("oci:")
        || image.len() <= 4
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!(
            "domain.root must identify the assembled rootfs tar as oci:NAME@sha256:CONTENT_DIGEST"
        );
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

/// Select the root archive of `reference`: its image's variable
/// (`GINUX_ROOTFS`, `GINUX_MINIMAL_ROOTFS`), else the tar the install ships,
/// else the download cache. The engine hashes the selected bytes before use.
pub fn rootfs_path(reference: &str) -> Result<PathBuf> {
    let image = image_of(reference);
    let variable = image.rootfs_env();
    if let Some(path) = std::env::var_os(variable) {
        if path.is_empty() {
            bail!("{variable} is empty; select a verified assembled rootfs tar");
        }
        return Ok(PathBuf::from(path));
    }
    let digest = root_digest(reference)?;
    if installed_reference(image)?.as_deref() == Some(reference) {
        // A standalone install ships the reference but not the tar.
        let shipped = image.shipped_rootfs();
        if shipped.is_file() {
            return Ok(shipped);
        }
    }
    Ok(dirs::cache_dir()
        .context("cannot locate the image cache; set GINUX_ROOTFS to an assembled rootfs tar")?
        .join("recipe/images")
        .join(format!("{digest}.tar")))
}

/// Whether `ensure_rootfs` reports its download as `fetch` events. The
/// coordinator turns this on with `--machine-events`.
static FETCH_EVENTS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_fetch_events(enabled: bool) {
    FETCH_EVENTS.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// `{short}` of the URL template: the first 16 hex digits of the digest.
fn runtime_url(digest: &str) -> String {
    let template = std::env::var("RECIPE_RUNTIME_URL").ok().filter(|t| !t.is_empty()).unwrap_or_else(|| {
        "https://github.com/LevitateOS/prebuilt/releases/download/recipe-runtime-{short}/rootfs.tar.zst"
            .to_owned()
    });
    template
        .replace("{digest}", digest)
        .replace("{short}", &digest[..16])
}

/// The runtime image for `reference`, downloaded on first use. An image
/// selected with `GINUX_ROOTFS`, or already on disk, is returned as it is; the
/// engine verifies its bytes before use either way.
pub fn ensure_rootfs(reference: &str) -> Result<PathBuf> {
    let path = rootfs_path(reference)?;
    let variable = image_of(reference).rootfs_env();
    if std::env::var_os(variable).is_some() || path.exists() {
        return Ok(path);
    }
    let digest = root_digest(reference)?;
    fetch_image(digest, &path, variable)?;
    Ok(path)
}

/// The runtime image for `reference` opened for reading, downloaded first
/// when missing. A downloaded image is opened under its download lock, the
/// lock `recipe runtime remove` also takes, so it is never deleted between
/// being found and being opened (once open, deleting it only unlinks it).
pub fn open_rootfs(reference: &str) -> Result<(PathBuf, std::fs::File)> {
    use fs2::FileExt;
    for _ in 0..3 {
        let path = ensure_rootfs(reference)?;
        let downloaded = std::env::var_os(image_of(reference).rootfs_env()).is_none()
            && path.parent().is_some_and(|directory| {
                images_directory().is_ok_and(|images| images == directory)
            });
        if !downloaded {
            let file = crate::fsx::open_regular(&path)?;
            return Ok((path, file));
        }
        let digest = root_digest(reference)?;
        let lock = path.with_file_name(format!(".fetch-{digest}.lock"));
        let lock =
            crate::fsx::open_lease(&lock, true).context("cannot open the image download lock")?;
        lock.lock_exclusive()
            .context("cannot lock the image download")?;
        match crate::fsx::open_regular(&path) {
            Ok(file) => return Ok((path, file)),
            // Removed after it was found: download it again.
            Err(_) if !path.exists() => continue,
            Err(error) => return Err(error),
        }
    }
    bail!("the runtime image for {reference} was removed repeatedly while a run opened it")
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
    use sha2::{Digest, Sha256};
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
        "Next: run 'recipe runtime fetch' later, or select a local copy with {variable}=/path/rootfs.tar (its SHA-256 must be {digest}); RECIPE_RUNTIME_URL names another address"
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
    let mut hasher = Sha256::new();
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
        hasher.update(&buffer[..n]);
        file.write_all(&buffer[..n])
            .with_context(|| format!("cannot write {}", partial_path.display()))?;
    }
    bar.finish_and_clear();
    let actual = hex::encode(hasher.finalize());
    if actual != digest {
        bail!(Startup.error(format!(
            "the image downloaded from {url} has SHA-256 {actual}, not {digest}; it was discarded\n{way_forward}"
        )));
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
/// the downloaded image files in the download cache and their unpacked
/// copies in the scratch pool, plus each image installed with Recipe (full
/// and minimal) when it is the one in use. An image selected with
/// `GINUX_ROOTFS` or `GINUX_MINIMAL_ROOTFS` is the caller's file, not
/// Recipe's, and is not counted.
pub struct BuildImage {
    /// The download cache (`~/.cache/recipe/images`).
    pub path: PathBuf,
    pub bytes: u64,
    /// Whether `recipe runtime remove` can delete any of it: false when all
    /// there is is the image installed with the system.
    pub removable: bool,
}

/// The download cache of runtime images.
fn images_directory() -> Result<PathBuf> {
    Ok(dirs::cache_dir()
        .context("cannot locate the image cache; set GINUX_ROOTFS to an assembled rootfs tar")?
        .join("recipe/images"))
}

/// The regular files of the download cache (image files and partial
/// downloads; the 0-byte download locks count as nothing).
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
        .sum::<u64>()
        + crate::inputs::image_cache_bytes();
    let full = root_reference().ok();
    let minimal = minimal_reference().ok().flatten();
    let shipped_bytes = [(Image::Full, full), (Image::Minimal, minimal)]
        .into_iter()
        .filter_map(|(image, reference)| {
            let shipped = std::env::var_os(image.rootfs_env()).is_none()
                && rootfs_path(&reference?)
                    .is_ok_and(|selected| selected == image.shipped_rootfs());
            shipped.then(|| std::fs::metadata(image.shipped_rootfs()).map_or(0, |m| m.len()))
        })
        .sum::<u64>();
    Ok(BuildImage {
        path,
        bytes: removable_bytes + shipped_bytes,
        removable: removable_bytes > 0 || shipped_bytes == 0,
    })
}

/// `recipe runtime remove`: delete the downloaded runtime images and their
/// unpacked copies. A download in progress (its lock held) is left alone;
/// the images installed with Recipe and those selected with `GINUX_ROOTFS`
/// or `GINUX_MINIMAL_ROOTFS` are never touched. The image is downloaded again when a recipe needs it.
/// Returns the bytes freed.
pub fn remove_build_image() -> Result<u64> {
    use fs2::FileExt;
    let directory = images_directory()?;
    let mut freed = 0;
    for (path, bytes) in cached_images(&directory) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        // `<digest>.tar`, or `.partial-<digest>-…` of a download.
        let digest = name
            .strip_suffix(".tar")
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
    freed += crate::inputs::remove_image_caches()?;
    Ok(freed)
}
