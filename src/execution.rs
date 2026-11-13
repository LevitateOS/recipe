//! Where one recipe of a graph runs: in a Ginux domain, or on the host.
//!
//! A recipe that cannot start a program gains nothing from a Ginux domain but
//! pays for its root image, its namespaces and its ptrace supervisor. The
//! choice is made per recipe, statically, before anything runs:
//!
//! - The recipe's source, the base it `extends` and every module it imports
//!   are compiled (never run) and every function call, method call and
//!   function-pointer name in them is examined.
//! - A recipe that can reach one of the program-starting or image-reading
//!   helpers ([`crate::helpers::PROGRAM_HELPERS`]) runs in Ginux, as do recipes
//!   whose calls cannot be resolved statically: `eval`, `Fn(...)` with a
//!   computed name, a dynamic `import`.
//! - Everything else runs on the host, confined by Recipe's own helpers
//!   (`crate::helpers::confine`, `crate::helpers::acquire::net`), and the
//!   host worker does not register the program helpers at all.
//!
//! `--execution ginux` keeps every recipe in Ginux; `--execution host`
//! requires the host and refuses a recipe that is not eligible, naming the
//! call that keeps it in Ginux. The default, `auto`, is the rule above.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// The caller's choice (`--execution`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    /// Host when the recipe cannot start a program, else Ginux.
    #[default]
    Auto,
    /// Always Ginux.
    Ginux,
    /// Always the host; a recipe that can start a program is refused.
    Host,
}

impl ExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ginux => "ginux",
            Self::Host => "host",
        }
    }
}

impl std::fmt::Display for ExecutionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for ExecutionMode {
    type Err = anyhow::Error;
    fn from_str(text: &str) -> Result<Self> {
        match text {
            "auto" => Ok(Self::Auto),
            "ginux" => Ok(Self::Ginux),
            "host" => Ok(Self::Host),
            _ => bail!("execution must be auto, ginux or host (got {text:?})"),
        }
    }
}

/// The first thing found that keeps a recipe in Ginux.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Blocker {
    /// What it is, e.g. "calls exec" or "uses eval".
    pub what: String,
    pub file: PathBuf,
    pub line: Option<usize>,
}

impl std::fmt::Display for Blocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self
            .file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.file.display().to_string());
        match self.line {
            Some(line) => write!(f, "{} at {name}:{line}", self.what),
            None => write!(f, "{} in {name}", self.what),
        }
    }
}

/// Whether a recipe can run on the host, from its source alone.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Eligibility {
    /// None: no code in the recipe, its base or its modules can start a program.
    pub blocker: Option<Blocker>,
}

impl Eligibility {
    pub fn host(&self) -> bool {
        self.blocker.is_none()
    }
}

/// Where a recipe runs, and why, in words for the run's diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Placement {
    pub host: bool,
    pub reason: String,
}

impl Placement {
    /// Apply the caller's `mode` to a recipe's eligibility.
    pub(crate) fn choose(mode: ExecutionMode, eligibility: &Eligibility) -> Result<Self> {
        Ok(match (mode, &eligibility.blocker) {
            (ExecutionMode::Ginux, _) => Self {
                host: false,
                reason: "runs in Ginux (--execution ginux)".into(),
            },
            (ExecutionMode::Host | ExecutionMode::Auto, None) => Self {
                host: true,
                reason: "runs on the host (starts no program)".into(),
            },
            (ExecutionMode::Auto, Some(blocker)) => Self {
                host: false,
                reason: format!("runs in Ginux ({blocker})"),
            },
            (ExecutionMode::Host, Some(blocker)) => bail!(
                "--execution host refused: this recipe {blocker}; a recipe that can start a \
                 program or read the root image, or whose calls cannot be read from its source, \
                 runs only in Ginux. Use --execution auto or ginux, or replace that call with \
                 Recipe helpers"
            ),
        })
    }
}

