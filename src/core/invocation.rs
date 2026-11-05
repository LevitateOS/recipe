//! Shared engine configuration and compatibility globals for recipe operations.

use anyhow::{Result, anyhow, bail};
use rhai::{
    AST, ASTFlags, Caches, Dynamic, Engine, EvalContext, Expr, GlobalRuntimeState, Map, Position,
    Scope, Stmt, StmtBlock,
};
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

/// The imports and global constants needed by hook calls belong to an invocation,
/// not to Scope. Keeping Scope alone and disabling call_fn's AST evaluation loses
/// those bindings. The low-level Rhai API is confined here and version-pinned.
pub(crate) struct Runtime<'a> {
    engine: &'a Engine,
    ast: &'a AST,
    scope: Scope<'static>,
    initialized: Option<(GlobalRuntimeState, Caches)>,
}

impl<'a> Runtime<'a> {
    pub(crate) fn new(engine: &'a Engine, ast: &'a AST, mut scope: Scope<'static>) -> Result<Self> {
        let api2 = api2(ast)?;
        let initialized = if api2 {
            let mut global = engine.new_global_runtime_state();
            global.lib.push(ast.shared_lib().clone());
            global.source = ast.source().map(Into::into);
            global.embedded_module_resolver = ast.resolver().cloned();
            let mut caches = Caches::new();
            let setup = Expr::Stmt(Box::new(StmtBlock::new(
                ast.statements().iter().cloned(),
                Position::NONE,
                Position::NONE,
            )));
            // Unlike ordinary expression evaluation, setup retains declarations
            // and imports at root scope, including dynamic import expressions.
            #[allow(deprecated)]
            let _ = EvalContext::new(engine, &mut global, &mut caches, &mut scope, None)
                .eval_expression_tree_raw(&(&setup).into(), false)
                .or_else(|error| match *error {
                    rhai::EvalAltResult::Return(value, _) | rhai::EvalAltResult::Exit(value, _) => {
                        Ok(value)
                    }
                    _ => Err(error),
                })
                .map_err(|e| anyhow!("Failed to initialize recipe: {e}"))?;
            Some((global, caches))
        } else {
            engine
                .run_ast_with_scope(&mut scope, ast)
                .map_err(|e| anyhow!("Failed to initialize recipe: {e}"))?;
            None
        };
        Ok(Self {
            engine,
            ast,
            scope,
            initialized,
        })
    }

