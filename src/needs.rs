//! Statically declared recipe authority. Reading it never evaluates recipe code.
pub mod contract;
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
            crate::inputs::validate_input_name(&name)?;
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
            crate::publication::validate_path(relative)?;
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

/// Bind main inputs as NAME=PATH and dependency inputs as DEP::NAME=PATH.
/// A dependency alias must identify one recipe across the complete graph.
/// No input is implicitly lent to another recipe, even when names match.
pub(crate) fn bind_inputs(
    graph: &[Declaration],
    supplied: &[crate::inputs::ExtraInput],
    search: Option<&Path>,
) -> Result<BTreeMap<PathBuf, Vec<crate::inputs::ExtraInput>>> {
    let main = graph.last().context("empty recipe graph")?;
    let mut aliases = BTreeMap::<String, BTreeSet<PathBuf>>::new();
    for node in graph {
        for dependency in &node.dependencies {
            aliases
                .entry(dependency.name.clone())
                .or_default()
                .insert(dependency.recipe.clone());
        }
    }
    let mut bindings: BTreeMap<PathBuf, Vec<crate::inputs::ExtraInput>> = graph
        .iter()
        .map(|node| (node.recipe.clone(), Vec::new()))
        .collect();
    for input in supplied {
        let (recipe, name) = if let Some((alias, name)) = input.name.split_once("::") {
            let recipes = aliases
                .get(alias)
                .with_context(|| format!("unknown dependency input scope {alias}"))?;
            ensure!(
                recipes.len() == 1,
                "ambiguous dependency alias {alias}; use distinct dependency names"
            );
            (recipes.first().unwrap(), name)
        } else {
            (&main.recipe, input.name.as_str())
        };
        bindings
            .get_mut(recipe)
            .unwrap()
            .push(crate::inputs::ExtraInput {
                name: name.into(),
                path: input.path.clone(),
            });
    }
    for node in graph {
        node.needs
            .validate_inputs(
                &bindings[&node.recipe]
                    .iter()
                    .map(|input| (input.name.clone(), input.path.clone()))
                    .collect::<Vec<_>>(),
            )
            .with_context(|| format!("inputs for {}", node.recipe.display()))?;
        let discovered =
            crate::inputs::discover_inputs(&node.recipe, search, &bindings[&node.recipe])
                .with_context(|| format!("input/source closure for {}", node.recipe.display()))?;
        for (source, digest) in &node.source_digests {
            ensure!(
                discovered
                    .inputs
                    .iter()
                    .any(
                        |input| input.host_path.canonicalize().ok().as_ref() == Some(source)
                            && hex::encode(input.sha256) == *digest
                    ),
                "recipe authority changed during graph preflight: {}",
                source.display()
            );
        }
    }
    Ok(bindings)
}

/// Full declaration/input/contract preflight without executing setup or a workload.
pub fn validate(
    recipe: &Path,
    search: Option<&Path>,
    supplied: &[(String, PathBuf)],
    narrowing: &Narrowing,
) -> Result<serde_json::Value> {
    let graph = declarations(recipe, search)?;
    let supplied = supplied
        .iter()
        .map(|(name, path)| crate::inputs::ExtraInput {
            name: name.clone(),
            path: path.clone(),
        })
        .collect::<Vec<_>>();
    bind_inputs(&graph, &supplied, search)?;
    inspect(recipe, search, narrowing)
}

/// The resolved limits an inspection reports for one recipe.
fn limits(effective: &crate::ExecutionOptions) -> serde_json::Value {
    serde_json::json!({"memory_mib":effective.memory_mib,"storage_max_objects":effective.storage_max_objects,"storage_max_bytes":effective.storage_max_bytes,"timeout_secs":effective.timeout_secs,"network":effective.network_hosts,"network_max_bytes":effective.network_max_bytes})
}

