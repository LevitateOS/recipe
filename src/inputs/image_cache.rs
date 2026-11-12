//! Verified, unpacked root images cached per digest in the coordinator's
//! private scratch pool, and per-invocation roots cloned from them.
//!
//! The cache keeps every guarantee an uncached root has:
//! - A cached tree is made only by [`super::root::unpack_image`] (the same
//!   validator and code as an uncached root) from image bytes verified
//!   against the pinned digest, in a partial directory that is renamed to its
//!   digest's name only when complete. Its name is the digest it was verified
//!   against, so a different `domain.root` never matches it.
//! - It is never handed to a domain. Each root is a new tree of new inodes:
//!   regular files are `FICLONE` clones that share data extents copy-on-write,
//!   so nothing written through one root reaches the cache or another root.
//!   Hardlinks exist only among one root's own files, as in the image.
//! - It is trusted only during the boot that made it. A crash can leave
//!   unpacked bytes that never reached the disk; instead of an fsync of the
//!   whole tree, a cache from another boot is discarded and rebuilt.
//! - Without reflinks (tmpfs, ext4, another filesystem) there is no cache and
//!   every root is unpacked from the verified image, as before.
//!
//! One lock orders the pool's caches: cloning holds it shared; eviction and
//! the rename that publishes a cache hold it exclusively, briefly, so a cache
//! is never removed while a root is cloned from it. Unpacking a new cache
//! holds only that digest's build lock, so concurrent invocations of other
//! images keep cloning meanwhile, and two builds of one image never race.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;

use super::root::VerifiedImage;

/// Caches always kept, the newest by last use, when a new one is published.
const KEEP: usize = 2;
/// Older caches are kept too while they were used this recently.
const KEEP_USED_WITHIN: std::time::Duration = std::time::Duration::from_secs(3600);
const LOCK: &str = "images.lock";
const PREFIX: &str = "image-";
const PARTIAL: &str = ".partial-";
/// Suffix of a digest's build lock, `image-<digest>.build`, beside the pool.
const BUILD: &str = ".build";
/// Holds the boot id the cache was made in; written last, before the rename.
const READY: &str = "ready";
const TREE: &str = "tree";

/// Fill the empty directory `root` with the image named by `reference`, cloned
/// from its cache (built first from the verified image when absent). Returns
/// false, with `root` still empty, when no cache can serve it: the caller then
/// unpacks the verified image itself.
pub(super) fn populate(
    reference: &str,
    image: &mut Option<VerifiedImage>,
    root: &Path,
) -> Result<bool> {
    let Some(boot) = boot_id() else {
        return Ok(false);
    };
    let pool = crate::temporary::pool()?;
    if !reflinks(&pool)? {
        return Ok(false);
    }
    let digest = crate::runtime::root_digest(reference)?;
    let cache = pool.join(format!("{PREFIX}{digest}"));
    let lock = crate::fsx::open_lease(&pool.join(LOCK), true)?;
    // Publishing releases the exclusive lock before cloning takes it shared;
    // a concurrent publication of another image may evict this one between.
    for _ in 0..3 {
        lock.lock_shared().context("cannot lock the image cache")?;
        if ready(&cache, &boot) {
            // Last use orders eviction; a failed touch only ages the cache.
            let _ = OpenOptions::new()
                .write(true)
                .open(cache.join(READY))
                .and_then(|file| file.set_modified(std::time::SystemTime::now()));
            let cloned = clone_tree(&cache.join(TREE), root);
            lock.unlock()?;
            return match cloned {
                Ok(()) => Ok(true),
                Err(error) if cancelled(&error) => Err(error),
                Err(error) => {
                    crate::diagnostics::diag!(
                        "recipe: warning: cannot clone the cached image, unpacking it instead: {error:#}"
                    );
                    crate::fsx::make_removable(root);
                    fs::remove_dir_all(root)?;
                    fs::create_dir(root)?;
                    Ok(false)
                }
            };
        }
        lock.unlock()?;
        let building = crate::fsx::open_lease(&build_lock(&cache), true)?;
        building
            .lock_exclusive()
            .context("cannot lock the image cache build")?;
        if !ready(&cache, &boot) {
            build(&pool, &cache, &boot, reference, image, &lock)?;
        }
        drop(building);
    }
    bail!("the image cache was evicted repeatedly while in use")
}

fn cancelled(error: &anyhow::Error) -> bool {
    crate::failure::Category::of(error) == Some(crate::failure::Category::Cancelled)
        || crate::cancellation::signal() != 0
}

