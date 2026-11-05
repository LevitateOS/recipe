//! Input discovery: base recipe, Rhai imports, and declared extras.
//!
//! Resolution mirrors the executor: `//! extends:` and `import "..."` paths
//! resolve relative to the referring file's directory first, then against the
//! executor search path. Anything else — absolute host paths, missing files,
//! symlinks, directories, secret-bearing names — fails with an explicit
//! error instead of staging unchecked host state.
//!
//! Staged domain names preserve relative directories under `/inputs`. Literal
//! imports and dependency declarations are discovered by parsing, never by
//! evaluating recipe code on the coordinator.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::core::executor::parse_extends;

/// Domain name of the main recipe inside `/inputs`.
pub const MAIN_NAME: &str = "main.rhai";
/// Domain path of the main recipe inside `/inputs`.
pub const MAIN_NAME_DOMAIN: &str = "/inputs/main.rhai";

/// G10 input-name bound mirrored: `inputs/` + NAME fits the 100-byte ustar
/// field and `/inputs/` + NAME fits the §2 ≤ 128 B domain-path bound.
const MAX_INPUT_NAME: usize = 92;
/// G10 per-file/total caps mirrored (streamed, never fully buffered).
const MAX_INPUT_FILE: u64 = 512 * 1024 * 1024;
const MAX_INPUTS_TOTAL: u64 = 2 * 1024 * 1024 * 1024;
/// Transitive import depth; deeper nesting is an explicit failure, not a hang.
const MAX_IMPORT_DEPTH: usize = 16;

/// One staged host file and its `/inputs/NAME` identity.
#[derive(Debug, Clone)]
pub struct StagedInput {
    /// Relative domain name under `/inputs`; each component uses `[A-Za-z0-9._-]`.
    pub name: String,
    /// Host file this was staged from (never crosses into the domain).
    pub host_path: PathBuf,
    /// Byte size.
    pub size: u64,
    /// sha256 of the file content.
    pub sha256: [u8; 32],
    /// Unix permission bits (`& 0o777`).
    pub mode: u32,
    /// How the referring file named this input (`extends:`/`import` target,
    /// file name, or explicit extra NAME); R06/I02 layout reconstruction.
    pub referenced_as: String,
}

/// Caller-declared asset, source-closure, or cache file/tree: `NAME=host-path`.
#[derive(Debug, Clone)]
pub struct ExtraInput {
    /// Relative domain name; validated like every other input name.
    pub name: String,
    /// Host file to stage at `/inputs/{name}`.
    pub path: PathBuf,
}

/// Discovery result consumed by [`super::prepare_invocation`][crate::inputs].
#[derive(Debug)]
pub struct Discovered {
    /// Staged inputs sorted by domain name.
    pub inputs: Vec<StagedInput>,
    /// sha256 hex of the main recipe source (protocol `recipe_digest`).
    pub recipe_digest: String,
    /// Main recipe file stem (invocation-id basis).
    pub recipe_stem: String,
    /// Canonical host recipe directories (leak-check markers, never emitted).
    pub host_dirs: Vec<PathBuf>,
}

/// Discover and hash the full input set without copying anything yet.
pub fn discover_inputs(
    recipe: &Path,
    search_path: Option<&Path>,
    extra_inputs: &[ExtraInput],
) -> Result<Discovered> {
    let mut collector = Collector::new(search_path);

    let recipe_source = read_recipe_file(recipe)?;
    let recipe_digest = hex::encode(Sha256::digest(recipe_source.as_bytes()));
    let recipe_stem = recipe
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .with_context(|| {
            format!(
                "input preparation failure: recipe path has no file name: {}",
                recipe.display()
            )
        })?;

    let recipe_canonical = canonical_host(recipe)?;
    collector.note_host_dir(&recipe_canonical);
    collector.stage_named(MAIN_NAME, recipe, MAIN_NAME)?;
    collector
        .visited
        .insert((recipe_canonical.clone(), MAIN_NAME.to_owned()));
    collector.collect_related(&recipe_canonical, MAIN_NAME, &recipe_source, 0)?;

    for extra in extra_inputs {
        validate_input_name(&extra.name)?;
        let meta = std::fs::symlink_metadata(&extra.path)?;
        if meta.is_dir() {
            for entry in walkdir::WalkDir::new(&extra.path)
                .follow_links(false)
                .sort_by_file_name()
                .into_iter()
                .filter_entry(|entry| {
                    entry.depth() != 1
                        || !matches!(entry.file_name().to_str(), Some(".git" | "target"))
                })
            {
                let entry = entry
                    .context("input preparation failure: cannot traverse declared input tree")?;
                if entry.file_type().is_dir() {
                    continue;
                }
                let relative = entry.path().strip_prefix(&extra.path)?;
                let name = format!(
                    "{}/{}",
                    extra.name,
                    relative
                        .to_str()
                        .context("input preparation failure: non-Unicode source name")?
                );
                collector.stage_named(&name, entry.path(), &name)?;
            }
        } else {
            collector.stage_named(&extra.name, &extra.path, &extra.name)?;
        }
        if let Ok(canonical) = extra.path.canonicalize() {
            collector.note_host_dir(&canonical);
        }
    }

    collector.finish(recipe_digest, recipe_stem)
}