/// Resolve authority for inspection, without evaluating the recipe or starting
/// a workload. Ginux expands the exact same version-pinned contract as launch.
pub fn inspect(
    recipe: &Path,
    search_path: Option<&Path>,
    narrowing: &Narrowing,
) -> Result<serde_json::Value> {
    let nodes = declarations(recipe, search_path)?;
    let main = nodes.last().context("empty recipe graph")?;
    let execution = main
        .needs
        .resolve(narrowing, &crate::ExecutionOptions::default())?;
    let launcher = crate::GinuxLauncher::from_env()?;
    let mut dependency_contracts = Vec::new();
    for node in nodes.iter().take(nodes.len() - 1) {
        let effective = node.needs.resolve(
            &node.needs.dependency_narrowing(narrowing)?,
            &crate::ExecutionOptions::default(),
        )?;
        let receipt = contract::inspect(launcher.path(), &effective)?;
        dependency_contracts.push(serde_json::json!({"declaration":node,"contract":receipt,
            "effective":limits(&effective)}));
    }
    let contract = contract::inspect(launcher.path(), &execution)?;
    Ok(
        serde_json::json!({"recipe":main.recipe,"api":main.api,"needs":main.needs,
        "effective":limits(&execution),
        "contract":contract,"dependencies":dependency_contracts,"dependency_bindings":main.dependencies,"source_digests":main.source_digests,
        "network_usage":{"status":"unobserved","reason":"declaration inspection does not launch","declared":main.needs.network,"warnings":[]}}),
    )
}

/// Compare authority, not hook text or evaluation results.
pub fn diff(old: &Path, new: &Path, search: Option<&Path>) -> Result<serde_json::Value> {
    let old = inspect(old, search, &Narrowing::default())?;
    let new = inspect(new, search, &Narrowing::default())?;
    let mut changes = serde_json::Map::new();
    for field in ["inputs", "outputs"] {
        let before = old["needs"][field]
            .as_object()
            .context("invalid old declaration")?;
        let after = new["needs"][field]
            .as_object()
            .context("invalid new declaration")?;
        let added = after
            .iter()
            .filter(|(name, _)| !before.contains_key(*name))
            .collect::<BTreeMap<_, _>>();
        let removed = before
            .iter()
            .filter(|(name, _)| !after.contains_key(*name))
            .collect::<BTreeMap<_, _>>();
        let changed = after
            .iter()
            .filter_map(|(name, value)| {
                before
                    .get(name)
                    .filter(|prior| *prior != value)
                    .map(|prior| (name, serde_json::json!({"old":prior,"new":value})))
            })
            .collect::<BTreeMap<_, _>>();
        changes.insert(
            field.into(),
            serde_json::json!({"added":added,"removed":removed,"changed":changed}),
        );
    }
    let endpoints =
        |value: &serde_json::Value| value["needs"]["network"].as_array().unwrap().to_vec();
    let before = endpoints(&old);
    let after = endpoints(&new);
    changes.insert("network".into(), serde_json::json!({"added":after.iter().filter(|value| !before.contains(value)).collect::<Vec<_>>(),
        "removed":before.iter().filter(|value| !after.contains(value)).collect::<Vec<_>>() }));
    for field in [
        "memory_mib",
        "storage_max_objects",
        "storage_max_bytes",
        "timeout_secs",
        "network_max_bytes",
    ] {
        let effective = |value: &serde_json::Value| {
            value["needs"][field]
                .as_u64()
                .unwrap_or_else(|| value["needs"]["memory_mib"].as_u64().unwrap() * 1024 * 1024)
        };
        let before = effective(&old);
        let after = effective(&new);
        if old["needs"][field] != new["needs"][field] || before != after {
            changes.insert(field.into(), serde_json::json!({"old":old["needs"][field],"new":new["needs"][field],
                "upper_bound_old":before,"upper_bound_new":after,"change":if after > before {"raised"} else if after < before {"lowered"} else {"same bound"}}));
        }
    }
    let dependency_authority = |inspection: &serde_json::Value| -> Result<serde_json::Value> {
        let recipe = PathBuf::from(
            inspection["recipe"]
                .as_str()
                .context("recipe identity missing")?,
        );
        let parent = recipe.parent().context("recipe parent missing")?;
        let relative = |value: &serde_json::Value| -> serde_json::Value {
            let path = Path::new(value.as_str().unwrap_or(""));
            serde_json::json!(path.strip_prefix(parent).unwrap_or(path))
        };
        let mut values = Vec::new();
        for node in inspection["dependencies"]
            .as_array()
            .context("dependency authority missing")?
        {
            let declaration = &node["declaration"];
            let edges = declaration["dependencies"].as_array().context("dependency edges missing")?.iter().map(|edge|
                serde_json::json!({"name":edge["name"],"recipe":relative(&edge["recipe"]),"build_only":edge["build_only"]})).collect::<Vec<_>>();
            values.push(serde_json::json!({"recipe":relative(&declaration["recipe"]),"needs":declaration["needs"],
                "dependencies":edges,"contract_digest":node["contract"]["digest"]}));
        }
        let direct = inspection["dependency_bindings"].as_array().context("direct dependency bindings missing")?.iter()
            .map(|edge| serde_json::json!({"name":edge["name"],"recipe":relative(&edge["recipe"]),"build_only":edge["build_only"]})).collect::<Vec<_>>();
        Ok(serde_json::json!({"nodes": values, "direct": direct}))
    };
    let old_dependencies = dependency_authority(&old)?;
    let new_dependencies = dependency_authority(&new)?;
    if old_dependencies != new_dependencies {
        changes.insert(
            "dependencies".into(),
            serde_json::json!({"old":old_dependencies,"new":new_dependencies}),
        );
    }
    let authority_changed = changes.values().any(|value| {
        if value.get("added").is_some() {
            ["added", "removed", "changed"]
                .iter()
                .any(|key| match &value[*key] {
                    serde_json::Value::Array(values) => !values.is_empty(),
                    serde_json::Value::Object(values) => !values.is_empty(),
                    _ => false,
                })
        } else {
            true
        }
    }) || old["contract"]["digest"] != new["contract"]["digest"];
    Ok(
        serde_json::json!({"authority_changed":authority_changed,"changes":changes,"old_contract_digest":old["contract"]["digest"],"new_contract_digest":new["contract"]["digest"]}),
    )
}

