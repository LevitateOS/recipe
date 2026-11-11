//! What the coordinator reports about declared authority: the `validate`,
//! `inspect`, `diff` and `explain` commands, and the endpoint use a finished
//! run recorded. Nothing here evaluates recipe code.
use super::{Narrowing, contract, declaration, declarations};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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
    crate::inputs::bind_inputs(&graph, &supplied, search)?;
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

/// The first connection a retained invocation's supervisor log records as
/// denied: the endpoint as the audit line names it (a numeric address) and the
/// network endpoints the recipe declared. None when no denial was recorded or
/// the record cannot be read.
pub fn refused_connection(directory: &Path) -> Option<serde_json::Value> {
    let recorded = crate::invocation_log::Recorded::read(directory).ok()?;
    let text = String::from_utf8_lossy(&recorded.supervisor_log);
    let endpoint = text.lines().find_map(|line| {
        if !line.starts_with("ginux: audit ") {
            return None;
        }
        let fields = line
            .split_whitespace()
            .filter_map(|part| part.split_once('='))
            .collect::<BTreeMap<_, _>>();
        (fields.get("egress") == Some(&"denied"))
            .then(|| fields.get("network-endpoint").map(|endpoint| (*endpoint).to_owned()))
            .flatten()
    })?;
    let declared = recorded.authority["declaration"]["needs"]["network"].clone();
    Some(serde_json::json!({"endpoint": endpoint, "declared": if declared.is_array() { declared } else { serde_json::json!([]) }}))
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
