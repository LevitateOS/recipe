//! What the coordinator reports about declared authority: the `validate`,
//! `inspect`, `diff` and `explain` commands, and the endpoint use a finished
//! run recorded. Nothing here evaluates recipe code.
use super::{Narrowing, contract, declaration, declarations};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
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
    crate::inputs::bind_inputs(&graph, &supplied, search, true)?;
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
    let ginux = crate::ginux::Ginux::from_env()?;
    let mut dependency_contracts = Vec::new();
    for node in nodes.iter().take(nodes.len() - 1) {
        let effective = node.needs.resolve(
            &node
                .needs
                .dependency_narrowing(narrowing, &crate::ExecutionOptions::default().host_libc)?,
            &crate::ExecutionOptions::default(),
        )?;
        let receipt = contract::inspect(&ginux, &effective)?;
        dependency_contracts.push(serde_json::json!({"declaration":node,"contract":receipt,
            "effective":limits(&effective)}));
    }
    let contract = contract::inspect(&ginux, &execution)?;
    Ok(
        serde_json::json!({"recipe":main.recipe,"api":crate::declarations::API,"needs":main.needs,
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
    let ginux = crate::ginux::Ginux::from_env()?;
    let config = contract::inspection_config(&execution);
    let receipt = contract::validate(&ginux, &config, &execution)?;
    let operation = contract::operation(operation);
    let text = contract::command(&ginux, &config, Some(operation))?;
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
/// of today's recipe source or Ginux. The runtime's audit lines
/// (`GINUX_NATIVE_AUDIT: … operation=OP policy-source=S policy-rule=N …`) are
/// in the supervisor log copy; absence and truncation are explicit.
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
        if !line.starts_with("GINUX_NATIVE_AUDIT: ") {
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
        let recipe_rule = receipt.contract.provenance(source, index, operation);
        decisions.push(serde_json::json!({"audit":line,"recipe_rule":recipe_rule}));
    }
    Ok(
        serde_json::json!({"invocation":result["invocation"],"recipe":authority["declaration"],
        "operation":operation,"contract_digest":receipt.digest,"pinned_endpoints":authority["pinned_endpoints"],"execution":authority["execution"],
        "decisions":decisions,"audit_discarded_bytes":result["log_discarded_bytes"],
        "network_usage":result["network_usage"],
        "phase":"Ginux entry is recorded; recipe phase is not correlated across diagnostic streams",
        "limitation":"No matching audit means unobserved, never an implicit allow or successful connection; \
         the runtime records no destination address, so a program's connection to an undeclared \
         address on a granted port is not reported"}),
    )
}

/// Endpoint use of a Ginux run whose worker reported none. A run without a
/// network grant has no endpoint to account for and is complete; a run with
/// one always has a worker report ([`host_network_usage`]), since the worker
/// counts Recipe's own downloads there too, so missing use is unobserved.
pub(crate) fn network_usage(host_endpoints: &BTreeMap<String, Vec<String>>) -> serde_json::Value {
    if host_endpoints.is_empty() {
        return usage_report(host_endpoints, |_, _| (false, false));
    }
    serde_json::json!({"status":"unobserved","reason":"the worker reported no network use","warnings":[],
        "declared":host_endpoints.keys().collect::<Vec<_>>() })
}

/// The report from the worker's own account of each declared endpoint
/// (`crate::helpers::acquire::net`), in a host run and a Ginux run alike.
/// It covers Recipe's own downloads, not programs a recipe starts.
pub(crate) fn host_network_usage(
    host_endpoints: &BTreeMap<String, Vec<String>>,
    usage: &BTreeMap<String, crate::helpers::acquire::net::EndpointUse>,
) -> serde_json::Value {
    usage_report(host_endpoints, |declared, _| {
        let key = declared
            .parse::<std::net::SocketAddr>()
            .map(|address| address.to_string())
            .unwrap_or_else(|_| declared.to_ascii_lowercase());
        let seen = usage.get(&key).copied().unwrap_or_default();
        (seen.attempted, seen.connected)
    })
}

/// `network_usage` for a complete account: per declared endpoint, whether it
/// was attempted and whether it connected; unused endpoints are warnings.
fn usage_report(
    host_endpoints: &BTreeMap<String, Vec<String>>,
    seen: impl Fn(&str, &Vec<String>) -> (bool, bool),
) -> serde_json::Value {
    let mut warnings = Vec::new();
    let endpoints = host_endpoints
        .iter()
        .map(|(declared, pins)| {
            let (attempted, connected) = seen(declared, pins);
            if !attempted {
                warnings.push(format!("unused NEEDS.network endpoint {declared}"));
            }
            serde_json::json!({"declared":declared,"pinned":pins,"attempted":attempted,"connected":connected})
        })
        .collect::<Vec<_>>();
    serde_json::json!({"status":"complete","endpoints":endpoints,"warnings":warnings})
}