    pub(crate) fn scope(&self) -> &Scope<'static> {
        &self.scope
    }

    pub(crate) fn is_api2(&self) -> bool {
        self.initialized.is_some()
    }

    /// Checks use an isolated variable scope as before. Filesystem effects and
    /// returned ctx remain observable; hook-local imports/variables unwind in Rhai.
    pub(crate) fn call(
        &mut self,
        name: &str,
        args: Vec<Dynamic>,
        isolated: bool,
    ) -> std::result::Result<Dynamic, Box<rhai::EvalAltResult>> {
        let mut isolated_scope;
        let scope = if isolated {
            isolated_scope = self.scope.clone();
            &mut isolated_scope
        } else {
            &mut self.scope
        };
        let Some((global, caches)) = self.initialized.as_mut() else {
            return self.engine.call_fn::<Dynamic>(scope, self.ast, name, args);
        };

        // Preserve Rhai's per-evaluation operation limit. A failed hook must not
        // consume the budget needed by its bounded failure cleanup.
        global.num_operations = 0;

        // Compile only an engine-owned call expression; argument values never
        // enter source text. Capture scope matches Engine::call_fn's access to
        // initialized globals instead of a normal Rhai call's empty scope.
        let placeholders = vec!["()"; args.len()].join(",");
        let call = self
            .engine
            .compile_expression(format!("{name}({placeholders})"))?;
        let Some(Stmt::FnCall(call, pos)) = call.statements().first() else {
            return Err(rhai::EvalAltResult::ErrorSystem(
                "Invalid hook call".into(),
                "expected a function call".into(),
            )
            .into());
        };
        let mut call = call.clone();
        call.capture_parent_scope = true;
        call.args = args
            .into_iter()
            .map(|v| Expr::from_dynamic(v, Position::NONE))
            .collect();
        EvalContext::new(self.engine, global, caches, scope, None)
            .eval_expression_tree(&(&Expr::FnCall(call, *pos)).into())
            .or_else(|error| match *error {
                rhai::EvalAltResult::Exit(value, _) => Ok(value),
                _ => Err(error),
            })
    }

    pub(crate) fn call_map(
        &mut self,
        name: &str,
        args: Vec<Dynamic>,
        isolated: bool,
    ) -> std::result::Result<Map, Box<rhai::EvalAltResult>> {
        let value = self.call(name, args, isolated)?;
        value.try_cast_result::<Map>().map_err(|value| {
            rhai::EvalAltResult::ErrorMismatchOutputType(
                "map".into(),
                value.type_name().into(),
                Position::NONE,
            )
            .into()
        })
    }

    pub(crate) fn check(&mut self, name: &str, ctx: &Map) -> Result<Readiness> {
        if !super::runner::has_fn(self.ast, name) {
            return Ok(Readiness::NeedsWork(
                ctx.clone(),
                "check function is missing".into(),
            ));
        }
        let result = self.call(name, vec![ctx.clone().into()], true);
        if !self.is_api2() {
            return match result {
                Ok(value) => value
                    .try_cast::<Map>()
                    .map(Readiness::Ready)
                    .ok_or_else(|| anyhow!("{name} must return ctx")),
                Err(error) if deliberate_throw(&error) => {
                    Ok(Readiness::NeedsWork(ctx.clone(), error.to_string()))
                }
                Err(error) => Err(anyhow!("{name} failed: {error}")),
            };
        }
        let value = result.map_err(|e| anyhow!("{name} failed: {e}"))?;
        let mut outcome = value
            .try_cast::<Map>()
            .ok_or_else(|| anyhow!("{name} must return ready(ctx) or needs_work(ctx, reason)"))?;
        let status = outcome
            .remove("status")
            .and_then(Dynamic::try_cast::<String>);
        let ctx = outcome.remove("ctx").and_then(Dynamic::try_cast::<Map>)
            .ok_or_else(|| anyhow!("{name} readiness result is missing a ctx map; return ready(ctx) or needs_work(ctx, reason)"))?;
        match status.as_deref() {
            Some("ready") if outcome.is_empty() => Ok(Readiness::Ready(ctx)),
            Some("needs_work") => {
                let reason = outcome
                    .remove("reason")
                    .and_then(Dynamic::try_cast::<String>)
                    .filter(|reason| !reason.trim().is_empty())
                    .ok_or_else(|| anyhow!("{name} needs_work requires a nonempty reason"))?;
                if !outcome.is_empty() {
                    bail!("{name} readiness result has unknown fields");
                }
                Ok(Readiness::NeedsWork(ctx, reason))
            }
            _ => bail!("{name} must return ready(ctx) or needs_work(ctx, reason)"),
        }
    }
}

pub(crate) enum Readiness {
    Ready(Map),
    NeedsWork(Map, String),
}

// A missing function, failed import, wrong type, or exhausted resource budget
// must not become permission to install. Only an intentional legacy throw is
// the old needs-work signal. Module initialization errors stay fatal.
fn deliberate_throw(error: &rhai::EvalAltResult) -> bool {
    match error {
        rhai::EvalAltResult::ErrorRuntime(..) => true,
        rhai::EvalAltResult::ErrorInFunctionCall(_, source, inner, _)
            if source != "<recipe-helper>" =>
        {
            deliberate_throw(inner)
        }
        _ => false,
    }
}

/// Inspect declarations without executing any recipe code. A base/child pair
/// cannot declare an unsupported version. The selected API governs the merged AST.
fn api2(ast: &AST) -> Result<bool> {
    let mut declared = false;
    for statement in ast.statements() {
        if let Stmt::Var(declaration, flags, _) = statement {
            let (name, value, _) = declaration.as_ref();
            if name.name != "RECIPE_API" {
                continue;
            }
            if !flags.contains(ASTFlags::CONSTANT) || !matches!(value, Expr::IntegerConstant(2, _))
            {
                bail!(
                    "Unsupported recipe API: declare const RECIPE_API = 2 or omit it for legacy semantics"
                );
            }
            declared = true;
        }
    }
    Ok(declared)
}