/// Examine `recipe`, its `extends` base and its imported modules.
pub fn eligibility(recipe: &Path, search_path: Option<&Path>) -> Result<Eligibility> {
    let engine = crate::core::invocation::compile_engine();
    let recipe = recipe
        .canonicalize()
        .with_context(|| format!("cannot resolve recipe {}", recipe.display()))?;
    let source = std::fs::read_to_string(&recipe)
        .with_context(|| format!("cannot read recipe {}", recipe.display()))?;
    let child = engine
        .compile(&source)
        .map_err(|e| anyhow::anyhow!("cannot compile {}: {e}", recipe.display()))?;
    let mut files = vec![(recipe.clone(), child.clone())];
    if let Some(base) = crate::recipes::parse_extends(&source) {
        let path = crate::recipes::find_related(&base, &recipe, search_path)
            .with_context(|| format!("base recipe {base:?} of {} not found", recipe.display()))?;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read base recipe {}", path.display()))?;
        let mut ast = engine
            .compile(&text)
            .map_err(|e| anyhow::anyhow!("cannot compile {}: {e}", path.display()))?;
        // A base function the child replaces (same name and arity) never runs.
        let replaced: Vec<(String, usize)> = child
            .iter_functions()
            .map(|f| (f.name.to_owned(), f.params.len()))
            .collect();
        ast.retain_functions(|_, _, name, arity| {
            !replaced.iter().any(|(n, a)| n == name && *a == arity)
        });
        files.push((path, ast));
    }
    let mut seen = std::collections::BTreeSet::new();
    while let Some((path, ast)) = files.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let mut modules = Vec::new();
        if let Some(blocker) = examine(&path, &ast, &mut modules) {
            return Ok(Eligibility {
                blocker: Some(blocker),
            });
        }
        for module in modules {
            let target = crate::recipes::module_file(&module);
            let found = crate::recipes::find_related(&target, &path, search_path)
                .with_context(|| format!("module {module:?} of {} not found", path.display()))?;
            let found = found.canonicalize().unwrap_or(found);
            let text = std::fs::read_to_string(&found)
                .with_context(|| format!("cannot read module {}", found.display()))?;
            let ast = engine
                .compile(&text)
                .map_err(|e| anyhow::anyhow!("cannot compile {}: {e}", found.display()))?;
            files.push((found, ast));
        }
    }
    Ok(Eligibility { blocker: None })
}

/// The earliest blocker (by line) in one compiled file; literal imports go to
/// `modules`. Rhai keeps functions in a hash table, so the earliest line, not
/// the first one walked, makes the report the same on every run.
fn examine(file: &Path, ast: &rhai::AST, modules: &mut Vec<String>) -> Option<Blocker> {
    use rhai::{ASTNode, Expr, Stmt};
    let mut all = Vec::new();
    let mut found = None;
    let blocker = |what: String, position: rhai::Position| Blocker {
        what,
        file: file.to_owned(),
        line: position.line(),
    };
    ast.walk(&mut |nodes| {
        let call = match nodes.last() {
            Some(ASTNode::Stmt(Stmt::FnCall(call, position)))
            | Some(ASTNode::Expr(Expr::FnCall(call, position)))
            | Some(ASTNode::Expr(Expr::MethodCall(call, position))) => Some((call, *position)),
            Some(ASTNode::Stmt(Stmt::Import(import, position))) => {
                match &import.0 {
                    Expr::StringConstant(module, _) => modules.push(module.to_string()),
                    _ => {
                        found = Some(blocker(
                            "imports a module by a computed name".into(),
                            *position,
                        ))
                    }
                }
                None
            }
            Some(ASTNode::Expr(Expr::Variable(variable, _, position)))
                if crate::helpers::is_program_helper(&variable.1) =>
            {
                found = Some(blocker(format!("names {}", variable.1), *position));
                None
            }
            Some(ASTNode::Expr(Expr::DynamicConstant(value, position))) => {
                if let Some(pointer) = value.read_lock::<rhai::FnPtr>()
                    && crate::helpers::is_program_helper(pointer.fn_name())
                {
                    found = Some(blocker(
                        format!("points to {}", pointer.fn_name()),
                        *position,
                    ));
                }
                None
            }
            _ => None,
        };
        if let Some((call, position)) = call {
            let name = call.name.as_str();
            if crate::helpers::is_program_helper(name) {
                found = Some(blocker(format!("calls {name}"), position));
            } else if name == "eval" {
                found = Some(blocker("uses eval".into(), position));
            } else if name == "Fn" {
                match call.args.first() {
                    Some(Expr::StringConstant(target, _)) => {
                        if crate::helpers::is_program_helper(target) {
                            found = Some(blocker(format!("calls {target} through Fn"), position));
                        }
                    }
                    _ => {
                        found = Some(blocker(
                            "builds a function pointer from a computed name (Fn)".into(),
                            position,
                        ))
                    }
                }
            }
        }
        all.extend(found.take());
        true
    });
    all.into_iter()
        .min_by_key(|blocker| blocker.line.unwrap_or(usize::MAX))
}
