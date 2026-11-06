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
    engine.set_max_string_size(64 * 1024 * 1024);
    crate::helpers::register_all(&mut engine);
    engine
}

pub(crate) fn scope(
    recipe: &Path,
    base_dir: Option<&Path>,
    build_dir: Option<&Path>,
    defines: &[(String, String)],
    options: &crate::ExecutionOptions,
) -> PreparedScope {
    let paths: Map = [
        (
            "recipe_dir".into(),
            recipe
                .parent()
                .unwrap_or(Path::new("."))
                .to_string_lossy()
                .to_string()
                .into(),
        ),
        (
            "base_recipe_dir".into(),
            base_dir
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
                .into(),
        ),
        (
            "build".into(),
            build_dir
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
                .into(),
        ),
        (
            "tools".into(),
            crate::inputs::TOOLS_DOMAIN_PATH.into(),
        ),
        ("inputs".into(), "/inputs".into()),
        ("output".into(), "/out".into()),
        ("target".into(), "/out".into()),
        ("temp".into(), "/work/tmp".into()),
    ]
    .into();
    let jobs = i64::from(options.jobs);
    let platform: Map = [
        ("os".into(), std::env::consts::OS.into()),
        ("arch".into(), std::env::consts::ARCH.into()),
    ]
    .into();
    let runtime: Map = [
        ("host".into(), platform.clone().into()),
        (
            "target".into(),
            Map::from([
                ("os".into(), options.target_os.clone().into()),
                ("arch".into(), options.target_arch.clone().into()),
            ])
            .into(),
        ),
        ("paths".into(), paths.into()),
        (
            "options".into(),
            Map::from([
                ("jobs".into(), jobs.into()),
                ("force_acquire".into(), options.force_acquire.into()),
                ("force_build".into(), options.force_build.into()),
                ("force_install".into(), options.force_install.into()),
            ])
            .into(),
        ),
    ]
    .into();
    PreparedScope {
        runtime,
        defines: defines.to_vec(),
    }
}

/// Canonical invocation inputs are independent of recipe-specific defines.
pub(crate) struct PreparedScope {
    runtime: Map,
    defines: Vec<(String, String)>,
}

/// The imports and global constants needed by hook calls belong to an invocation,
/// not to Scope. Keeping Scope alone and disabling call_fn's AST evaluation loses
/// those bindings. The low-level Rhai API is confined here and version-pinned.
pub(crate) struct Runtime<'a> {
    engine: &'a Engine,
    ast: &'a AST,
    scope: Scope<'static>,
    initialized: (GlobalRuntimeState, Caches),
    runtime: serde_json::Value,
}

impl<'a> Runtime<'a> {
    pub(crate) fn new(
        engine: &'a Engine,
        ast: &'a AST,
        prepared: PreparedScope,
        operation: &str,
        persist_state: bool,
    ) -> Result<Self> {
        validate_api(ast)?;
        let PreparedScope {
            mut runtime,
            defines,
        } = prepared;
        let mut scope = Scope::new();
        for (key, value) in defines {
            if key == "RUNTIME" {
                bail!("RUNTIME is an engine-owned invocation input");
            }
            scope.push_constant(key, value);
        }
        let expected_runtime = {
            let paths = runtime["paths"].clone().cast::<Map>();
            if paths["build"]
                .clone()
                .into_string()
                .map_or(true, |s| s.is_empty())
            {
                bail!("invocation is missing its engine-supplied build directory");
            }
            for statement in ast.statements() {
                if let Stmt::Var(declaration, _, _) = statement {
                    if declaration.0.name == "RUNTIME" {
                        bail!("RUNTIME is an engine-owned API 2 constant; do not redeclare it");
                    }
                }
            }
            runtime.insert("operation".into(), operation.into());
            let mut options = runtime["options"].clone().cast::<Map>();
            options.insert("persist_state".into(), persist_state.into());
            runtime.insert("options".into(), options.into());
            rhai::serde::from_dynamic::<serde_json::Value>(&runtime.clone().into())
                .map_err(|e| anyhow!("Invalid invocation inputs: {e}"))?
        };
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

    pub(crate) fn scope(&self) -> &Scope<'static> {
        &self.scope
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
        if !super::runner::has_fn(self.ast, name) {
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
fn validate_api(ast: &AST) -> Result<()> {
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