pub fn explain_recipe(
    recipe: &Path,
    search: Option<&Path>,
    operation: &str,
    narrowing: &Narrowing,
) -> Result<serde_json::Value> {
    let node = declaration(recipe, search)?;
    let execution = node
        .needs
        .resolve(narrowing, &crate::ExecutionOptions::default())?;
    let launcher = crate::GinuxLauncher::from_env()?;
    let config = contract::inspection_config(&execution);
    let receipt = contract::validate(launcher.path(), &config, &execution)?;
    let operation = contract::operation(operation);
    let text = contract::command(
        launcher.path(),
        &config,
        "instance-explain",
        Some(operation),
    )?;
    let rule = text
        .lines()
        .find_map(|line| line.strip_prefix("Rule: "))
        .context("Ginux omitted decision provenance")?;
    let source = rule
        .split_whitespace()
        .find_map(|part| part.strip_prefix("source="))
        .context("Ginux omitted rule source")?;
    let index = rule
        .split_whitespace()
        .find_map(|part| part.strip_prefix("index="))
        .context("Ginux omitted rule index")?
        .parse()?;
    Ok(
        serde_json::json!({"recipe":node.recipe,"operation":operation,"ginux":text,"recipe_rule":receipt.contract.provenance(source,index,operation),"contract_digest":receipt.digest,"execution":execution,"phase":"not observed (declaration only)"}),
    )
}

