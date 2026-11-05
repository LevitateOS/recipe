//! Shared engine configuration and compatibility globals for recipe operations.

use rhai::{Engine, Scope};
use std::path::Path;

pub(crate) fn engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(2_000_000);
    engine.set_max_call_levels(64);
    engine.set_max_string_size(64 * 1024 * 1024);
    crate::helpers::register_all(&mut engine);
    engine
}

pub(crate) fn scope(
    recipe: &Path,
    base_dir: Option<&Path>,
    build_dir: Option<&Path>,
    tools_prefix: Option<&Path>,
    defines: &[(String, String)],
) -> Scope<'static> {
    let mut scope = Scope::new();
    scope.push_constant(
        "RECIPE_DIR",
        recipe
            .parent()
            .unwrap_or(Path::new("."))
            .to_string_lossy()
            .to_string(),
    );
    if let Some(base_dir) = base_dir {
        scope.push_constant("BASE_RECIPE_DIR", base_dir.to_string_lossy().to_string());
    }
    if let Some(build_dir) = build_dir {
        scope.push_constant("BUILD_DIR", build_dir.to_string_lossy().to_string());
        let tools = tools_prefix
            .map(Path::to_path_buf)
            .unwrap_or_else(|| build_dir.join(".tools"));
        scope.push_constant("TOOLS_PREFIX", tools.to_string_lossy().to_string());
    } else if let Some(tools) = tools_prefix {
        scope.push_constant("TOOLS_PREFIX", tools.to_string_lossy().to_string());
    }
    scope.push_constant("ARCH", std::env::consts::ARCH);
    scope.push_constant("NPROC", num_cpus::get() as i64);
    scope.push_constant("RPM_PATH", std::env::var("RPM_PATH").unwrap_or_default());
    for (key, value) in defines {
        scope.push_constant(key.as_str(), value.clone());
    }
    scope
}
