//! Statically declared recipe authority. Reading it never evaluates recipe code.
pub mod contract;
pub mod report;
use anyhow::{Context, Result, bail, ensure};
use rhai::{ASTFlags, Expr, Stmt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Kinds that can cross a recipe's input/publication boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathKind {
    File,
    Directory,
}

impl PathKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
        }
    }
    pub(crate) fn matches(self, metadata: &std::fs::Metadata) -> bool {
        match self {
            Self::File => metadata.is_file(),
            Self::Directory => metadata.is_dir(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputNeed {
    pub kind: PathKind,
    pub optional: bool,
}

/// The complete authority declaration of one executable recipe.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Needs {
    pub inputs: BTreeMap<String, InputNeed>,
    /// Each define the recipe reads, to its default; `None` means required.
    pub defines: BTreeMap<String, Option<String>>,
    pub outputs: BTreeMap<String, PathKind>,
    pub network: Vec<String>,
    pub memory_mib: u64,
    pub storage_max_objects: u64,
    pub storage_max_bytes: Option<u64>,
    pub timeout_secs: u64,
    /// The bytes the recipe declares for its own downloads. For a recipe
    /// with `PACKAGES` it is what `NEEDS.network_max_bytes` says, or 0 when
    /// left out; the pins in use and Recipe's allowance for connections and
    /// retries are added by [`Needs::network_budget`].
    pub network_max_bytes: u64,
    /// The runtime image a Ginux run of the recipe is unpacked from:
    /// `NEEDS.runtime: "minimal"` for a recipe whose programs are the tools
    /// of a root it is given (a shell, coreutils, findutils, sed and grep are
    /// all it has of its own), the full image otherwise
    /// ([`crate::runtime::Image`]).
    #[serde(default, skip_serializing_if = "crate::runtime::Image::is_full")]
    pub runtime: crate::runtime::Image,
    /// For a recipe with `PACKAGES`: each pins section's C library (`glibc`,
    /// `musl`, `any`) to the bytes and number of its packages
    /// (docs/recipe-format.md, section 2.7, decision 9). Read from the pins
    /// file, never declared.
    #[serde(skip)]
    pub pins: BTreeMap<String, Payload>,
    /// The number of `SOURCES` entries fetched on each C library: `any` for
    /// the entries without `libc`, `glibc` and `musl` for the others.
    #[serde(skip)]
    pub source_files: BTreeMap<String, u64>,
}

/// What a run downloads: bytes of payload in a number of files.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Payload {
    pub bytes: u64,
    pub files: u64,
}

/// Bytes one downloaded file may cost on the wire besides its own bytes: the
/// TLS handshake of its connection (key exchange and the server's
/// certificate chain, 3-10 KiB), the HTTP request and response headers
/// (about 1-2 KiB), and the same once more when the server redirects to a
/// mirror or a CDN on another connection, with room to spare.
const ALLOWANCE_PER_FILE: u64 = 64 * 1024;

/// The share of the payload set aside for retries (`helpers/acquire/retry.rs`
/// tries a transient failure up to four times, and a body cut off is fetched
/// again from its start) and for the framing of TLS records (under 0.5%): a
/// quarter of the payload may be transferred a second time.
const RETRY_SHARE_DIVISOR: u64 = 4;

/// The network byte cap of a run that downloads `payload`. One rule for both
/// kinds of run: Ginux counts every byte sent and received on the run's
/// connections against `[network] max_bytes`; a host run counts response
/// bodies (`helpers/acquire/net.rs`). The cap stays bounded by the payload:
/// a run cannot pull unrelated gigabytes.
pub fn download_cap(payload: Payload) -> u64 {
    // The files themselves.
    let files = payload.bytes;
    // Handshakes, headers and redirects, per file.
    let connections = payload.files.saturating_mul(ALLOWANCE_PER_FILE);
    // Retries and record framing.
    let retries = payload.bytes / RETRY_SHARE_DIVISOR;
    files.saturating_add(connections).saturating_add(retries)
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LiteralNeeds {
    inputs: BTreeMap<String, String>,
    defines: BTreeMap<String, serde_json::Value>,
    outputs: BTreeMap<String, PathKind>,
    network: Vec<String>,
    memory_mib: Option<u64>,
    storage_max_objects: Option<u64>,
    storage_max_bytes: Option<u64>,
    timeout_secs: Option<u64>,
    network_max_bytes: Option<u64>,
    runtime: Option<String>,
}

/// Explicit caller restrictions. None uses the recipe's declaration; an empty
/// network subset disables networking. These fields never introduce authority.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Narrowing {
    pub network: Option<Vec<String>>,
    pub memory_mib: Option<u64>,
    pub storage_max_objects: Option<u64>,
    pub storage_max_bytes: Option<u64>,
    pub timeout_secs: Option<u64>,
    pub network_max_bytes: Option<u64>,
}

impl Needs {
    fn parse(value: serde_json::Value) -> Result<Self> {
        let raw: LiteralNeeds =
            serde_json::from_value(value).context("invalid NEEDS declaration")?;
        let defaults = crate::ExecutionOptions::default();
        let mut inputs = BTreeMap::new();
        for (name, kind) in raw.inputs {
            crate::domain::validate_input_name(&name)?;
            ensure!(
                !name.contains('/') && !name.contains("::") && name.len() <= 64,
                "NEEDS.inputs names must be single components of at most 64 bytes"
            );
            let optional = kind.ends_with('?');
            let kind = match kind.strip_suffix('?').unwrap_or(&kind) {
                "file" => PathKind::File,
                "directory" => PathKind::Directory,
                _ => bail!("NEEDS.inputs.{name} must be file, directory, file? or directory?"),
            };
            ensure!(
                !crate::domain::RESERVED_INPUT_NAMES.contains(&name.as_str())
                    && name != crate::domain::DEPENDENCY_INPUT
                    && name != crate::domain::DEPENDENCY_TOOLS,
                "NEEDS.inputs.{name} is reserved"
            );
            inputs.insert(name, InputNeed { kind, optional });
        }
        let mut defines = BTreeMap::new();
        for (name, default) in raw.defines {
            ensure!(
                name.bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                    && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    && name.len() <= 64,
                "NEEDS.defines.{name}: a define's name is letters, digits and _ (read as DEFINES.{name})"
            );
            ensure!(
                !name.starts_with("RECIPE_"),
                "NEEDS.defines.{name}: names starting with RECIPE_ are the engine's own"
            );
            let default = match default {
                serde_json::Value::Null => None,
                serde_json::Value::String(text) => Some(text),
                _ => bail!(
                    "NEEDS.defines.{name} is its default as a string, or () when the caller must give it"
                ),
            };
            defines.insert(name, default);
        }
        ensure!(defines.len() <= 64, "NEEDS.defines exceeds 64 names");
        for name in raw.outputs.keys() {
            validate_name(name, "outputs")?;
        }
        ensure!(raw.outputs.len() <= 64, "NEEDS.outputs exceeds 64 names");
        ensure!(inputs.len() <= 64, "NEEDS.inputs exceeds 64 names");
        ensure!(
            raw.network.len() <= crate::domain::MAX_NETWORK_ENDPOINTS,
            "NEEDS.network exceeds endpoint bound"
        );
        let mut network = BTreeSet::new();
        for endpoint in raw.network {
            let endpoint = canonical_endpoint(&endpoint)?;
            ensure!(
                network.insert(endpoint),
                "NEEDS.network has a duplicate endpoint"
            );
        }
        let runtime = match raw.runtime.as_deref() {
            None => crate::runtime::Image::Full,
            Some("minimal") => crate::runtime::Image::Minimal,
            Some(other) => bail!(
                "NEEDS.runtime is \"minimal\" (the small image of a shell, coreutils, findutils, sed and grep) or left out (the full compiler image), not {other:?}"
            ),
        };
        let needs = Self {
            inputs,
            defines,
            outputs: raw.outputs,
            network: network.into_iter().collect(),
            memory_mib: raw.memory_mib.unwrap_or(defaults.memory_mib),
            storage_max_objects: raw
                .storage_max_objects
                .unwrap_or(defaults.storage_max_objects),
            storage_max_bytes: raw.storage_max_bytes,
            timeout_secs: raw.timeout_secs.unwrap_or(defaults.timeout_secs),
            network_max_bytes: raw.network_max_bytes.unwrap_or(defaults.network_max_bytes),
            runtime,
            pins: BTreeMap::new(),
            source_files: BTreeMap::new(),
        };
        needs.resolve(&Narrowing::default(), &defaults)?;
        Ok(needs)
    }

    /// The network byte cap of a run on a system whose C library is
    /// `host_libc`: [`download_cap`] of the declared bytes plus the pins in
    /// use (the section of that C library, else the `any` section, as
    /// `crate::pins::Pins::section` chooses it), in the `SOURCES` entries
    /// fetched there plus those pins. Declared bytes without a `SOURCES`
    /// entry (a recipe that fetches with the download helpers) count as one
    /// file. A group without pins for this system downloads nothing and its
    /// download step refuses the run; its cap stays one byte so the network
    /// policy is still valid until then.
    pub fn network_budget(&self, host_libc: &str) -> u64 {
        let pins = self
            .pins
            .get(host_libc)
            .or_else(|| self.pins.get("any"))
            .copied()
            .unwrap_or_default();
        let sources = ["any", host_libc]
            .iter()
            .filter_map(|libc| self.source_files.get(*libc))
            .sum::<u64>();
        let declared_files = if self.network_max_bytes > 0 {
            sources.max(1)
        } else {
            sources
        };
        download_cap(Payload {
            bytes: self.network_max_bytes.saturating_add(pins.bytes),
            files: declared_files.saturating_add(pins.files),
        })
        .max(1)
    }

    /// Resolve caller restrictions once; the same values govern config, worker,
    /// retention identity, deadlines and publication bounds.
    pub fn resolve(
        &self,
        narrowing: &Narrowing,
        execution: &crate::ExecutionOptions,
    ) -> Result<crate::ExecutionOptions> {
        fn lower(name: &str, requested: Option<u64>, declared: u64) -> Result<u64> {
            let value = requested.unwrap_or(declared);
            ensure!(
                value <= declared,
                "caller cannot widen NEEDS.{name}: requested {value}, declared {declared}; edit NEEDS.{name}"
            );
            Ok(value)
        }
        let mut resolved = execution.clone();
        resolved.memory_mib = lower("memory_mib", narrowing.memory_mib, self.memory_mib)?;
        resolved.storage_max_objects = lower(
            "storage_max_objects",
            narrowing.storage_max_objects,
            self.storage_max_objects,
        )?;
        resolved.timeout_secs = lower("timeout_secs", narrowing.timeout_secs, self.timeout_secs)?;
        resolved.network_max_bytes = lower(
            "network_max_bytes",
            narrowing.network_max_bytes,
            self.network_budget(&execution.host_libc),
        )?;
        let memory_bytes = self
            .memory_mib
            .checked_mul(1024 * 1024)
            .context("NEEDS.memory_mib overflows storage bound")?;
        let declared_bytes = self.storage_max_bytes.unwrap_or(memory_bytes);
        let effective_memory = resolved
            .memory_mib
            .checked_mul(1024 * 1024)
            .context("caller memory bound overflow")?;
        resolved.storage_max_bytes = narrowing.storage_max_bytes.or_else(|| {
            self.storage_max_bytes
                .map(|bytes| bytes.min(effective_memory))
        });
        if let Some(bytes) = resolved.storage_max_bytes {
            lower("storage_max_bytes", Some(bytes), declared_bytes)?;
        }
        resolved.network_hosts = if let Some(requested) = &narrowing.network {
            let mut endpoints = BTreeSet::new();
            for endpoint in requested {
                let endpoint = canonical_endpoint(endpoint)?;
                ensure!(
                    self.network.contains(&endpoint),
                    "caller cannot widen NEEDS.network with {endpoint}; edit NEEDS.network"
                );
                ensure!(
                    endpoints.insert(endpoint),
                    "caller network subset has a duplicate endpoint"
                );
            }
            endpoints.into_iter().collect()
        } else {
            self.network.clone()
        };
        resolved.validate()?;
        Ok(resolved)
    }

    /// Apply a caller ceiling to this dependency's own declaration. A ceiling
    /// above a dependency's limit keeps that stricter limit; endpoint sets
    /// intersect, so a caller never lends another recipe an endpoint.
    pub(crate) fn dependency_narrowing(
        &self,
        caller: &Narrowing,
        host_libc: &str,
    ) -> Result<Narrowing> {
        let cap = |requested: Option<u64>, declared: u64| requested.map(|n| n.min(declared));
        let memory_mib = cap(caller.memory_mib, self.memory_mib);
        let memory_bytes = memory_mib
            .unwrap_or(self.memory_mib)
            .checked_mul(1024 * 1024)
            .context("dependency memory bound overflow")?;
        let declared_bytes = self
            .storage_max_bytes
            .unwrap_or(self.memory_mib * 1024 * 1024);
        let network = caller
            .network
            .as_ref()
            .map(|requested| {
                requested
                    .iter()
                    .map(|value| canonical_endpoint(value))
                    .collect::<Result<BTreeSet<_>>>()
                    .map(|allowed| {
                        self.network
                            .iter()
                            .filter(|endpoint| allowed.contains(*endpoint))
                            .cloned()
                            .collect()
                    })
            })
            .transpose()?;
        Ok(Narrowing {
            network,
            memory_mib,
            storage_max_objects: cap(caller.storage_max_objects, self.storage_max_objects),
            storage_max_bytes: caller
                .storage_max_bytes
                .map(|n| n.min(declared_bytes).min(memory_bytes)),
            timeout_secs: cap(caller.timeout_secs, self.timeout_secs),
            network_max_bytes: cap(caller.network_max_bytes, self.network_budget(host_libc)),
        })
    }

    /// With `require`, every input that is not optional must be supplied;
    /// `latest()` sees no inputs, so its run does not require them.
    pub fn validate_inputs(&self, supplied: &[(String, PathBuf)], require: bool) -> Result<()> {
        let mut names = BTreeSet::new();
        for (name, path) in supplied {
            ensure!(names.insert(name), "duplicate input {name}");
            let need = self
                .inputs
                .get(name)
                .with_context(|| format!("undeclared input {name}; edit NEEDS.inputs"))?;
            let metadata = std::fs::symlink_metadata(path).with_context(|| {
                format!("NEEDS.inputs.{name}: input is missing or inaccessible")
            })?;
            ensure!(
                !metadata.file_type().is_symlink(),
                "NEEDS.inputs.{name} must not be a symlink (requires {})",
                need.kind.as_str()
            );
            ensure!(
                need.kind.matches(&metadata),
                "NEEDS.inputs.{name} requires {}",
                need.kind.as_str()
            );
        }
        for (name, need) in &self.inputs {
            ensure!(
                !require || need.optional || names.contains(name),
                "missing required NEEDS.inputs.{name} ({})",
                need.kind.as_str()
            );
        }
        Ok(())
    }
    /// The outputs a worker result names (each to its domain path): only
    /// declared names, each at its engine-placed path
    /// (`crate::domain::output_path`), and with `complete` every declared
    /// one (docs/recipe-format.md, sections 3.1 and 3.2).
    pub(crate) fn validate_output_names(
        &self,
        outputs: &BTreeMap<String, String>,
        complete: bool,
    ) -> Result<()> {
        for (name, path) in outputs {
            ensure!(
                self.outputs.contains_key(name),
                "the worker named an undeclared output {name}"
            );
            ensure!(
                *path == crate::domain::output_path(name),
                "the worker placed NEEDS.outputs.{name} at {path}, not where the engine places it"
            );
            crate::domain::validate_export_path(path.trim_start_matches('/'))?;
        }
        if complete {
            for name in self.outputs.keys() {
                ensure!(
                    outputs.contains_key(name),
                    "missing required NEEDS.outputs.{name}"
                );
            }
        }
        Ok(())
    }
}

fn validate_name(name: &str, field: &str) -> Result<()> {
    ensure!(
        crate::domain::valid_name(name),
        "invalid NEEDS.{field} name {name:?}"
    );
    Ok(())
}

/// Validate and canonicalize an endpoint without DNS, network access or setup.
fn canonical_endpoint(value: &str) -> Result<String> {
    if let Ok(address) = value.parse::<std::net::SocketAddr>() {
        ensure!(
            address.port() != 0
                && !address.ip().is_unspecified()
                && !address.ip().is_multicast()
                && address.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::BROADCAST),
            "invalid NEEDS.network endpoint {value}"
        );
        return Ok(address.to_string());
    }
    let (host, port) = value
        .rsplit_once(':')
        .context("NEEDS.network endpoint requires HOST:PORT")?;
    let port: u16 = port.parse().context("invalid NEEDS.network port")?;
    ensure!(
        port != 0
            && host.len() <= 253
            && !host.is_empty()
            && host.split('.').all(|label| !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')),
        "invalid NEEDS.network endpoint {value}"
    );
    Ok(format!("{}:{port}", host.to_ascii_lowercase()))
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub recipe: PathBuf,
    pub build_only: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Declaration {
    pub recipe: PathBuf,
    /// Every file it is compiled from, each by SHA-256 as read once with
    /// the recipe: the recipe, its pins file, the base it extends and the
    /// modules it, its base or a module imports.
    pub source_digests: BTreeMap<PathBuf, String>,
    pub needs: Needs,
    pub dependencies: Vec<Dependency>,
    /// Everything else it declares (docs/recipe-format.md, section 2).
    pub declared: crate::declarations::Declared,
}

impl Declaration {
    pub fn name(&self) -> &str {
        &self.declared.package.name
    }
    pub fn version(&self) -> Option<&str> {
        self.declared.package.version.as_deref()
    }

    /// The recipe's code wherever its folder lies: each file it is compiled
    /// from (`source_digests`: the recipe, its pins, the base it extends, the
    /// modules it imports) by its name relative to the recipe's folder, to
    /// its SHA-256 as read with the recipe. The same recipe on the install
    /// medium, in a release's recipes and in a working copy has the same
    /// content; this, never `source_digests` (keyed by where the files lie),
    /// is what names a result.
    pub fn content(&self) -> Result<BTreeMap<String, String>> {
        let folder = self.recipe.parent().unwrap_or(Path::new("/"));
        let name = |path: &Path| -> String {
            match path.strip_prefix(folder) {
                Ok(relative) => relative.to_string_lossy().into_owned(),
                // A base or module found on the search path, outside the
                // folder: its file name; the recipe names it already.
                Err(_) => path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            }
        };
        Ok(self
            .source_digests
            .iter()
            .map(|(path, digest)| (name(path), digest.clone()))
            .collect())
    }

    /// What names the recipe that produced a package (a receipt's
    /// `recipe_digest`): the SHA-256 of its [`Declaration::content`], so the
    /// medium's copy and a release's copy of the same recipe match.
    pub fn producer_digest(&self) -> Result<String> {
        use sha2::Digest;
        Ok(hex::encode(sha2::Sha256::digest(serde_json::to_vec(
            &self.content()?,
        )?)))
    }
}

/// Inspect source declarations only, including the same base/child code the
/// worker compiles. Every executable child owns its explicit NEEDS declaration;
/// the other declarations combine whole between base and child.
///
/// The recipe's name is its file name without `.rhai`. This is the one place
/// a recipe's name is taken from its path; the worker, whose recipe is staged
/// under another name, calls [`declaration_named`] with the name the
/// coordinator read here.
pub fn declaration(recipe: &Path, search_path: Option<&Path>) -> Result<Declaration> {
    let name = recipe
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .context("the recipe's file name is not UTF-8")?
        .to_owned();
    declaration_named(recipe, &name, search_path)
}

/// [`declaration`] of the recipe file `recipe` whose name is `name`.
pub(crate) fn declaration_named(
    recipe: &Path,
    name: &str,
    search_path: Option<&Path>,
) -> Result<Declaration> {
    ensure!(
        !std::fs::symlink_metadata(recipe)?.file_type().is_symlink(),
        "recipe must not be a symlink"
    );
    let recipe = recipe
        .canonicalize()
        .context("cannot resolve recipe declaration")?;
    ensure!(
        std::fs::symlink_metadata(&recipe)?.is_file(),
        "recipe must be a regular file"
    );
    let engine = crate::core::invocation::compile_engine();
    let compiled = crate::core::executor::compile_recipe(&engine, &recipe, search_path)?;
    crate::declarations::check_form(&compiled)?;
    let ast = &compiled.source_ast;
    let mut needs = None;
    let mut budget_declared = false;
    for statement in ast.statements() {
        if let Stmt::Var(binding, flags, _) = statement
            && binding.0.name == "NEEDS"
        {
            ensure!(needs.is_none(), "recipe must declare NEEDS exactly once");
            ensure!(
                flags.contains(ASTFlags::CONSTANT),
                "NEEDS must be const and literal"
            );
            ensure!(
                matches!(&binding.1, Expr::Map(..)),
                "NEEDS must be a literal map"
            );
            let value = literal(&binding.1)?;
            budget_declared = value.get("network_max_bytes").is_some();
            needs = Some(Needs::parse(value)?);
        }
    }
    let mut needs = needs.context("recipe must declare literal const NEEDS = #{...}")?;
    let declared = crate::declarations::read(&compiled, name, &needs)?;
    // A group declares only the budget of its other sources (0 when it has
    // none); Recipe adds the bytes of the pins in use (decision 9).
    if let Some(pins) = &declared.pins {
        if !budget_declared {
            needs.network_max_bytes = 0;
        }
        needs.pins = pins
            .sections
            .iter()
            .map(|section| {
                (
                    section.libc.as_str().to_owned(),
                    Payload {
                        bytes: section.bytes(),
                        files: section.packages.len() as u64,
                    },
                )
            })
            .collect();
    }
    for source in declared.sources.values() {
        let libc = source.libc.map_or("any", |libc| libc.as_str());
        *needs.source_files.entry(libc.to_owned()).or_default() += 1;
    }
    let mut dependencies = BTreeMap::<String, Vec<String>>::new();
    for statement in compiled.ast.statements() {
        if let Stmt::Var(binding, _, _) = statement
            && crate::recipes::DEPENDENCY_FIELDS.contains(&binding.0.name.as_str())
        {
            let values =
                literal(&binding.1).context("dependency declarations must be literal arrays")?;
            ensure!(
                values.is_array(),
                "dependency declarations must be literal arrays"
            );
            let values: Vec<String> = serde_json::from_value(values)
                .context("dependency names must be literal strings")?;
            dependencies.insert(binding.0.name.to_string(), values);
        }
    }
    let mut resolved = Vec::new();
    for (field, names) in dependencies {
        for name in names {
            let file = crate::recipes::dependency_file(&name).map_err(anyhow::Error::msg)?;
            let path = crate::recipes::find_related(&file, &recipe, None)
                .or_else(|| {
                    compiled
                        .base_path
                        .as_ref()
                        .and_then(|base| crate::recipes::find_related(&file, base, None))
                })
                .or_else(|| crate::recipes::find_related(&file, &recipe, search_path))
                .with_context(|| format!("dependency recipe {name:?} not found"))?;
            ensure!(
                std::fs::symlink_metadata(&path)?.is_file(),
                "dependency recipe {name} must be a regular file, not a symlink"
            );
            resolved.push(Dependency {
                name,
                recipe: path.canonicalize()?,
                build_only: field == "build_deps",
            });
        }
    }
    Ok(Declaration {
        recipe,
        source_digests: compiled.source_digests,
        needs,
        dependencies: resolved,
        declared,
    })
}

pub(crate) fn literal(expression: &Expr) -> Result<serde_json::Value> {
    use serde_json::Value;
    Ok(match expression {
        Expr::StringConstant(value, _) => Value::String(value.to_string()),
        Expr::IntegerConstant(value, _) => Value::from(*value),
        Expr::BoolConstant(value, _) => Value::Bool(*value),
        Expr::Unit(_) => Value::Null,
        Expr::Array(values, _) => Value::Array(values.iter().map(literal).collect::<Result<_>>()?),
        Expr::Map(values, _) => {
            let mut map = serde_json::Map::new();
            for (key, value) in &values.0 {
                ensure!(
                    map.insert(key.name.to_string(), literal(value)?).is_none(),
                    "duplicate literal field {}",
                    key.name
                );
            }
            Value::Object(map)
        }
        _ => bail!(
            "declarations must be literal data; computed expressions, variables and helper calls are not read"
        ),
    })
}

/// Stable dependency postorder, with full cycle/depth checks before any launch.
pub fn declarations(recipe: &Path, search_path: Option<&Path>) -> Result<Vec<Declaration>> {
    fn visit(
        recipe: &Path,
        search: Option<&Path>,
        active: &mut Vec<PathBuf>,
        done: &mut BTreeSet<PathBuf>,
        result: &mut Vec<Declaration>,
    ) -> Result<()> {
        let node = declaration(recipe, search)?;
        ensure!(
            !active.contains(&node.recipe),
            "Circular recipe dependency detected at {}",
            node.recipe.display()
        );
        ensure!(
            active.len() < crate::recipes::MAX_DEPTH,
            "recipe dependency depth exceeded"
        );
        if done.contains(&node.recipe) {
            return Ok(());
        }
        active.push(node.recipe.clone());
        for dependency in &node.dependencies {
            visit(&dependency.recipe, search, active, done, result)?;
        }
        active.pop();
        done.insert(node.recipe.clone());
        result.push(node);
        Ok(())
    }
    let mut result = Vec::new();
    visit(
        recipe,
        search_path,
        &mut Vec::new(),
        &mut BTreeSet::new(),
        &mut result,
    )?;
    // Result bindings use DEP/OUTPUT. An alias may not descend into another
    // dependency's product, whose tree can legitimately contain rootfs links.
    for node in &result {
        for dependency in &node.dependencies {
            let producer = result
                .iter()
                .find(|candidate| candidate.recipe == dependency.recipe)
                .unwrap();
            for output in producer.needs.outputs.keys() {
                let product = format!("{}/{output}", dependency.name);
                for other in &node.dependencies {
                    ensure!(
                        other.name != product && !other.name.starts_with(&format!("{product}/")),
                        "dependency result binding collision at {product}; rename the dependency alias or output"
                    );
                }
            }
        }
    }
    Ok(result)
}

/// The defines a caller gives (`--define NAME=VALUE`) against a graph: each
/// reaches every recipe that declares it in `NEEDS.defines`; one no recipe
/// declares is refused, and a required define (default `()`) of any recipe
/// must be given (decision 1).
pub fn check_defines(graph: &[Declaration], given: &[(String, String)]) -> Result<()> {
    check_declared_defines(graph, given)?;
    for node in graph {
        for (name, default) in &node.needs.defines {
            ensure!(
                default.is_some() || given.iter().any(|(given, _)| given == name),
                "{} needs --define {name}=VALUE (NEEDS.defines.{name} has no default)",
                node.name()
            );
        }
    }
    Ok(())
}

/// The first half of [`check_defines`]: every `--define` is declared by a
/// recipe of the run. `latest()` needs no more: a required define it is not
/// given is `()` there.
pub fn check_declared_defines(graph: &[Declaration], given: &[(String, String)]) -> Result<()> {
    for (name, _) in given {
        if name.starts_with("RECIPE_") {
            continue; // the engine's own (RECIPE_SYSTEM_CONTEXT, RECIPE_SYSTEM_PRESENT)
        }
        ensure!(
            graph
                .iter()
                .any(|node| node.needs.defines.contains_key(name)),
            "--define {name}: no recipe of this run declares it in NEEDS.defines"
        );
    }
    Ok(())
}
