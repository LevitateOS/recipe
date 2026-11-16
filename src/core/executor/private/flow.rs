//! Install, the worker's part (docs/recipe-format.md, sections 3 and 6.1):
//! the download step, then the step that builds the outputs, then
//! `cleanup(reason)`. Activating or publishing the outputs (the rest of
//! install) is the coordinator's, after the worker has returned them.

use super::builtin::{self, Outputs};
use super::loaded::{EngineError, Load, Loaded, with_recipe};
use super::reporting::{report_phase_failure, report_phase_success};
use crate::core::events::HookStatus;
use crate::core::output;
use crate::lifecycle::{CleanupReason, Phase};
use crate::needs::PathKind;
use anyhow::{Context, anyhow};
use rhai::{Dynamic, Map};
use std::collections::BTreeMap;
use std::path::Path;

/// What a successful install hands the coordinator: each declared output
/// to the domain path the engine placed it at.
pub(crate) type Built = BTreeMap<String, String>;

// C4: every install runs every step. Deciding what is done already (a
// download by its SHA-256 in the store, a result by its identity and the
// checksums Recipe recorded, an installed program by its receipt) and the
// single steps are C4's. Until then nothing is ever taken as done: a run
// restored from a finished generation builds again on top of it.
pub(crate) fn install(recipe: &Path, load: &Load<'_>) -> Result<Built, EngineError> {
    with_recipe(recipe, load, |recipe| {
        let name = recipe.name.clone();
        output::set_event_recipe(&name);
        output::action(&format!("Preparing recipe for {name}"));
        output::detail(&format!("Recipe: {}", recipe.path.display()));
        if let Some(base_path) = &recipe.base_path {
            output::detail(&format!("Extends base recipe: {}", base_path.display()));
        }
        let declared = &recipe.declaration.declared;
        let work = work_step(declared);
        let steps = declared.steps();
        output::machine_event(serde_json::json!({
            "event": "plan",
            "recipe": name,
            "steps": steps,
        }));
        output::detail(&format!("Recipe flow: {}", steps.join(" → ")));

        let (deps, build_deps): (Vec<_>, Vec<_>) = recipe
            .declaration
            .dependencies
            .iter()
            .partition(|dependency| !dependency.build_only);
        let names = |list: &[&crate::needs::Dependency]| -> Vec<String> {
            list.iter()
                .map(|dependency| dependency.name.clone())
                .collect()
        };
        let (deps, build_deps) = (names(&deps), names(&build_deps));
        describe_dependencies("Runtime dependencies", "deps", &deps);
        describe_dependencies("Build-time dependencies", "build_deps", &build_deps);
        // `deps` are needed by every step; their environment lasts until the end.
        let _deps_environment = super::dependencies::select(&deps)?;

        output::action(&format!("Installing {name}"));
        let src = if steps.contains(&"download") {
            start_step(&name, Phase::Download);
            match builtin::download(declared, &load.options.host_libc) {
                Ok(src) => {
                    report_phase_success(&name, Phase::Download.name());
                    src
                }
                Err(error) => return Err(fail(recipe, Phase::Download, error)),
            }
        } else {
            Map::new()
        };

        start_step(&name, work);
        let built = match build_outputs(recipe, &build_deps, src) {
            Ok(built) => built,
            Err(error) => return Err(fail(recipe, work, error)),
        };
        report_phase_success(&name, work.name());

        // A failure of cleanup after a success fails the command.
        run_cleanup(recipe, CleanupReason::Succeeded(work))?;
        output::success(&format!("{name} built"));
        Ok(built)
    })
}

/// The step that builds the outputs: `build` for a recipe that defines
/// `build(src, out)`, else `install`, whose unpacking and placing happen in
/// the worker (section 6.1: "a recipe that compiles nothing shows no build
/// step").
fn work_step(declared: &crate::declarations::Declared) -> Phase {
    if declared.functions.contains(&"build") {
        Phase::Build
    } else {
        Phase::Install
    }
}

fn start_step(name: &str, phase: Phase) {
    output::sub_action(phase.name());
    output::hook_event(name, phase.name(), HookStatus::Running, "step started");
}

