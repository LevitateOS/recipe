//! Runs a recipe graph: each recipe in a Ginux instance through the public
//! lifecycle (`crate::ginux`, `crate::launch`), or on the host
//! (`crate::execution`, `crate::host`).
use crate::ginux::Ginux;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Run one executable recipe operation and its dependency graph.
///
/// Each recipe runs where `crate::execution` places it: in Ginux through
/// `explicit` (or the Ginux the environment names, `crate::runtime::ginux_setup`,
/// resolved only when some recipe needs Ginux), or on the host. Prepare immutable inputs, execute,
/// and return the run's result (`crate::launch::finish_outputs`) only after
/// successful completion and validated publication. Persistence retains
/// separate state/files pairs.
///
/// Build and install run the dependencies first, each in its own run (an
/// install: a dependency's outputs are published for its consumer, never
/// activated), and stage their published outputs for their consumer.
/// Download, cleanup and latest run the main recipe alone; latest always on
/// the host (docs/recipe-format.md, section 2.9). What is done already is
/// decided per run from Recipe's records (`crate::launch::start`).
pub(crate) fn run(
    explicit: Option<&Ginux>,
    invocation: &crate::launch::Invocation<'_>,
) -> Result<serde_json::Value> {
    Runner {
        explicit,
        ginux: None,
    }
    .run(invocation)
}

struct Runner<'l> {
    explicit: Option<&'l Ginux>,
    ginux: Option<Ginux>,
}

