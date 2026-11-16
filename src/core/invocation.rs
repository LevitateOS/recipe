//! The worker's Rhai engine and how it calls a recipe's functions
//! (docs/recipe-format.md, sections 1 and 3).
//!
//! What a function can see (section 1) is made true with a variable
//! resolver per run ([`set_constants`]): it knows the engine's constants
//! (section 4) and the recipe's literal top-level constants and
//! declarations, and answers for a name only when the scope being evaluated
//! does not hold it, so arguments and locals shadow it. Rhai asks it for
//! every variable of every function: the ones the engine calls, the ones
//! they call, the base of `extends` and the functions of imported modules.
//! A value computed at the top level stays in the top-level scope, which no
//! function sees.
//!
//! A resolver and not a global module: Rhai's optimizer inlines a global
//! module's variables into function bodies without regard to parameters of
//! the same name, which would replace a function's argument by a constant.

use anyhow::{Result, anyhow, bail};
use rhai::{
    AST, ASTFlags, Caches, Dynamic, Engine, EvalContext, Expr, GlobalRuntimeState, Map, Position,
    Scope, Stmt, StmtBlock,
};

/// The worker's engine. `programs` is false in a host-mode worker, which then
/// has none of [`crate::helpers::PROGRAM_HELPERS`].
pub(crate) fn engine(programs: bool) -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(2_000_000);
    engine.set_max_call_levels(64);
    set_expr_depths(&mut engine);
    engine.set_max_string_size(64 * 1024 * 1024);
    crate::helpers::register_all(&mut engine, programs);
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

/// The recipe's top-level constants whose value is literal data, with its
/// declarations among them, from an AST compiled without the optimizer (so a
/// constant computed from another is not folded into a literal). In
/// statement order: a child's constant, after its base's, replaces it.
pub(crate) fn literal_constants(ast: &AST) -> Map {
    let mut constants = Map::new();
    for statement in ast.statements() {
        if let Stmt::Var(binding, flags, _) = statement
            && flags.contains(ASTFlags::CONSTANT)
            && !matches!(binding.1, Expr::FnCall(..))
            && let Some(value) = binding.1.get_literal_value()
        {
            constants.insert(binding.0.name.as_str().into(), value);
        }
    }
    constants
}

/// Make `constants` (the engine's, then the recipe's literal ones) visible
/// to every function of this run, read-only (see the module documentation).
pub(crate) fn set_constants(engine: &mut Engine, constants: Map) {
    let constants = std::sync::Arc::new(constants);
    // Rhai marks the resolver "volatile", not deprecated; Rhai is pinned
    // (=1.19.0) for this and the setup evaluation below.
    #[allow(deprecated)]
    engine.on_var(move |name, index, context| {
        // A parameter, local or top-level variable of that name wins.
        if index > 0 || context.scope().contains(name) {
            return Ok(None);
        }
        Ok(constants.get(name).cloned())
    });
}

/// One recipe loaded for one operation: its merged AST and, once a function
/// is called, the state its top-level code left (imports among it).
pub(crate) struct Runtime<'a> {
    engine: &'a Engine,
    ast: AST,
    initialized: Option<(GlobalRuntimeState, Caches)>,
}

impl<'a> Runtime<'a> {
    pub(crate) fn new(engine: &'a Engine, ast: AST) -> Result<Self> {
        validate_api(&ast)?;
        Ok(Self {
            engine,
            ast,
            initialized: None,
        })
    }

    pub(crate) fn has_fn(&self, name: &str, arity: usize) -> bool {
        self.ast
            .iter_functions()
            .any(|f| f.name == name && f.params.len() == arity)
    }

    /// Run the top-level code once, before the first function (section 1,
    /// "Top-level code"): statements outside functions, with the engine's
    /// constants set. Its imports stay for every function; its variables
    /// stay in a scope no function sees.
    fn initialize(&mut self) -> Result<()> {
        if self.initialized.is_some() {
            return Ok(());
        }
        let mut global = self.engine.new_global_runtime_state();
        global.lib.push(self.ast.shared_lib().clone());
        global.source = self.ast.source().map(Into::into);
        global.embedded_module_resolver = self.ast.resolver().cloned();
        let mut caches = Caches::new();
        let mut scope = Scope::new();
        let setup = Expr::Stmt(Box::new(StmtBlock::new(
            self.ast.statements().iter().cloned(),
            Position::NONE,
            Position::NONE,
        )));
        // Unlike ordinary expression evaluation, setup retains imports at
        // root scope, including dynamic import expressions.
        #[allow(deprecated)]
        let _ = EvalContext::new(self.engine, &mut global, &mut caches, &mut scope, None)
            .eval_expression_tree_raw(&(&setup).into(), false)
            .or_else(|error| match *error {
                rhai::EvalAltResult::Return(value, _) | rhai::EvalAltResult::Exit(value, _) => {
                    Ok(value)
                }
                _ => Err(error),
            })
            .map_err(|e| anyhow!("the recipe's top-level code failed: {e}"))?;
        self.initialized = Some((global, caches));
        Ok(())
    }

    /// Call the recipe function `name` with `args`, in a fresh scope: it sees
    /// its arguments, the global constants and the imports (section 1).
    pub(crate) fn call(&mut self, name: &str, args: Vec<Dynamic>) -> Result<Dynamic> {
        self.initialize()?;
        let (global, caches) = self.initialized.as_mut().expect("initialized above");
        // Preserve Rhai's per-evaluation operation limit. A failed function
        // must not consume the budget its failure cleanup needs.
        global.num_operations = 0;
        // Compile only an engine-owned call expression; argument values never
        // enter source text.
        let placeholders = vec!["()"; args.len()].join(",");
        let call = self
            .engine
            .compile_expression(format!("{name}({placeholders})"))
            .map_err(|e| anyhow!("cannot call {name}: {e}"))?;
        let Some(Stmt::FnCall(call, pos)) = call.statements().first() else {
            bail!("cannot call {name}: not a function call");
        };
        let mut call = call.clone();
        call.args = args
            .into_iter()
            .map(|v| Expr::from_dynamic(v, Position::NONE))
            .collect();
        let mut scope = Scope::new();
        EvalContext::new(self.engine, global, caches, &mut scope, None)
            .eval_expression_tree(&(&Expr::FnCall(call, *pos)).into())
            .or_else(|error| match *error {
                rhai::EvalAltResult::Exit(value, _) => Ok(value),
                _ => Err(error),
            })
            .map_err(|e| anyhow!("{e}"))
    }
}

/// Refuse any recipe that does not declare `const RECIPE_API = 1;`, before
/// setup runs. The full check (first statement, old forms) is
/// `crate::declarations::read`; this one guards the worker's merged AST.
pub(crate) fn validate_api(ast: &AST) -> Result<()> {
    const MUST: &str = "Recipe must declare const RECIPE_API = 1 (this engine reads API 1)";
    let mut declared = false;
    for statement in ast.statements() {
        if let Stmt::Var(declaration, flags, _) = statement {
            let (name, value, _) = declaration.as_ref();
            if name.name != "RECIPE_API" {
                continue;
            }
            match value {
                Expr::IntegerConstant(crate::declarations::API, _)
                    if flags.contains(ASTFlags::CONSTANT) =>
                {
                    declared = true;
                }
                _ => bail!("{MUST}"),
            }
        }
    }
    if !declared {
        bail!("{MUST}");
    }
    Ok(())
}