/// Place the outputs, unpack and place what the recipe declares, call
/// `build` or `configure`, finish a `system` tree, check every output and
/// call `verify(out)`. Returns each output's domain path.
fn build_outputs(
    recipe: &mut Loaded<'_>,
    build_deps: &[String],
    src: Map,
) -> anyhow::Result<Built> {
    // `build_deps` are needed only while building.
    let _build_environment = super::dependencies::select(build_deps)?;
    let declared = recipe.declaration.declared.clone();
    let outputs = place_outputs(&recipe.declaration.needs)?;
    let out = out_argument(&outputs)?;

    builtin::unpack_packages(&declared, &outputs)?;
    builtin::place_files(&declared, &src, &outputs)?;
    if recipe.runtime.has_fn("build", 2) {
        crate::core::saved::with_checkpoints(|| {
            recipe
                .runtime
                .call("build", vec![src.clone().into(), out.clone()])
        })
        .map_err(|error| anyhow!("build(src, out) failed: {error}"))
        .map(drop)?;
    } else if recipe.runtime.has_fn("configure", 1) || recipe.runtime.has_fn("configure", 2) {
        let mut args = vec![out.clone()];
        if recipe.runtime.has_fn("configure", 2) {
            args.push(builtin::configure_sources(&src).into());
        }
        crate::core::saved::with_checkpoints(|| recipe.runtime.call("configure", args))
            .map_err(|error| anyhow!("configure failed: {error}"))
            .map(drop)?;
    }
    builtin::finish_system(&declared, &outputs)?;
    check_outputs(&outputs)?;
    if recipe.runtime.has_fn("verify", 1) {
        recipe
            .runtime
            .call("verify", vec![out])
            .map_err(|error| anyhow!("verify(out) failed: {error}"))
            .map(drop)?;
    }
    Ok(outputs
        .iter()
        .map(|(name, ..)| (name.clone(), crate::domain::output_path(name)))
        .collect())
}

/// Each declared output at the path the engine chooses for it
/// (`crate::domain::output_path`): a directory output exists (seeded from the
/// `target` input when the coordinator staged one), a file output's folder
/// exists and the recipe makes the file.
fn place_outputs(needs: &crate::needs::Needs) -> anyhow::Result<Outputs> {
    let mut outputs = Vec::new();
    for (name, kind) in &needs.outputs {
        let path = crate::domain::local(&crate::domain::output_path(name));
        match kind {
            PathKind::Directory => std::fs::create_dir_all(&path)
                .with_context(|| format!("cannot create the output folder {}", path.display()))?,
            PathKind::File => {
                let folder = path.parent().context("an output has no folder")?;
                std::fs::create_dir_all(folder)?;
            }
        }
        outputs.push((name.clone(), *kind, path));
    }
    Ok(outputs)
}

/// `out`: the path of the one output, or a map by output name (section 3.2).
fn out_argument(outputs: &Outputs) -> anyhow::Result<Dynamic> {
    let text = |path: &Path| -> anyhow::Result<Dynamic> {
        Ok(path
            .to_str()
            .with_context(|| format!("{} is not UTF-8", path.display()))?
            .into())
    };
    Ok(match outputs.as_slice() {
        [(_, _, path)] => text(path)?,
        several => {
            let mut map = Map::new();
            for (name, _, path) in several {
                map.insert(name.into(), text(path)?);
            }
            map.into()
        }
    })
}

/// Every declared output is there, of its declared kind, reached through
/// plain folders only, and not a symbolic link.
fn check_outputs(outputs: &Outputs) -> anyhow::Result<()> {
    for (name, kind, path) in outputs {
        for ancestor in path.ancestors().skip(1) {
            anyhow::ensure!(
                std::fs::symlink_metadata(ancestor)?.is_dir(),
                "NEEDS.outputs.{name} traverses a non-directory ancestor"
            );
        }
        let metadata = std::fs::symlink_metadata(path).with_context(|| match kind {
            PathKind::File => format!(
                "NEEDS.outputs.{name} is a file the recipe did not make: build(src, out) writes it at out"
            ),
            PathKind::Directory => format!("NEEDS.outputs.{name} is absent"),
        })?;
        anyhow::ensure!(
            kind.matches(&metadata),
            "NEEDS.outputs.{name} requires {}",
            kind.as_str()
        );
    }
    Ok(())
}

/// A step failed: report it, call `cleanup` with the failure reason, and
/// keep the step's error as the result, with a cleanup failure beside it.
fn fail(recipe: &mut Loaded<'_>, phase: Phase, error: anyhow::Error) -> EngineError {
    report_phase_failure(&recipe.name, phase.name(), &error);
    let error = match run_cleanup(recipe, CleanupReason::Failed(phase)) {
        Ok(()) => error,
        Err(cleanup) => anyhow!("{error:#}; and then {cleanup:#}"),
    };
    EngineError::Phase { phase, error }
}

/// Call `cleanup(reason)` when the recipe defines it.
pub(super) fn run_cleanup(recipe: &mut Loaded<'_>, reason: CleanupReason) -> anyhow::Result<()> {
    if !recipe.runtime.has_fn("cleanup", 1) {
        return Ok(());
    }
    let reason = reason.as_str();
    output::hook_event(
        &recipe.name,
        "cleanup",
        HookStatus::Running,
        &format!("cleanup({reason})"),
    );
    recipe
        .runtime
        .call("cleanup", vec![reason.into()])
        .map(drop)
        .map_err(|error| anyhow!("cleanup(\"{reason}\") failed: {error}"))
}

fn describe_dependencies(label: &str, field: &str, names: &[String]) {
    if names.is_empty() {
        let kind = if field == "deps" {
            "runtime"
        } else {
            "build-time"
        };
        output::detail(&format!(
            "No {kind} dependency recipes declared (`{field}`)."
        ));
    } else {
        output::detail(&format!("{label}: {}", names.join(", ")));
    }
}
