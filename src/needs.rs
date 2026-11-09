//! Statically declared recipe authority. Reading it never evaluates recipe code.
pub mod contract;
pub mod report;
use anyhow::{Context, Result, bail, ensure};
use rhai::{ASTFlags, Expr, OptimizationLevel, Stmt};
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
    pub outputs: BTreeMap<String, PathKind>,
    pub network: Vec<String>,
    pub memory_mib: u64,
    pub storage_max_objects: u64,
    pub storage_max_bytes: Option<u64>,
    pub timeout_secs: u64,
    pub network_max_bytes: u64,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LiteralNeeds {
    inputs: BTreeMap<String, String>,
    outputs: BTreeMap<String, PathKind>,
    network: Vec<String>,
    memory_mib: Option<u64>,
    storage_max_objects: Option<u64>,
    storage_max_bytes: Option<u64>,
    timeout_secs: Option<u64>,
    network_max_bytes: Option<u64>,
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
        let needs = Self {
            inputs,
            outputs: raw.outputs,
            network: network.into_iter().collect(),
            memory_mib: raw.memory_mib.unwrap_or(defaults.memory_mib),
            storage_max_objects: raw
                .storage_max_objects
                .unwrap_or(defaults.storage_max_objects),
            storage_max_bytes: raw.storage_max_bytes,
            timeout_secs: raw.timeout_secs.unwrap_or(defaults.timeout_secs),
            network_max_bytes: raw.network_max_bytes.unwrap_or(defaults.network_max_bytes),
        };
        needs.resolve(&Narrowing::default(), &defaults)?;
        Ok(needs)
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
            self.network_max_bytes,
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
    pub(crate) fn dependency_narrowing(&self, caller: &Narrowing) -> Result<Narrowing> {
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
            network_max_bytes: cap(caller.network_max_bytes, self.network_max_bytes),
        })
    }

    pub fn validate_inputs(&self, supplied: &[(String, PathBuf)]) -> Result<()> {
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
                need.optional || names.contains(name),
                "missing required NEEDS.inputs.{name} ({})",
                need.kind.as_str()
            );
        }
        Ok(())
    }
    pub(crate) fn validate_output_names(
        &self,
        context: &serde_json::Value,
        complete: bool,
    ) -> Result<()> {
        let outputs = context
            .get("outputs")
            .and_then(serde_json::Value::as_object)
            .context("ctx.outputs must be an object")?;
        for (name, path) in outputs {
            ensure!(
                self.outputs.contains_key(name),
                "undeclared ctx.outputs.{name}; edit NEEDS.outputs"
            );
            let path = path.as_str().context("ctx.outputs paths must be strings")?;
            let relative = path
                .strip_prefix('/')
                .context("ctx.outputs must use absolute domain paths")?;
            crate::domain::validate_export_path(relative)?;
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

    pub(crate) fn validate_worker_outputs(
        &self,
        context: &rhai::Map,
        complete: bool,
    ) -> Result<()> {
        let outputs: serde_json::Value = rhai::serde::from_dynamic(
            context
                .get("outputs")
                .context("ctx.outputs must be a map")?,
        )?;
        let value = serde_json::json!({"outputs": outputs});
        self.validate_output_names(&value, complete)?;
        for (name, path) in value["outputs"].as_object().unwrap() {
            let selected = Path::new(path.as_str().unwrap());
            for ancestor in selected.ancestors().skip(1) {
                ensure!(
                    std::fs::symlink_metadata(ancestor)?.is_dir(),
                    "NEEDS.outputs.{name} traverses a non-directory ancestor"
                );
            }
            let metadata = std::fs::symlink_metadata(path.as_str().unwrap())
                .with_context(|| format!("NEEDS.outputs.{name} is absent"))?;
            ensure!(
                self.outputs[name].matches(&metadata),
                "NEEDS.outputs.{name} requires {}",
                self.outputs[name].as_str()
            );
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
    pub api: u8,
    pub source_digests: BTreeMap<PathBuf, String>,
    pub needs: Needs,
    pub dependencies: Vec<Dependency>,
}

/// Inspect source declarations only, including the same base/child code the
/// worker compiles. Every executable child owns its explicit NEEDS declaration.
pub fn declaration(recipe: &Path, search_path: Option<&Path>) -> Result<Declaration> {
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
    let mut engine = rhai::Engine::new();
    engine.set_optimization_level(OptimizationLevel::None);
    let compiled = crate::core::executor::compile_recipe(&engine, &recipe, search_path)?;
    crate::core::invocation::validate_api(&compiled.ast)?;
    let ast = &compiled.source_ast;
    let mut needs = None;
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
            needs = Some(Needs::parse(literal(&binding.1)?)?);
        }
    }
    let needs = needs.context("recipe must declare literal const NEEDS = #{...}")?;
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
        api: 2,
        source_digests: compiled.source_digests,
        needs,
        dependencies: resolved,
    })
}

fn literal(expression: &Expr) -> Result<serde_json::Value> {
    use serde_json::Value;
    Ok(match expression {
        Expr::StringConstant(value, _) => Value::String(value.to_string()),
        Expr::IntegerConstant(value, _) => Value::from(*value),
        Expr::BoolConstant(value, _) => Value::Bool(*value),
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
            "NEEDS and dependency declarations must be literal; computed expressions are not authority"
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
