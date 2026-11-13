//! Shared engine configuration and explicit invocation inputs for recipe operations.

use anyhow::{Result, anyhow, bail};
use rhai::{
    AST, ASTFlags, Caches, Dynamic, Engine, EvalContext, Expr, GlobalRuntimeState, Ident, Map,
    Position, Scope, Stmt, StmtBlock,
};
use std::path::Path;

pub(crate) fn engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(2_000_000);
    engine.set_max_call_levels(64);
    set_expr_depths(&mut engine);
    engine.set_max_string_size(64 * 1024 * 1024);
    crate::helpers::register_all(&mut engine);
    engine
}

/// An engine that only compiles recipe source (declarations, metadata,
/// references), with the same parsing limits as `engine()` and no optimizer.
pub(crate) fn compile_engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_optimization_level(rhai::OptimizationLevel::None);
    set_expr_depths(&mut engine);
    engine
}

/// Rhai halves its expression-depth limits in debug builds; fixing them at its
/// release values makes a debug and a release Recipe accept the same recipes.
fn set_expr_depths(engine: &mut Engine) {
    engine.set_max_expr_depths(64, 32);
}

/// What the engine supplies to one recipe for one operation.
pub(crate) struct RuntimeInputs<'x> {
    pub recipe: &'x Path,
    pub base_dir: Option<&'x Path>,
    pub build_dir: &'x Path,
    pub defines: &'x [(String, String)],
    pub options: crate::ExecutionOptions,
    pub operation: &'x str,
    pub persist_state: bool,
}

/// The read-only map recipes see as `RUNTIME` and `ctx.runtime`.
fn runtime_map(inputs: &RuntimeInputs<'_>) -> Map {
    let text = |path: &Path| Dynamic::from(path.to_string_lossy().into_owned());
    let paths: Map = [
        (
            "recipe_dir".into(),
            text(inputs.recipe.parent().unwrap_or(Path::new("."))),
        ),
        (
            "base_recipe_dir".into(),
            inputs.base_dir.map_or_else(|| "".into(), text),
        ),
        ("build".into(), text(inputs.build_dir)),
        ("tools".into(), crate::domain::TOOLS.into()),
        ("inputs".into(), crate::domain::INPUTS.into()),
        (
            "dependencies".into(),
            crate::domain::input_path(crate::domain::DEPENDENCY_INPUT).into(),
        ),
        ("output".into(), crate::domain::OUT.into()),
        ("target".into(), crate::domain::OUT.into()),
        ("temp".into(), crate::domain::TMP.into()),
        // Where the package will live once activated; unit when it will not
        // be activated (--no-activate, a dependency).
        (
            "prefix".into(),
            inputs
                .options
                .package_prefix
                .clone()
                .map_or(Dynamic::UNIT, Into::into),
        ),
    ]
    .into();
    let options = &inputs.options;
    let platform = |os: &str, arch: &str| -> Dynamic {
        Map::from([("os".into(), os.into()), ("arch".into(), arch.into())]).into()
    };
    [
        (
            "host".into(),
            Map::from([
                ("os".into(), std::env::consts::OS.into()),
                ("arch".into(), std::env::consts::ARCH.into()),
                ("libc".into(), options.host_libc.clone().into()),
            ])
            .into(),
        ),
        (
            "target".into(),
            platform(&options.target_os, &options.target_arch),
        ),
        ("paths".into(), paths.into()),
        ("operation".into(), inputs.operation.into()),
        (
            "options".into(),
            Map::from([
                ("jobs".into(), i64::from(options.jobs).into()),
                ("force_acquire".into(), options.force_acquire.into()),
                ("force_build".into(), options.force_build.into()),
                ("force_install".into(), options.force_install.into()),
                ("persist_state".into(), inputs.persist_state.into()),
            ])
            .into(),
        ),
    ]
    .into()
}

/// The imports and global constants needed by hook calls belong to an invocation,
/// not to Scope. Keeping Scope alone and disabling call_fn's AST evaluation loses
/// those bindings. The low-level Rhai API is confined here and version-pinned.
pub(crate) struct Runtime<'a> {
    engine: &'a Engine,
    ast: AST,
    /// The recipe defines `cleanup(ctx, reason)`.
    cleanup: bool,
    scope: Scope<'static>,
    initialized: (GlobalRuntimeState, Caches),
    runtime: serde_json::Value,
}