impl Runner<'_> {
    /// Ginux, resolved the first time a recipe needs it.
    fn ginux(&mut self) -> Result<Ginux> {
        if self.ginux.is_none() {
            self.ginux = Some(match self.explicit {
                Some(ginux) => ginux.clone(),
                None => Ginux::from_env()?,
            });
        }
        Ok(self.ginux.clone().expect("resolved above"))
    }

    fn run(&mut self, invocation: &crate::launch::Invocation<'_>) -> Result<serde_json::Value> {
        // A stale variable of the fork's launcher stops every run.
        crate::runtime::refuse_gone_variables()?;
        invocation
            .options
            .validate()
            .context(crate::failure::Category::Startup.error("invalid invocation options"))?;
        crate::runtime::set_fetch_events(invocation.options.execution.machine_events);
        crate::core::output::set_machine_events(invocation.options.execution.machine_events);
        let all = crate::needs::declarations(invocation.recipe, invocation.search_path)?;
        let operation = invocation.operation;
        if operation == crate::worker::records::Operation::Latest {
            crate::needs::check_declared_defines(&all, &invocation.options.defines)?;
        } else {
            crate::needs::check_defines(&all, &invocation.options.defines)?;
        }
        let install = operation == crate::worker::records::Operation::Install;
        let builds = operation.builds();
        if invocation.options.execution.machine_events
            && (builds || operation == crate::worker::records::Operation::Download)
        {
            // For a front end: every recipe of this run in the order they run,
            // each with the steps it has (`plan` with `recipe` repeats them
            // when the recipe starts).
            let recipes: Vec<_> = all
                .iter()
                .filter(|node| builds || all.last().is_some_and(|main| main.recipe == node.recipe))
                .map(
                    |node| serde_json::json!({"name": node.name(), "steps": node.declared.steps()}),
                )
                .collect();
            crate::diagnostics::diag!(
                "{}",
                serde_json::json!({"event": "plan", "recipes": recipes})
            );
        }
        let bindings = crate::inputs::bind_inputs(
            &all,
            invocation.inputs,
            invocation.search_path,
            // latest() sees no inputs, so it needs none.
            operation != crate::worker::records::Operation::Latest,
        )?;
        // Resolve all declarations and contracts before the first dependency can
        // execute. Parent restrictions never become dependency grants.
        let main_recipe = &all.last().context("empty recipe graph")?.recipe;
        // Download and cleanup run the main recipe without its dependencies
        // (their DEPS entries are ()).
        let graph: Vec<_> = all
            .iter()
            .filter(|node| builds || node.recipe == *main_recipe)
            .collect();
        // Each recipe's state key, recorded with what its run keeps.
        let keys = crate::state::keys(
            &all,
            &invocation.options.defines,
            invocation.options.execution.package_prefix.as_deref(),
            &invocation.options.execution.host_libc,
        )?;
        let mut executions = Vec::with_capacity(graph.len());
        let mut placements = Vec::with_capacity(graph.len());
        for node in &graph {
            let placement = if operation == crate::worker::records::Operation::Latest {
                // latest() runs alone on the host and may not start programs:
                // a host worker has no program helpers and cannot exec.
                crate::execution::Placement {
                    host: true,
                    reason: "latest() runs on the host".into(),
                }
            } else {
                let eligibility =
                    crate::execution::eligibility(&node.recipe, invocation.search_path)?;
                crate::execution::Placement::choose(
                    invocation.options.execution.execution,
                    &eligibility,
                )
                .with_context(|| format!("placing {}", node.recipe.display()))?
            };
            placements.push(placement);
            let mut execution = invocation.options.execution.clone();
            if node.recipe != *main_recipe {
                execution = execution.for_dependency();
                execution.narrowing = node.needs.dependency_narrowing(
                    &invocation.options.execution.narrowing,
                    &invocation.options.execution.host_libc,
                )?;
            }
            let effective = node.needs.resolve(&execution.narrowing, &execution)?;
            if !placements.last().unwrap().host {
                crate::needs::contract::inspect(&self.ginux()?, &effective)?;
            }
            executions.push(execution);
        }
        let mut results = std::collections::BTreeMap::<PathBuf, serde_json::Value>::new();
        // A dependency's published outputs are staged for its consumer and
        // then go, also when a later run of the graph fails.
        let mut staged = Staged {
            state_dir: invocation.state_dir,
            outputs: Vec::new(),
        };
        let system = invocation
            .options
            .defines
            .iter()
            .any(|(k, _)| k == "RECIPE_SYSTEM_CONTEXT");
        // A system change: packages the root already holds, by recipe digest
        // and version (`RECIPE_SYSTEM_PRESENT`, written by the system route).
        let present = if system && install {
            system_present()?
        } else {
            Vec::new()
        };
        let mut skipped = Vec::new();
        for ((node, execution), placement) in graph.iter().zip(executions).zip(&placements) {
            let main = node.recipe == *main_recipe;
            let name = node
                .recipe
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            if !present.is_empty() {
                let digest = node.producer_digest()?;
                let version = node.version().map(str::to_owned);
                if let Some(held) = present
                    .iter()
                    .find(|p| p.digest == digest && Some(&p.version) == version.as_ref())
                {
                    crate::diagnostics::diag!(
                        "recipe: {name}: the root already holds {} {} from this recipe; not produced again",
                        held.name,
                        held.version
                    );
                    skipped.push(serde_json::json!({"name": held.name, "recipe": node.recipe}));
                    results.insert(
                        node.recipe.clone(),
                        serde_json::json!({"name": held.name, "version": held.version,
                            "outputs": {}, "system_present": true}),
                    );
                    continue;
                }
            }
            crate::diagnostics::diag!("recipe: {name}: {}", placement.reason);
            crate::cancellation::check()?;
            // Each dependency's declared output names, for the worker's DEPS.
            let dependency_outputs: std::collections::BTreeMap<String, Vec<String>> = node
                .dependencies
                .iter()
                .map(|dependency| -> Result<_> {
                    let producer = all
                        .iter()
                        .find(|node| node.recipe == dependency.recipe)
                        .context("a dependency is missing from the recipe graph")?;
                    Ok((
                        dependency.name.clone(),
                        producer.needs.outputs.keys().cloned().collect(),
                    ))
                })
                .collect::<Result<_>>()?;
            let transfer = (builds && !node.dependencies.is_empty())
                .then(crate::temporary::TemporaryDirectory::new)
                .transpose()?;
            let mut internal = Vec::new();
            if let Some(transfer) = &transfer {
                let dependencies = transfer.path().join(crate::domain::DEPENDENCY_INPUT);
                let tools = transfer.path().join(crate::domain::DEPENDENCY_TOOLS);
                std::fs::create_dir(&dependencies)?;
                std::fs::create_dir(&tools)?;
                let mut product_inputs = Vec::new();
                for dependency in &node.dependencies {
                    // A runtime dependency's `system` output is for the root
                    // the change writes, not for its consumer: in a system
                    // change it is not staged into the consumer's inputs.
                    let staged_system =
                        |output: &str| !(system && !dependency.build_only && output == "system");
                    let outputs = results
                        .get(&dependency.recipe)
                        .context("dependency was not prepared")?
                        .get("outputs")
                        .and_then(serde_json::Value::as_object)
                        .context("dependency outputs missing")?;
                    let destination = dependencies.join(&dependency.name);
                    std::fs::create_dir_all(&destination)?;
                    for (name, value) in outputs {
                        if !staged_system(name) {
                            continue;
                        }
                        let path = PathBuf::from(
                            value.as_str().context("dependency output must be a path")?,
                        );
                        let kind = graph
                            .iter()
                            .find(|node| node.recipe == dependency.recipe)
                            .context("dependency was not prepared")?
                            .needs
                            .outputs
                            .get(name)
                            .copied()
                            .context("a dependency published an undeclared output")?;
                        // The input snapshot owner preserves aliases across named
                        // products. Copying each product here would split them.
                        product_inputs.push(crate::inputs::ExtraInput {
                            name: format!(
                                "{}/{}/{name}",
                                crate::domain::DEPENDENCY_INPUT,
                                dependency.name
                            ),
                            path: path.clone(),
                        });
                        if kind == crate::needs::PathKind::Directory && name == "tools" {
                            crate::target::merge_tree(&path, &tools)?;
                        }
                    }
                }
                internal.extend([
                    crate::inputs::ExtraInput {
                        name: crate::domain::DEPENDENCY_INPUT.into(),
                        path: dependencies,
                    },
                    crate::inputs::ExtraInput {
                        name: crate::domain::DEPENDENCY_TOOLS.into(),
                        path: tools,
                    },
                ]);
                internal.extend(product_inputs);
            }
            let destination = if main {
                invocation.destination.to_path_buf()
            } else {
                crate::retained::dependency_destination(invocation.destination, &node.recipe)
            };
            let child = crate::launch::Invocation {
                recipe: &node.recipe,
                inputs: &bindings[&node.recipe],
                destination: &destination,
                // A dependency's outputs are published for its consumer.
                operation: if main {
                    operation
                } else {
                    crate::worker::records::Operation::Install
                },
                key: &keys[&node.recipe],
                options: crate::worker::records::InvocationOptions {
                    defines: invocation.options.defines.clone(),
                    execution,
                },
                dependencies: &dependency_outputs,
                // Only the main recipe's install activates and cleans up
                // (`crate::launch::Finish`); a dependency keeps its result
                // for its consumer until the whole install is done.
                finish: if main { invocation.finish } else { None },
                // latest() and cleanup("manual") see no kept files and keep
                // nothing: no saved files are restored into their run, and
                // none are kept after it.
                persist_state: invocation.persist_state
                    && !matches!(
                        operation,
                        crate::worker::records::Operation::Latest
                            | crate::worker::records::Operation::Cleanup
                    ),
                ..*invocation
            };
            let result = self
                .run_single(&child, &internal, node, placement.host)
                .with_context(|| format!("running recipe {}", node.recipe.display()))?;
            if !main {
                staged
                    .outputs
                    .push((node.recipe.clone(), result["outputs"].clone()));
            }
            results.insert(node.recipe.clone(), result);
        }
        let mut result = results.remove(main_recipe).unwrap();
        result["operation"] = operation.as_str().into();
        // The install is complete (published, activated, the main recipe's
        // material deleted unless kept): now the material of the dependency
        // runs made for it goes too, each under its own destination lock.
        let ran_dependencies: Vec<&Path> = graph
            .iter()
            .filter(|node| node.recipe != *main_recipe)
            .filter(|node| results[&node.recipe]["system_present"] != true)
            .map(|node| node.recipe.as_path())
            .collect();
        let cleaned = install && invocation.finish.is_some_and(|finish| !finish.keep);
        if cleaned {
            let freed = crate::state::cleanup_dependencies(
                invocation.state_dir,
                invocation.destination,
                &ran_dependencies,
            );
            let own = result["freed_bytes"].as_u64().unwrap_or(0);
            result["freed_bytes"] = (own + freed).into();
        }
        // Per dependency that ran: its name, its state (a dependency's
        // result is kept and published for its consumer, never installed;
        // after a cleaned install nothing of it is left) and its steps.
        result["dependencies"] = graph
            .iter()
            .filter(|node| node.recipe != *main_recipe)
            .map(|node| {
                let ran = &results[&node.recipe];
                let mut steps = ran
                    .get("steps")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!([]));
                let state = if ran["system_present"] == true {
                    crate::state::State::Installed
                } else if cleaned {
                    for step in steps.as_array_mut().into_iter().flatten() {
                        if step["step"] == "cleanup" {
                            step["status"] = crate::core::events::StepStatus::Done.as_str().into();
                        }
                    }
                    crate::state::State::None
                } else {
                    crate::state::State::Built
                };
                serde_json::json!({"name": node.name(), "state": state.as_str(), "steps": steps})
            })
            .collect();
        if install && system {
            let mut runtime = std::collections::BTreeSet::new();
            let mut pending = vec![main_recipe.clone()];
            while let Some(path) = pending.pop() {
                let node = graph.iter().find(|n| n.recipe == path).unwrap();
                for dep in node.dependencies.iter().filter(|d| !d.build_only) {
                    if runtime.insert(dep.recipe.clone()) {
                        pending.push(dep.recipe.clone());
                    }
                }
            }
            let mut packages = Vec::new();
            for node in &graph {
                let digest = node.producer_digest()?;
                let mut output = if node.recipe == *main_recipe {
                    result.clone()
                } else {
                    results[&node.recipe].clone()
                };
                if output["system_present"] == true {
                    continue;
                }
                output["producer"] = serde_json::json!({"version":1,"recipe":node.recipe,"recipe_digest":digest,"architecture":invocation.options.execution.target_arch,
                    "dependencies":node.dependencies.iter().filter(|d|!d.build_only).map(|d| d.recipe.clone()).collect::<Vec<_>>()});
                if node.recipe == *main_recipe || runtime.contains(&node.recipe) {
                    packages.push(output);
                }
            }
            result["system_producers"] = serde_json::Value::Array(packages);
            result["system_present"] = serde_json::json!(skipped);
        }
        // A dependency's published outputs were staged for its consumer,
        // whose run is over: they go (when `staged` drops), except those a
        // system change writes into its root (`system_producers`; the system
        // route deletes them once written). The main recipe's stay: they are
        // the result.
        let consumed: std::collections::BTreeSet<String> = result["system_producers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|producer| producer["producer"]["recipe"].as_str().map(str::to_owned))
            .collect();
        staged
            .outputs
            .retain(|(recipe, _)| !consumed.contains(recipe.to_string_lossy().as_ref()));
        Ok(result)
    }

    fn run_single(
        &mut self,
        invocation: &crate::launch::Invocation<'_>,
        dependency_inputs: &[crate::inputs::ExtraInput],
        expected: &crate::needs::Declaration,
        host: bool,
    ) -> Result<serde_json::Value> {
        let ginux = if host { None } else { Some(self.ginux()?) };
        let place = if host {
            "on the host"
        } else {
            "in a Ginux instance"
        };
        let operation = invocation.operation;
        invocation
            .options
            .validate()
            .context(crate::failure::Category::Startup.error("invalid invocation options"))?;
        let prepared = crate::inputs::prepare_invocation(
            invocation.recipe,
            invocation.search_path,
            invocation.inputs,
            &invocation.options.execution,
            ginux.as_ref(),
            dependency_inputs,
            operation != crate::worker::records::Operation::Latest,
        )
        .map_err(|err| {
            anyhow::Error::new(crate::failure::Category::Startup.error(format!(
                "input preparation failed for `{operation}` ({}): {err:#}",
                invocation.recipe.display(),
            )))
        })?;
        anyhow::ensure!(
            prepared.declaration.source_digests == expected.source_digests
                && prepared.declaration.needs == expected.needs
                && prepared.declaration.dependencies == expected.dependencies,
            "recipe authority changed since graph preflight: {}",
            expected.recipe.display()
        );
        let invocation = crate::launch::Invocation {
            options: crate::worker::records::InvocationOptions {
                defines: invocation.options.defines.clone(),
                execution: prepared.execution.clone(),
            },
            ..*invocation
        };
        let report = match &ginux {
            Some(ginux) => crate::launch::run_domain(&prepared, &invocation, ginux)?,
            None => crate::host::run_host(&prepared, &invocation)?,
        };
        crate::diagnostics::diag!(
            "recipe: invocation {} finished {place}: ok={} ({} exit {}; result {})",
            prepared.invocation_id,
            report.result.ok,
            if host { "worker" } else { "ginux start" },
            report
                .launcher_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signaled".to_string()),
            report.result_outcome,
        );
        if !report.result.ok {
            // A valid result carries a failure record whenever it is not ok.
            let failure = report
                .result
                .failure
                .clone()
                .expect("a failed result carries its failure");
            return Err(anyhow::Error::new(WorkloadFailure(failure)).context(
                crate::failure::Category::Workload.error(format!(
                    "`{operation}` failed {place} (invocation {})",
                    prepared.invocation_id
                )),
            ));
        }
        report.summary.ok_or_else(|| {
            anyhow::Error::new(crate::failure::Category::Protocol.error(format!(
                "`{operation}` succeeded {place} (invocation {}) but no result was published",
                prepared.invocation_id,
            )))
        })
    }
}