/// Read one recipe/module file: must exist, be a regular file (never a
/// symlink or directory), and be valid UTF-8 Rhai source.
fn read_recipe_file(path: &Path) -> Result<String> {
    let meta = std::fs::symlink_metadata(path).with_context(|| {
        format!(
            "input preparation failure: cannot stat recipe file {}",
            path.display()
        )
    })?;
    if meta.file_type().is_symlink() {
        bail!(
            "input preparation failure: recipe file {} is a symlink \
             (only regular files are staged; resolve it to a real path first)",
            path.display()
        );
    }
    if !meta.file_type().is_file() {
        bail!(
            "input preparation failure: recipe path {} is not a regular file \
             (directories are never copied recursively)",
            path.display()
        );
    }
    std::fs::read_to_string(path)
        .with_context(|| format!("input preparation failure: cannot read {}", path.display()))
}

/// Canonical host identity for cycle detection (internal only).
fn canonical_host(path: &Path) -> Result<PathBuf> {
    path.canonicalize().with_context(|| {
        format!(
            "input preparation failure: cannot resolve {}",
            path.display()
        )
    })
}

/// Collector stages each domain name at most once and tracks the byte total.
struct Collector<'a> {
    search_path: Option<&'a Path>,
    staged: Vec<StagedInput>,
    by_name: HashMap<String, PathBuf>,
    visited: HashSet<(PathBuf, String)>,
    total: u64,
    host_dirs: Vec<PathBuf>,
}

impl<'a> Collector<'a> {
    fn new(search_path: Option<&'a Path>) -> Self {
        Self {
            search_path,
            staged: Vec::new(),
            by_name: HashMap::new(),
            visited: HashSet::new(),
            total: 0,
            host_dirs: Vec::new(),
        }
    }

    fn note_host_dir(&mut self, canonical: &Path) {
        if let Some(parent) = canonical.parent() {
            let parent = parent.to_path_buf();
            if !self.host_dirs.contains(&parent) {
                self.host_dirs.push(parent);
            }
        }
    }

    /// Stage `host` at `/inputs/{name}`, hashing and validating it.
    ///
    /// Restaging the same domain name with the same host bytes is a no-op
    /// (shared imports); the same name with different bytes is an explicit
    /// duplicate-name failure.
    fn stage_named(&mut self, name: &str, host: &Path, referenced_as: &str) -> Result<()> {
        validate_input_name(name)?;
        if let Some(first) = self.by_name.get(name) {
            if same_file(first, host) {
                return Ok(());
            }
            bail!(
                "input preparation failure: duplicate input name {name:?}: {} and {} \
                 (domain input names must be unique; rename the extra input)",
                first.display(),
                host.display()
            );
        }
        if self.staged.len() >= 100_000 {
            bail!("input preparation failure: too many input files");
        }
        let mut staged = validate_and_hash(name, host)?;
        staged.referenced_as = referenced_as.to_string();
        self.total = self.total.checked_add(staged.size).with_context(|| {
            format!("input preparation failure: input byte total overflowed at {name:?}")
        })?;
        if self.total > MAX_INPUTS_TOTAL {
            bail!(
                "input preparation failure: inputs total {total} B exceeds {MAX_INPUTS_TOTAL} B \
                 (declare a smaller source closure)",
                total = self.total,
            );
        }
        self.by_name.insert(name.to_string(), host.to_path_buf());
        self.staged.push(staged);
        Ok(())
    }

