//! Recipe dependencies use the same lifecycle executor as their parent.
//!
//! One invocation shares a tools prefix, disambiguates dependency work paths,
//! and detects cycles by canonical recipe path before evaluation.

use super::executor::{ExecutionContext, InstallAttemptError, install_with_context};
use super::output;
use anyhow::{Context, Result, bail};
use rhai::Engine;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

/// Resolves dependency recipes into an invocation's shared tools destination.
pub struct BuildDepsResolver<'a> {
    engine: &'a Engine,
    recipes_path: Option<&'a Path>,
    defines: &'a [(String, String)],
}

impl<'a> BuildDepsResolver<'a> {
    pub fn new(
        engine: &'a Engine,
        recipes_path: Option<&'a Path>,
        defines: &'a [(String, String)],
    ) -> Self {
        Self {
            engine,
            recipes_path,
            defines,
        }
    }

    pub(crate) fn resolve_with_context(
        &self,
        deps: &[String],
        persist_enabled: bool,
        exec: &mut ExecutionContext<'_>,
    ) -> Result<()> {
        for name in deps {
            let recipe_path = self.find_recipe(name, exec.current_recipe())?;
            // Path hashing separates equal recipe names in different directories.
            let digest = format!(
                "{:x}",
                Sha256::digest(recipe_path.as_os_str().as_encoded_bytes())
            );
            let stem = recipe_path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy();
            let dep_build_dir = exec
                .work_root()?
                .join(".deps")
                .join(format!("{stem}-{}", &digest[..16]));
            std::fs::create_dir_all(&dep_build_dir)
                .with_context(|| format!("creating dependency work directory for '{name}'"))?;
            output::hook_event(
                name,
                "dependency.prepare",
                "requested",
                "executing shared recipe lifecycle",
            );
            match install_with_context(
                self.engine,
                &dep_build_dir,
                &recipe_path,
                self.defines,
                persist_enabled,
                self.recipes_path,
                exec,
            ) {
                Ok(_) => output::hook_event(
                    name,
                    "dependency.ready",
                    "success",
                    "tool dependency prepared",
                ),
                Err(
                    InstallAttemptError::Fatal(error) | InstallAttemptError::Phase { error, .. },
                ) => {
                    return Err(error)
                        .with_context(|| format!("preparing dependency recipe '{name}'"));
                }
            }
        }
        Ok(())
    }

    fn find_recipe(&self, name: &str, parent_recipe: Option<&Path>) -> Result<PathBuf> {
        let name_path = Path::new(name);
        if name_path.as_os_str().is_empty()
            || name_path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            bail!(
                "Dependency name '{name}' must be a nonempty relative recipe name without parent traversal"
            );
        }
        let filename = format!("{name}.rhai");
        for directory in parent_recipe
            .and_then(Path::parent)
            .into_iter()
            .chain(self.recipes_path)
        {
            let candidate = directory.join(&filename);
            if candidate.is_file() {
                return candidate
                    .canonicalize()
                    .with_context(|| format!("resolving dependency '{name}'"));
            }
        }
        bail!(
            "Dependency recipe '{filename}' not found (parent: {parent_recipe:?}, search_path: {:?})",
            self.recipes_path
        )
    }
}
