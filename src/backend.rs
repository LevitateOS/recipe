//! The required Ginux launcher for executable recipe operations.
use anyhow::{bail, Context, Result};
use sha2::Digest;
use std::path::{Path, PathBuf};

/// Verified executable used to start the Ginux domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GinuxLauncher {
    path: PathBuf,
}

impl GinuxLauncher {
    /// Resolve and validate the required GINUX_BIN executable.
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            path: resolve_launcher()?,
        })
    }

    /// The executable selected for this invocation.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Route one executable recipe operation through Ginux.
    ///
    /// Prepare immutable inputs, execute in Ginux, and return the final context
    /// only after successful completion and validated publication. Persistence
    /// retains separate state/files pairs; there is no host execution callback.
    pub(crate) fn run(
        &self,
        invocation: &crate::launch::Invocation<'_>,
    ) -> Result<serde_json::Value> {
        invocation
            .options
            .validate_for(invocation.operation)
            .context(crate::failure::Category::Startup.error("invalid invocation options"))?;
        let graph = crate::needs::declarations(invocation.recipe, invocation.search_path)?;
        let bindings =
            crate::inputs::bind_inputs(&graph, invocation.inputs, invocation.search_path)?;
        // Resolve all declarations and contracts before the first dependency can
        // execute. Parent restrictions never become dependency grants, and
        // force flags apply to the main recipe only.
        let main_recipe = &graph.last().context("empty recipe graph")?.recipe;
        let mut executions = Vec::with_capacity(graph.len());
        for node in &graph {
            let mut execution = invocation.options.execution.clone();
            if node.recipe != *main_recipe {
                execution = execution.for_dependency();
                execution.narrowing = node
                    .needs
                    .dependency_narrowing(&invocation.options.execution.narrowing)?;
            }
            let effective = node.needs.resolve(&execution.narrowing, &execution)?;
            crate::needs::contract::inspect(&self.path, &effective)?;
            executions.push(execution);
        }
        let mut results = std::collections::BTreeMap::<PathBuf, serde_json::Value>::new();
        for (node, execution) in graph.iter().zip(executions) {
            let main = node.recipe == *main_recipe;
            // Every non-install operation reconstructs the installed input
            // identity from checked cached products, without installing them.
            let cached_only = invocation.operation != crate::worker::protocol::Operation::Install;
            crate::cancellation::check()?;
            let transfer = (!node.dependencies.is_empty())
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
                    let outputs = results
                        .get(&dependency.recipe)
                        .context("dependency was not prepared")?
                        .get("outputs")
                        .and_then(serde_json::Value::as_object)
                        .context("dependency outputs missing")?;
                    let destination = dependencies.join(&dependency.name);
                    std::fs::create_dir_all(&destination)?;
                    for (name, value) in outputs {
                        let path = PathBuf::from(
                            value.as_str().context("dependency output must be a path")?,
                        );
                        let kind = graph
                            .iter()
                            .find(|node| node.recipe == dependency.recipe)
                            .unwrap()
                            .needs
                            .outputs[name];
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
                use sha2::{Digest, Sha256};
                let digest =
                    hex::encode(Sha256::digest(node.recipe.as_os_str().as_encoded_bytes()));
                invocation.destination.join(".deps").join(&digest[..32])
            };
            let operation = if main {
                invocation.operation
            } else if cached_only {
                crate::worker::protocol::Operation::IsInstalled
            } else {
                crate::worker::protocol::Operation::Install
            };
            let child = crate::launch::Invocation {
                recipe: &node.recipe,
                inputs: &bindings[&node.recipe],
                destination: &destination,
                operation,
                cleanup_reason: if main {
                    invocation.cleanup_reason
                } else {
                    None
                },
                options: crate::worker::protocol::InvocationOptions {
                    defines: invocation.options.defines.clone(),
                    execution,
                },
                ..*invocation
            };
            let result = self.run_single(&child, &internal, node).with_context(|| {
                if !main && cached_only {
                    format!(
                        "manual {} requires matching cached products from dependency {}; run install explicitly to prepare them",
                        invocation.operation, node.recipe.display()
                    )
                } else {
                    format!("running recipe {}", node.recipe.display())
                }
            })?;
            results.insert(node.recipe.clone(), result);
        }
        let mut result = results.remove(main_recipe).unwrap();
        if invocation
            .options
            .defines
            .iter()
            .any(|(k, _)| k == "RECAB_SYSTEM_CONTEXT")
        {
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
                let digest = hex::encode(sha2::Sha256::digest(serde_json::to_vec(
                    &node.source_digests,
                )?));
                let mut output = if node.recipe == *main_recipe {
                    result.clone()
                } else {
                    results[&node.recipe].clone()
                };
                output["producer"] = serde_json::json!({"version":1,"recipe":node.recipe,"recipe_digest":digest,"architecture":invocation.options.execution.target_arch,
                    "dependencies":node.dependencies.iter().filter(|d|!d.build_only).map(|d| d.recipe.clone()).collect::<Vec<_>>()});
                if node.recipe == *main_recipe || runtime.contains(&node.recipe) {
                    packages.push(output);
                }
            }
            result["system_producers"] = serde_json::Value::Array(packages);
        }
        Ok(result)
    }

    fn run_single(
        &self,
        invocation: &crate::launch::Invocation<'_>,
        dependency_inputs: &[crate::inputs::ExtraInput],
        expected: &crate::needs::Declaration,
    ) -> Result<serde_json::Value> {
        let operation = invocation.operation;
        invocation
            .options
            .validate_for(operation)
            .context(crate::failure::Category::Startup.error("invalid invocation options"))?;
        let prepared = crate::inputs::prepare_invocation(
            invocation.recipe,
            invocation.search_path,
            invocation.inputs,
            &invocation.options.execution,
            &self.path,
            dependency_inputs,
        )
        .map_err(|err| {
            anyhow::Error::new(crate::failure::Category::Startup.error(format!(
                "ginux input preparation failed for `{operation}` ({}): {err:#}",
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
            options: crate::worker::protocol::InvocationOptions {
                defines: invocation.options.defines.clone(),
                execution: prepared.execution.clone(),
            },
            ..*invocation
        };
        let report = crate::launch::run_domain(&prepared, &invocation, &self.path)?;
        crate::diagnostics::diag!(
            "recipe: invocation {} finished in the Ginux domain: ok={} ({} checkpoint{} \
                 acked; launcher exit {}; result {})",
            prepared.invocation_id,
            report.result.ok,
            report.checkpoints_acked,
            if report.checkpoints_acked == 1 {
                ""
            } else {
                "s"
            },
            report
                .launcher_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signaled".to_string()),
            report.result_outcome,
        );
        if !report.result.ok {
            // The protocol guarantees a failure record on every non-ok result.
            let failure = report
                .result
                .failure
                .clone()
                .expect("protocol: a failed result carries its failure");
            return Err(anyhow::Error::new(WorkloadFailure(failure)).context(
                crate::failure::Category::Workload.error(format!(
                    "`{operation}` failed in the Ginux domain (invocation {})",
                    prepared.invocation_id
                )),
            ));
        }
        if !report.result.ctx_summary.is_object() {
            bail!(crate::failure::Category::Protocol.error(format!(
                "`{operation}` succeeded in the Ginux domain (invocation {}) but its ctx summary \
                 is not a JSON object",
                prepared.invocation_id,
            )));
        }
        Ok(report.result.ctx_summary)
    }
}

/// Resolve Ginux from `GINUX_BIN`, or the launcher shipped with the OS.
///
/// A missing, empty, nonexistent, or unexecutable value is an explicit
/// `startup` failure — never a silent fallback to host execution.
fn resolve_launcher() -> Result<PathBuf> {
    let raw = match std::env::var("GINUX_BIN") {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => crate::runtime::LAUNCHER.to_owned(),
        Err(error) => return Err(error).context("GINUX_BIN is not valid UTF-8"),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!(crate::failure::Category::Startup.error(
            "GINUX_BIN is empty \
             (no launcher to start)"
        ));
    }
    let path = PathBuf::from(trimmed);
    let meta = std::fs::metadata(&path).with_context(|| {
        crate::failure::Category::Startup.error(format!(
            "ginux launcher {} is missing or inaccessible",
            path.display()
        ))
    })?;
    if !meta.is_file() {
        bail!(crate::failure::Category::Startup
            .error(format!("ginux launcher {} is not a file", path.display())));
    }
    if !is_executable(&meta) {
        bail!(crate::failure::Category::Startup.error(format!(
            "ginux launcher {} is not executable",
            path.display()
        )));
    }
    Ok(path)
}

/// True when the current user could exec this file (owner/group/other exec bit).
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

/// The worker's account of a failed workload, kept typed for the autofix loop.
#[derive(Debug)]
pub(crate) struct WorkloadFailure(pub crate::worker::protocol::FailureInfo);

impl std::fmt::Display for WorkloadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for WorkloadFailure {}