    /// Preserve each reference's directory layout instead of flattening names.
    fn collect_related(
        &mut self,
        canonical: &Path,
        domain_name: &str,
        source: &str,
        depth: usize,
    ) -> Result<()> {
        if depth > MAX_IMPORT_DEPTH {
            bail!(
                "input preparation failure: import/dependency nesting exceeds {MAX_IMPORT_DEPTH}"
            );
        }
        let mut references = static_references(source)?;
        if let Some(base) = parse_extends(source) {
            references.push((base, false));
        }
        for (target, module) in references {
            let target = if module {
                let mut path = PathBuf::from(&target);
                path.set_extension("rhai");
                path.to_string_lossy().into_owned()
            } else {
                target
            };
            let host = self.resolve_related(&target, canonical, "recipe/module")?;
            let beside_referrer = canonical
                .parent()
                .is_some_and(|parent| parent.join(&target).is_file());
            let name = if beside_referrer {
                Path::new(domain_name)
                    .parent()
                    .unwrap_or(Path::new(""))
                    .join(&target)
            } else {
                PathBuf::from(&target)
            };
            let name = name
                .to_str()
                .context("input preparation failure: non-Unicode domain path")?;
            validate_input_name(name)?;
            // Validate the selected path before canonicalization, preserving symlink rejection.
            let source = read_recipe_file(&host)?;
            let canonical = canonical_host(&host)?;
            self.note_host_dir(&canonical);
            self.stage_named(name, &host, &target)?;
            if self.visited.insert((canonical.clone(), name.to_owned())) {
                self.collect_related(&canonical, name, &source, depth + 1)?;
            }
        }
        Ok(())
    }

    /// Resolve an `extends:`/`import` target like the executor does, refusing
    /// absolute host paths outright.
    fn resolve_related(&self, target: &str, referrer: &Path, kind: &str) -> Result<PathBuf> {
        let target = target.trim();
        if target.is_empty() {
            bail!(
                "input preparation failure: empty {kind} target in {} \
                 (declare the base/module path explicitly)",
                referrer.display()
            );
        }
        if target.starts_with('/') {
            bail!(
                "input preparation failure: {kind} target {target:?} in {} is an absolute host path \
                 (host checkout paths never enter the domain; use a path relative to the recipe \
                 or the recipes search path)",
                referrer.display()
            );
        }
        if Path::new(target)
            .components()
            .any(|component| component == std::path::Component::ParentDir)
        {
            bail!(
                "input preparation failure: {kind} target {target:?} in {} escapes with `..` \
                 (keep base/module paths inside the recipe tree)",
                referrer.display()
            );
        }
        if let Some(parent) = referrer.parent() {
            let candidate = parent.join(target);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        if let Some(search) = self.search_path {
            let candidate = search.join(target);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        bail!(
            "input preparation failure: {kind} target {target:?} from {} not found \
             (tried the recipe directory and {:?}; only declared files are staged)",
            referrer.display(),
            self.search_path,
        )
    }

    fn finish(self, recipe_digest: String, recipe_stem: String) -> Result<Discovered> {
        let mut inputs = self.staged;
        inputs.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(Discovered {
            inputs,
            recipe_digest,
            recipe_stem,
            host_dirs: self.host_dirs,
        })
    }
}

/// Best-effort producer guard mirrored from G10: well-known secret-bearing
/// locations and key material are never silently packaged. Explicit selection
/// plus regular-file-only staging is the mechanism; this is not a boundary.
fn reject_secret_bearing_host(host: &Path, name: &str) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = host.as_os_str().as_bytes();
    for component in bytes.split(|byte| *byte == b'/') {
        match component {
            b".ssh" | b".gnupg" | b"id_rsa" | b"id_ed25519" | b".netrc" | b".git-credentials"
            | b"credentials.json" => {
                bail!(
                    "input preparation failure: refusing secret-bearing input {name:?} \
                     (host path names a credential location; stage only declared build inputs)"
                )
            }
            _ => {}
        }
        if component.len() > 4 && (component.ends_with(b".pem") || component.ends_with(b".key")) {
            bail!(
                "input preparation failure: refusing secret-bearing input {name:?} \
                 (host path names key material; stage only declared build inputs)"
            );
        }
    }
    Ok(())
}

/// G10 input-name rules mirrored: charset, length, reserved names.
pub(super) fn validate_input_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > MAX_INPUT_NAME {
        bail!(
            "input preparation failure: input name {name:?} has {} B, want 1..={MAX_INPUT_NAME} B \
             (so `/inputs/` + NAME fits the §2 domain-path bound)",
            name.len(),
        );
    }
    if name
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
        || name.split('/').next() == Some("recipe-worker")
    {
        bail!("input preparation failure: input name {name:?} is reserved");
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
    {
        bail!(
            "input preparation failure: input name {name:?} has unexpected characters \
             (want `[A-Za-z0-9._-]`, so the NAME is a safe tar member and domain path)"
        );
    }
    Ok(())
}

/// Validate one host file and hash it by streaming (never fully buffered).
fn validate_and_hash(name: &str, host: &Path) -> Result<StagedInput> {
    let meta = std::fs::symlink_metadata(host).with_context(|| {
        format!(
            "input preparation failure: cannot stat input {name:?} at {}",
            host.display()
        )
    })?;
    if meta.file_type().is_symlink() {
        bail!(
            "input preparation failure: input {name:?} at {} is a symlink \
             (only regular files are staged; stage the target file explicitly)",
            host.display()
        );
    }
    if !meta.file_type().is_file() {
        bail!(
            "input preparation failure: input {name:?} at {} is not a regular file \
             (directories are never copied recursively)",
            host.display()
        );
    }
    reject_secret_bearing_host(host, name)?;
    let size = meta.len();
    if size > MAX_INPUT_FILE {
        bail!(
            "input preparation failure: input {name:?} is {size} B, exceeding {MAX_INPUT_FILE} B \
             (declare a smaller source closure)"
        );
    }
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o777
    };
    #[cfg(not(unix))]
    let mode = 0o644;
    let sha256 = hash_file(host, size)?;
    Ok(StagedInput {
        name: name.to_string(),
        host_path: host.to_path_buf(),
        size,
        sha256,
        mode,
        referenced_as: String::new(),
    })
}