/// This boot's identity, or None when the kernel does not expose one.
fn boot_id() -> Option<String> {
    let id = fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

/// Whether files in the pool can be cloned. Empty probe files: cloning them
/// needs no writeback, and support is decided before their length is.
fn reflinks(pool: &Path) -> Result<bool> {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if let Some(supported) = SUPPORTED.get() {
        return Ok(*supported);
    }
    let source = tempfile::tempfile_in(pool)?;
    let clone = tempfile::tempfile_in(pool)?;
    let supported = crate::fsx::reflink(&source, &clone)?;
    Ok(*SUPPORTED.get_or_init(|| supported))
}

fn ready(cache: &Path, boot: &str) -> bool {
    fs::symlink_metadata(cache).is_ok_and(|metadata| metadata.is_dir())
        && fs::symlink_metadata(cache.join(TREE)).is_ok_and(|metadata| metadata.is_dir())
        && fs::read_to_string(cache.join(READY)).is_ok_and(|made| made == boot)
}

/// A partial cache, removed unless it was renamed into place.
struct Partial(Option<tempfile::TempDir>);
impl Drop for Partial {
    fn drop(&mut self) {
        if let Some(directory) = &self.0 {
            crate::fsx::make_removable(directory.path());
        }
    }
}

fn build_lock(cache: &Path) -> PathBuf {
    let mut name = cache.as_os_str().to_owned();
    name.push(BUILD);
    PathBuf::from(name)
}

/// Unpack the verified image into a new cache and publish it. Called with
/// this digest's build lock held; the pool lock is taken exclusively only to
/// evict and rename.
fn build(
    pool: &Path,
    cache: &Path,
    boot: &str,
    reference: &str,
    image: &mut Option<VerifiedImage>,
    lock: &std::fs::File,
) -> Result<()> {
    let name = cache.file_name().context("image cache has no name")?;
    let mut partial = Partial(Some(
        tempfile::Builder::new()
            .prefix(&format!("{}{PARTIAL}", name.to_string_lossy()))
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(pool)?,
    ));
    let directory = partial.0.as_ref().expect("partial cache").path().to_owned();
    let tree = directory.join(TREE);
    fs::create_dir(&tree)?;
    super::root::unpack_image(super::root::verified_snapshot(image, reference)?, &tree)?;
    fs::write(directory.join(READY), boot)?;
    lock.lock_exclusive()
        .context("cannot lock the image cache")?;
    evict(pool, cache, boot);
    let published = fs::rename(&directory, cache).context("cannot publish the image cache");
    lock.unlock()?;
    published?;
    let _published = partial.0.take().expect("partial cache").keep();
    Ok(())
}

/// Remove partial caches whose build was abandoned, caches from other boots,
/// a stale cache at `replacing`, and caches beyond the [`KEEP`] newest that
/// were not used within [`KEEP_USED_WITHIN`]. Called with the pool lock held
/// exclusively, so no root is being cloned. Housekeeping: never fails.
fn evict(pool: &Path, replacing: &Path, boot: &str) {
    let Ok(entries) = fs::read_dir(pool) else {
        return;
    };
    let mut kept = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(PREFIX) || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let path = entry.path();
        if let Some((cache, _)) = name.split_once(PARTIAL) {
            // A partial is abandoned when no build holds its digest's build
            // lock (this build holds it for the partial being published).
            let abandoned =
                match crate::fsx::open_lease(&pool.join(format!("{cache}{BUILD}")), false) {
                    Ok(lock) => lock.try_lock_exclusive().is_ok(),
                    Err(_) => true,
                };
            if abandoned {
                remove(&path);
            }
            continue;
        }
        if path == replacing || !ready(&path, boot) {
            remove(&path);
            continue;
        }
        if let Ok(used) = fs::metadata(path.join(READY)).and_then(|m| m.modified()) {
            kept.push((used, path));
        }
    }
    kept.sort_by_key(|(used, _)| std::cmp::Reverse(*used));
    let now = std::time::SystemTime::now();
    // One slot is for the cache being published.
    for (used, path) in kept.into_iter().skip(KEEP - 1) {
        if now
            .duration_since(used)
            .is_ok_and(|idle| idle > KEEP_USED_WITHIN)
        {
            remove(&path);
        }
    }
}

/// Removing a cache is housekeeping; leftovers never fail an invocation.
fn remove(path: &Path) {
    crate::fsx::make_removable(path);
    if let Err(error) = fs::remove_dir_all(path) {
        crate::diagnostics::diag!(
            "recipe: warning: cannot remove image cache {}: {error}",
            path.display()
        );
    }
}

/// Recreate the tree `source` under the empty directory `root` with new
/// inodes: directories and symlinks made anew, regular files cloned, and
/// hardlinks among the tree's own files linked again. Entries are created in
/// `source`'s directory order, which is the order the image unpacked them in,
/// so the root lists its directories as an unpacked root does.
fn clone_tree(source: &Path, root: &Path) -> Result<()> {
    let mut links = HashMap::<(u64, u64), PathBuf>::new();
    let mut directories = Vec::new();
    for entry in walkdir::WalkDir::new(source)
        .follow_links(false)
        .min_depth(1)
    {
        crate::cancellation::check()?;
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        let destination = root.join(relative);
        let metadata = entry.metadata()?;
        let mode = metadata.mode() & 0o777;
        let kind = entry.file_type();
        if kind.is_dir() {
            fs::DirBuilder::new().mode(0o700).create(&destination)?;
            directories.push((destination, mode));
        } else if kind.is_file() {
            if metadata.nlink() > 1 {
                let key = (metadata.dev(), metadata.ino());
                if let Some(first) = links.get(&key) {
                    fs::hard_link(first, &destination)?;
                    continue;
                }
                links.insert(key, destination.clone());
            }
            let input = crate::fsx::open_regular(entry.path())?;
            let output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&destination)?;
            if !crate::fsx::reflink(&input, &output)? {
                bail!("cannot clone cached image file {}", relative.display());
            }
            output.set_permissions(fs::Permissions::from_mode(mode))?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, &destination)?;
        } else {
            bail!("unexpected image cache entry {}", relative.display());
        }
    }
    // Children before parents, as an unpacked root applies its modes.
    for (directory, mode) in directories.into_iter().rev() {
        fs::set_permissions(directory, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}