/// Explain recorded authority/audits from a retained invocation, independently
/// of today's recipe source or launcher. Absence/truncation is explicit.
pub fn explain_invocation(directory: &Path, operation: &str) -> Result<serde_json::Value> {
    let crate::invocation_log::Recorded {
        authority,
        result,
        supervisor_log,
    } = crate::invocation_log::Recorded::read(directory)?;
    let receipt: contract::Receipt = serde_json::from_value(authority["contract"].clone())?;
    let operation = contract::operation(operation);
    let text = String::from_utf8_lossy(&supervisor_log);
    let mut decisions = Vec::new();
    for line in text.lines() {
        if !line.starts_with("ginux: audit ") && !line.starts_with("ginux: denial-summary ") {
            continue;
        }
        let fields = line
            .split_whitespace()
            .filter_map(|part| part.split_once('='))
            .collect::<BTreeMap<_, _>>();
        if fields.get("operation") != Some(&operation) {
            continue;
        }
        let source = fields.get("policy-source").copied().unwrap_or("unknown");
        let index = fields
            .get("policy-rule")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let connect = operation == contract::TCP_CONNECT;
        let recipe_rule = if connect && fields.get("egress") == Some(&"denied") {
            "NEEDS.network/caller subset: decoded numeric endpoint is outside the pinned grant"
                .to_owned()
        } else if connect && fields.get("result") == Some(&"-122") {
            "NEEDS.network_max_bytes/caller byte cap: shared send+receive budget exhausted"
                .to_owned()
        } else if connect && fields.get("result") == Some(&"-24") {
            "Ginux fixed connection/descriptor bound; no NEEDS field widens this limit".to_owned()
        } else {
            receipt.contract.provenance(source, index, operation)
        };
        decisions.push(serde_json::json!({"audit":line,"recipe_rule":recipe_rule}));
    }
    Ok(
        serde_json::json!({"invocation":result["invocation"],"recipe":authority["declaration"],
        "operation":operation,"contract_digest":receipt.digest,"pinned_endpoints":authority["pinned_endpoints"],"execution":authority["execution"],
        "decisions":decisions,"audit_discarded_bytes":result["log_discarded_bytes"],
        "network_usage":result["network_usage"],
        "phase":"Ginux entry/exit is recorded; recipe phase is not correlated across diagnostic streams",
        "limitation":"No matching audit means unobserved, never an implicit allow or successful connection"}),
    )
}

/// Advisory endpoint use from the supervisor's bounded final summary. Missing
/// records are unknown, never false/unused; syscall audit truncation is separate.
pub(crate) fn network_usage(
    host_endpoints: &BTreeMap<String, Vec<String>>,
    diagnostics: &crate::invocation_log::InvocationLog,
    discarded: u64,
) -> serde_json::Value {
    let observed = (|| -> Result<_> {
        let bytes = diagnostics.supervisor_log()?;
        ensure!(discarded == 0, "supervisor log truncated");
        let text = String::from_utf8_lossy(&bytes);
        let mut count = None;
        let mut usage = BTreeMap::new();
        for line in text
            .lines()
            .filter_map(|line| line.strip_prefix("ginux: network-summary "))
        {
            let fields = line
                .split_whitespace()
                .filter_map(|part| part.split_once('='))
                .collect::<BTreeMap<_, _>>();
            if fields.get("version") == Some(&"1") {
                ensure!(count.is_none(), "duplicate network summary");
                count = Some(
                    fields
                        .get("endpoints")
                        .context("summary count missing")?
                        .parse::<usize>()?,
                );
            } else if let Some(endpoint) = fields.get("endpoint") {
                let endpoint = endpoint.parse::<std::net::SocketAddr>()?.to_string();
                let attempted = fields
                    .get("attempted")
                    .context("attempt state missing")?
                    .parse::<bool>()?;
                let connected = fields
                    .get("connected")
                    .context("connection state missing")?
                    .parse::<bool>()?;
                ensure!(
                    usage.insert(endpoint, (attempted, connected)).is_none(),
                    "duplicate endpoint summary"
                );
            }
        }
        let expected = host_endpoints
            .values()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>();
        ensure!(
            count == Some(expected.len())
                && usage.keys().cloned().collect::<BTreeSet<_>>() == expected,
            "complete configured endpoint summary unavailable"
        );
        Ok(usage)
    })();
    match observed {
        Ok(usage) => {
            let mut warnings = Vec::new();
            let endpoints = host_endpoints.iter().map(|(declared, pins)| {
                let attempted = pins.iter().any(|pin| usage[pin].0);
                let connected = pins.iter().any(|pin| usage[pin].1);
                if !attempted { warnings.push(format!("unused NEEDS.network endpoint {declared}")); }
                serde_json::json!({"declared":declared,"pinned":pins,"attempted":attempted,"connected":connected})
            }).collect::<Vec<_>>();
            serde_json::json!({"status":"complete","endpoints":endpoints,"warnings":warnings})
        }
        Err(error) => {
            serde_json::json!({"status":"unobserved","reason":error.to_string(),"warnings":[],
            "declared":host_endpoints.keys().collect::<Vec<_>>() })
        }
    }
}