/// Stream `host` through sha256, failing if it grows while being read.
pub fn hash_file(host: &Path, expected: u64) -> Result<[u8; 32]> {
    let mut source = File::open(host)
        .with_context(|| format!("input preparation failure: cannot open {}", host.display()))?;
    let mut hasher = Sha256::new();
    let copied = std::io::copy(&mut source, &mut WriterHasher(&mut hasher))?;
    if copied != expected {
        bail!(
            "input preparation failure: {} changed while reading ({copied} B vs {expected} B)",
            host.display(),
        );
    }
    Ok(hasher.finalize().into())
}

struct WriterHasher<'a>(&'a mut Sha256);

impl Write for WriterHasher<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Compile only: no module loading, top-level evaluation or native helpers on
/// the coordinator. Dynamic imports/dependency declarations need explicit inputs.
fn static_references(source: &str) -> Result<Vec<(String, bool)>> {
    use rhai::{ASTNode, Expr, OptimizationLevel, Stmt};
    let mut engine = rhai::Engine::new();
    engine.set_optimization_level(OptimizationLevel::None);
    let ast = engine
        .compile(source)
        .context("input preparation failure: invalid Rhai source")?;
    let mut references = Vec::new();
    let mut invalid = None;
    ast.walk(&mut |nodes| {
        if let Some(ASTNode::Stmt(Stmt::Import(import, _))) = nodes.last() {
            match &import.0 {
                Expr::StringConstant(path, _) => references.push((path.to_string(), true)),
                _ => invalid = Some("dynamic import cannot be prepared; use a literal module path"),
            }
        }
        true
    });
    for statement in ast.statements() {
        if let Stmt::Var(declaration, _, _) = statement {
            if matches!(declaration.0.name.as_str(), "deps" | "build_deps") {
                match &declaration.1 {
                    Expr::Array(items, _) => {
                        for item in items {
                            match item {
                                Expr::StringConstant(name, _) => {
                                    references.push((format!("{name}.rhai"), false))
                                }
                                _ => {
                                    invalid = Some(
                                        "dependency names must be literal strings for input preparation",
                                    )
                                }
                            }
                        }
                    }
                    _ => {
                        invalid = Some(
                            "dependency declarations must be literal arrays for input preparation",
                        )
                    }
                }
            }
        }
    }
    if let Some(reason) = invalid {
        bail!("input preparation failure: {reason}");
    }
    Ok(references)
}

/// Same host file reached under two names (e.g. via search path overlap).
fn same_file(first: &Path, second: &Path) -> bool {
    match (first.canonicalize(), second.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => first == second,
    }
}
