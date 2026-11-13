//! Installed Ginux defaults, shared by the engine and OS runtime packaging.
//! Explicit environment selections take precedence; payload verification and
//! mandatory managed execution remain unchanged.
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub const LAUNCHER: &str = "/usr/libexec/recipe/ginux";
pub const ROOTFS: &str = "/usr/share/recipe/runtime/rootfs.tar";
pub const ROOT_REFERENCE: &str = "/usr/share/recipe/runtime/root-reference";
/// Where a standalone install keeps the sources it ships, under the prefix.
pub const SOURCES: &str = "/usr/share/recipe/sources";
const MAX_CONFIG_PATH: usize = 128;

/// Default immutable image reference: the pinned Rocky rootfs content,
/// named by digest rather than by host path (host `.artifacts` paths never
/// enter instance config). Override with `GINUX_ROOT`; the override must pass
/// the same reference validation. The launcher resolves the reference.
const DEFAULT_ROOT_REF: &str = "oci:rocky-10.2.20260525.0-amd64-rootfs@sha256:b93bfd0f6deea2a7a96216655cda3ebbd87a445231548ff491ee30aed20a36ab";

/// Image reference: the pin by default, `GINUX_ROOT` override when set.
/// Mirrors the instance parser's reference rule (no leading `/`, no `..`,
/// charset `[A-Za-z0-9_.:/@-]`), plus the §2 ≤ 128 B bound.
pub fn root_reference() -> Result<String> {
    let root = match std::env::var("GINUX_ROOT") {
        Ok(root) => root,
        Err(std::env::VarError::NotPresent) => {
            installed_reference()?.unwrap_or_else(|| DEFAULT_ROOT_REF.to_string())
        }
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("input preparation failure: GINUX_ROOT is not valid UTF-8")
        }
    };
    if root.is_empty() || root.len() > MAX_CONFIG_PATH {
        bail!(
            "input preparation failure: instance root {root:?} has {} B, want 1..={MAX_CONFIG_PATH} B (§2)",
            root.len(),
        );
    }
    validate_reference(&root)?;
    Ok(root)
}

/// The install prefix of the running executable: `/usr` for `/usr/bin/recipe`,
/// `~/.local` for `~/.local/bin/recipe`.
pub fn prefix() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
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

/// The rootfs tar this install ships; it may not exist.
pub fn shipped_rootfs() -> PathBuf {
    under_prefix("share/recipe/runtime/rootfs.tar", ROOTFS)
}

/// The file naming the runtime image this install was made for.
pub fn reference_file() -> PathBuf {
    under_prefix("share/recipe/runtime/root-reference", ROOT_REFERENCE)
}

/// The sources this install ships.
pub fn shipped_sources() -> PathBuf {
    under_prefix("share/recipe/sources", SOURCES)
}

/// Where the selected reference came from, for `recipe runtime status`.
pub fn reference_origin() -> String {
    if std::env::var_os("GINUX_ROOT").is_some() {
        "GINUX_ROOT".to_owned()
    } else if installed_reference().ok().flatten().is_some() {
        format!("installed root-reference ({})", reference_file().display())
    } else {
        "built-in pin".to_owned()
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

fn installed_reference() -> Result<Option<String>> {
    match std::fs::read_to_string(reference_file()) {
        Ok(value) => Ok(Some(value.trim().to_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("cannot read installed Recipe runtime reference"),
    }
}

/// Select the root archive; the engine hashes the selected bytes before use.
pub fn rootfs_path(reference: &str) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("GINUX_ROOTFS") {
        if path.is_empty() {
            bail!("GINUX_ROOTFS is empty; select a verified assembled rootfs tar");
        }
        return Ok(PathBuf::from(path));
    }
    let digest = root_digest(reference)?;
    if installed_reference()?.as_deref() == Some(reference) {
        // A standalone install ships the reference but not the tar.
        let shipped = shipped_rootfs();
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
    if std::env::var_os("GINUX_ROOTFS").is_some() || path.exists() {
        return Ok(path);
    }
    let digest = root_digest(reference)?;
    fetch_image(digest, &path)?;
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

fn fetch_image(digest: &str, destination: &Path) -> Result<()> {
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
        "Next: run 'recipe runtime fetch' later, or select a local copy with GINUX_ROOTFS=/path/rootfs.tar (its SHA-256 must be {digest}); RECIPE_RUNTIME_URL names another address"
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
    let bar = indicatif::ProgressBar::with_draw_target(total, crate::diagnostics::progress_target());
    bar.set_style(
        indicatif::ProgressStyle::with_template("{bar:40} {bytes}/{total_bytes} {bytes_per_sec} eta {eta}")
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
    let mut decoder = zstd::stream::read::Decoder::new(counting).map_err(|error| {
        Startup.error(format!("cannot start decompressing {url}: {error}"))
    })?;

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
