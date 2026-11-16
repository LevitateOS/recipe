//! The engine's constants of one run (docs/recipe-format.md, section 4),
//! which replace `RUNTIME` and `ctx.runtime`. All are read-only and visible
//! everywhere (`crate::core::invocation::set_constants`).

use super::loaded::Load;
use anyhow::{Context, Result, bail};
use rhai::{Dynamic, Map};
use std::path::Path;

/// `PREFIX`, `JOBS`, `HOST_LIBC`, `PACKAGE_FORMAT`, `INPUTS`, `DEFINES`,
/// `DEPS` and `WORK` for `declaration` in this run.
pub(crate) fn of(declaration: &crate::needs::Declaration, load: &Load<'_>) -> Result<Map> {
    let needs = &declaration.needs;
    let options = load.options;
    let text = |path: &Path| -> Result<Dynamic> {
        Ok(path
            .to_str()
            .with_context(|| format!("{} is not UTF-8", path.display()))?
            .into())
    };
    let staged = |name: &str| crate::domain::local(&crate::domain::input_path(name));

    // Where the package output will live once activated; () when it will
    // not be activated (--no-activate, a dependency) and for other outputs.
    let prefix = match &options.package_prefix {
        Some(prefix) if needs.outputs.contains_key("package") => prefix.as_str().into(),
        _ => Dynamic::UNIT,
    };

    let package_format = declaration
        .declared
        .pins
        .as_ref()
        .and_then(|pins| pins.section(&options.host_libc))
        .map_or(Dynamic::UNIT, |section| section.format.as_str().into());

    let mut inputs = Map::new();
    for (name, need) in &needs.inputs {
        let path = staged(name);
        let value = if std::fs::symlink_metadata(&path).is_ok() {
            text(&path)?
        } else if need.optional {
            Dynamic::UNIT
        } else {
            bail!("the required input {name} was not staged");
        };
        inputs.insert(name.into(), value);
    }

    let mut defines = Map::new();
    for (name, default) in &needs.defines {
        let given = load
            .defines
            .iter()
            .rev()
            .find(|(given, _)| given == name)
            .map(|(_, value)| value);
        let value = given.or(default.as_ref()).with_context(|| {
            format!("--define {name}=VALUE is required (NEEDS.defines.{name} has no default)")
        })?;
        defines.insert(name.into(), value.as_str().into());
    }

    let declared: std::collections::BTreeSet<&str> = declaration
        .dependencies
        .iter()
        .map(|dependency| dependency.name.as_str())
        .collect();
    if !declared
        .iter()
        .copied()
        .eq(load.dependencies.keys().map(String::as_str))
    {
        bail!("the coordinator named other dependencies than the recipe declares");
    }
    let mut deps = Map::new();
    for (name, outputs) in load.dependencies {
        let output = |output: &str| -> Result<Dynamic> {
            let path = staged(&format!(
                "{}/{name}/{output}",
                crate::domain::DEPENDENCY_INPUT
            ));
            if std::fs::symlink_metadata(&path).is_ok() {
                text(&path)
            } else {
                Ok(Dynamic::UNIT)
            }
        };
        let value = match outputs.as_slice() {
            [only] => output(only)?,
            several => {
                let mut map = Map::new();
                for name in several {
                    map.insert(name.into(), output(name)?);
                }
                map.into()
            }
        };
        deps.insert(name.into(), value);
    }

    Ok(Map::from([
        ("PREFIX".into(), prefix),
        ("JOBS".into(), i64::from(options.jobs).into()),
        ("HOST_LIBC".into(), options.host_libc.as_str().into()),
        ("PACKAGE_FORMAT".into(), package_format),
        ("INPUTS".into(), inputs.into()),
        ("DEFINES".into(), defines.into()),
        ("DEPS".into(), deps.into()),
        (
            "WORK".into(),
            text(&crate::domain::local(crate::domain::BUILD))?,
        ),
    ]))
}