impl<'a> Runtime<'a> {
    pub(crate) fn new(engine: &'a Engine, ast: AST, inputs: RuntimeInputs<'_>) -> Result<Self> {
        validate_api(&ast)?;
        // Cleanup is optional, but a present hook must take ctx and reason, so
        // a typo cannot silently disable hygiene.
        let cleanup = has_fn(&ast, "cleanup");
        if cleanup && !has_fn_arity(&ast, "cleanup", 2) {
            bail!("cleanup hook must be cleanup(ctx, reason) (found no 2-arg cleanup)");
        }
        let mut scope = Scope::new();
        for (key, value) in inputs.defines {
            if crate::recipes::RESERVED_NAMES.contains(&key.as_str()) {
                bail!("--define {key} names an engine-owned recipe binding");
            }
            scope.push_constant(key.as_str(), value.clone());
        }
        for statement in ast.statements() {
            if let Stmt::Var(declaration, _, _) = statement
                && declaration.0.name == "RUNTIME"
            {
                bail!("RUNTIME is an engine-owned API 2 constant; do not redeclare it");
            }
        }
        let runtime = runtime_map(&inputs);
        let expected_runtime =
            rhai::serde::from_dynamic::<serde_json::Value>(&runtime.clone().into())
                .map_err(|e| anyhow!("Invalid invocation inputs: {e}"))?;
        let initialized = {
            let mut global = engine.new_global_runtime_state();
            global.lib.push(ast.shared_lib().clone());
            global.source = ast.source().map(Into::into);
            global.embedded_module_resolver = ast.resolver().cloned();
            let mut caches = Caches::new();
            let runtime_constant = Stmt::Var(
                Box::new((
                    Ident {
                        name: "RUNTIME".into(),
                        pos: Position::NONE,
                    },
                    Expr::from_dynamic(runtime.clone().into(), Position::NONE),
                    None,
                )),
                ASTFlags::CONSTANT,
                Position::NONE,
            );
            let setup = Expr::Stmt(Box::new(StmtBlock::new(
                std::iter::once(runtime_constant).chain(ast.statements().iter().cloned()),
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
            if scope.is_constant("ctx") == Some(true) {
                bail!("API 2 ctx must be a mutable map declared with let");
            }
            // Preserve a shared ctx captured by a Rhai closure. get_value_mut
            // panics for shared values; a Dynamic write lock handles both forms.
            let mut ctx_value = scope
                // Rhai iter_raw visits the newest binding first. Preserve
                // the child's shadowing ctx, including shared closure captures.
                .iter_raw()
                .find(|(name, _, _)| *name == "ctx")
                .map(|(_, _, value)| value.clone())
                .ok_or_else(|| anyhow!("API 2 recipe must declare a ctx map"))?;
            let mut ctx = ctx_value
                .write_lock::<Map>()
                .ok_or_else(|| anyhow!("API 2 recipe must declare a ctx map"))?;
            // Discard any saved runtime snapshot; paths and policy always come
            // from this invocation. State/output map values stay recipe-owned.
            ctx.insert("runtime".into(), Dynamic::from(runtime).into_read_only());
            for field in ["state", "outputs"] {
                ctx.entry(field.into()).or_insert_with(|| Map::new().into());
            }
            validate_namespaces(&ctx, &expected_runtime)?;
            drop(ctx);
            scope.set_value("ctx", ctx_value);
            (global, caches)
        };
        Ok(Self {
            engine,
            ast,
            cleanup,
            scope,
            initialized,
            runtime: expected_runtime,
        })
    }

    pub(crate) fn restore_state(&mut self, state: &Map) -> Result<()> {
        let mut value = self
            .scope
            .iter_raw()
            .find(|(name, _, _)| *name == "ctx")
            .map(|(_, _, value)| value.clone())
            .ok_or_else(|| anyhow!("missing initialized ctx"))?;
        let mut ctx = value
            .write_lock::<Map>()
            .ok_or_else(|| anyhow!("initialized ctx is not a map"))?;
        ctx.insert("state".into(), state.clone().into());
        drop(ctx);
        self.scope.set_value("ctx", value);
        Ok(())
    }

    pub(crate) fn has_fn(&self, name: &str) -> bool {
        has_fn(&self.ast, name)
    }

    /// The recipe's top-level ctx (always a map once `new` succeeds).
    pub(crate) fn ctx(&self) -> Map {
        self.scope
            .iter_raw()
            .find(|(name, _, _)| *name == "ctx")
            .and_then(|(_, _, value)| value.read_lock::<Map>().map(|ctx| ctx.clone()))
            .unwrap_or_default()
    }

    /// A top-level `deps`/`build_deps` declaration. Malformed declarations are
    /// errors, not silently omitted tools.
    pub(crate) fn dependency_names(&self, field: &str) -> Result<Vec<String>> {
        let Some(value) = self.scope.get_value::<Dynamic>(field) else {
            return Ok(Vec::new());
        };
        let values = value
            .try_cast::<rhai::Array>()
            .ok_or_else(|| anyhow!("{field} must be an array of recipe names"))?;
        values
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                value
                    .into_string()
                    .map_err(|_| anyhow!("{field}[{index}] must be a recipe name string"))
            })
            .collect()
    }

    /// Run a hook, then verify it with `check` when the recipe defines that check.
    pub(crate) fn run_hook(&mut self, hook: &str, check: Option<&str>, ctx: Map) -> Result<Map> {
        let ctx = self
            .call_map(hook, vec![ctx.into()], false)
            .map_err(|e| anyhow!("{hook} failed: {e}"))?;
        match check.filter(|check| self.has_fn(check)) {
            Some(check) => self.verify(check, &ctx),
            None => Ok(ctx),
        }
    }

    /// Run a check and require it to be ready.
    pub(crate) fn verify(&mut self, check: &str, ctx: &Map) -> Result<Map> {
        match self.check(check, ctx)? {
            Readiness::Ready(ctx) => Ok(ctx),
            Readiness::NeedsWork(_, reason) => Err(anyhow!("{check} is not ready: {reason}")),
        }
    }

    pub(crate) fn has_cleanup(&self) -> bool {
        self.cleanup
    }

    /// Run `cleanup(ctx, reason)`; without the hook, ctx is unchanged.
    pub(crate) fn cleanup(&mut self, ctx: Map, reason: &str) -> Result<Map> {
        if !self.cleanup {
            return Ok(ctx);
        }
        self.call_map("cleanup", vec![ctx.into(), reason.into()], false)
            .map_err(|error| anyhow!("cleanup failed (reason={reason}): {error}"))
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
        let (global, caches) = &mut self.initialized;

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
        let ctx = value
            .try_cast_result::<Map>()
            .map_err(|value| -> Box<rhai::EvalAltResult> {
                rhai::EvalAltResult::ErrorMismatchOutputType(
                    "map".into(),
                    value.type_name().into(),
                    Position::NONE,
                )
                .into()
            })?;
        validate_namespaces(&ctx, &self.runtime)
            .map_err(|error| -> Box<rhai::EvalAltResult> { error.to_string().into() })?;
        Ok(ctx)
    }

    pub(crate) fn check(&mut self, name: &str, ctx: &Map) -> Result<Readiness> {
        if !self.has_fn(name) {
            return Ok(Readiness::NeedsWork(
                ctx.clone(),
                "check function is missing".into(),
            ));
        }
        let result = self.call(name, vec![ctx.clone().into()], true);
        let value = result.map_err(|e| anyhow!("{name} failed: {e}"))?;
        let mut outcome = value
            .try_cast::<Map>()
            .ok_or_else(|| anyhow!("{name} must return ready(ctx) or needs_work(ctx, reason)"))?;
        let status = outcome
            .remove("status")
            .and_then(Dynamic::try_cast::<String>);
        let ctx = outcome.remove("ctx").and_then(Dynamic::try_cast::<Map>)
            .ok_or_else(|| anyhow!("{name} readiness result is missing a ctx map; return ready(ctx) or needs_work(ctx, reason)"))?;
        validate_namespaces(&ctx, &self.runtime)?;
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

fn validate_namespaces(ctx: &Map, expected_runtime: &serde_json::Value) -> Result<()> {
    for field in ["state", "outputs"] {
        if !ctx.get(field).is_some_and(Dynamic::is::<Map>) {
            bail!("ctx.{field} must be a map in API 2");
        }
    }
    let actual = ctx
        .get("runtime")
        .ok_or_else(|| anyhow!("ctx.runtime is engine-owned and must be preserved"))?;
    let actual: serde_json::Value =
        rhai::serde::from_dynamic(actual).map_err(|e| anyhow!("ctx.runtime is invalid: {e}"))?;
    if &actual != expected_runtime {
        bail!("ctx.runtime is engine-owned and must not be changed");
    }
    Ok(())
}

pub(crate) enum Readiness {
    Ready(Map),
    NeedsWork(Map, String),
}

/// Inspect declarations without executing any recipe code. A base/child pair
/// cannot declare an unsupported version. The selected API governs the merged AST.
pub(crate) fn validate_api(ast: &AST) -> Result<()> {
    let mut declared = false;
    for statement in ast.statements() {
        if let Stmt::Var(declaration, flags, _) = statement {
            let (name, value, _) = declaration.as_ref();
            if name.name != "RECIPE_API" {
                continue;
            }
            if !flags.contains(ASTFlags::CONSTANT) || !matches!(value, Expr::IntegerConstant(2, _))
            {
                bail!("Recipe must declare const RECIPE_API = 2");
            }
            declared = true;
        }
    }
    if !declared {
        bail!("Recipe must declare const RECIPE_API = 2");
    }
    Ok(())
}

fn has_fn(ast: &AST, name: &str) -> bool {
    ast.iter_functions().any(|f| f.name == name)
}

fn has_fn_arity(ast: &AST, name: &str, arity: usize) -> bool {
    ast.iter_functions()
        .any(|f| f.name == name && f.params.len() == arity)
}