/// The published outputs of the dependencies that ran, deleted when the run
/// of the graph ends, by success or by failure
/// ([`crate::publication::discard`]).
struct Staged<'a> {
    state_dir: Option<&'a Path>,
    outputs: Vec<(PathBuf, serde_json::Value)>,
}

impl Drop for Staged<'_> {
    fn drop(&mut self) {
        for (_, outputs) in &self.outputs {
            crate::publication::discard(self.state_dir, outputs);
        }
    }
}

/// The identity a system receipt records for the recipe of `node`
/// (`recipe_digest`): the digest of its source digests.
/// A package a system change's root already holds.
#[derive(serde::Deserialize)]
struct Present {
    name: String,
    version: String,
    #[serde(rename = "recipe_digest")]
    digest: String,
}

/// The environment variable naming the file of packages the root holds.
pub const SYSTEM_PRESENT_ENV: &str = "RECIPE_SYSTEM_PRESENT";

fn system_present() -> Result<Vec<Present>> {
    let Some(path) = std::env::var_os(SYSTEM_PRESENT_ENV) else {
        return Ok(Vec::new());
    };
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", PathBuf::from(&path).display()))?;
    Ok(serde_json::from_str(&text)?)
}

/// The worker's account of a failed workload, kept typed.
#[derive(Debug)]
pub(crate) struct WorkloadFailure(pub crate::worker::records::FailureInfo);

impl std::fmt::Display for WorkloadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for WorkloadFailure {}
