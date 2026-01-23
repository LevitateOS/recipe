//! Execution context management for recipe scripts
//!
//! Provides thread-local execution context for helper functions.

use rhai::EvalAltResult;
use std::cell::RefCell;
use std::path::PathBuf;

/// Execution context for recipe scripts
#[derive(Clone)]
pub struct ExecutionContext {
    pub prefix: PathBuf,
    pub build_dir: PathBuf,
    pub current_dir: PathBuf,
    pub last_downloaded: Option<PathBuf>,
    /// Files installed during this execution (for state tracking)
    pub installed_files: Vec<PathBuf>,
}

thread_local! {
    /// Current execution context for helper functions
    pub static CONTEXT: RefCell<Option<ExecutionContext>> = const { RefCell::new(None) };
}

/// Initialize the execution context
pub fn init_context(prefix: PathBuf, build_dir: PathBuf) {
    let ctx = ExecutionContext {
        prefix,
        build_dir: build_dir.clone(),
        current_dir: build_dir,
        last_downloaded: None,
        installed_files: Vec::new(),
    };
    CONTEXT.with(|c| *c.borrow_mut() = Some(ctx));
}

/// Record an installed file in the context
pub fn record_installed_file(path: PathBuf) {
    CONTEXT.with(|c| {
        if let Some(ref mut ctx) = *c.borrow_mut() {
            ctx.installed_files.push(path);
        }
    });
}

/// Get all installed files from the context
pub fn get_installed_files() -> Vec<PathBuf> {
    CONTEXT.with(|c| {
        c.borrow()
            .as_ref()
            .map(|ctx| ctx.installed_files.clone())
            .unwrap_or_default()
    })
}

/// Clear the execution context
pub fn clear_context() {
    CONTEXT.with(|c| *c.borrow_mut() = None);
}

/// RAII guard that clears context when dropped.
/// Use this to ensure context cleanup even if recipe execution panics.
pub struct ContextGuard;

impl ContextGuard {
    /// Create a new context guard. Context will be cleared when guard is dropped.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ContextGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ContextGuard {
    fn drop(&mut self) {
        clear_context();
    }
}

/// Execute a closure with immutable access to the context
pub fn with_context<F, R>(f: F) -> Result<R, Box<EvalAltResult>>
where
    F: FnOnce(&ExecutionContext) -> Result<R, Box<EvalAltResult>>,
{
    CONTEXT.with(|c| {
        let ctx = c.borrow();
        let ctx = ctx.as_ref().ok_or("No execution context")?;
        f(ctx)
    })
}

/// Execute a closure with mutable access to the context
pub fn with_context_mut<F, R>(f: F) -> Result<R, Box<EvalAltResult>>
where
    F: FnOnce(&mut ExecutionContext) -> Result<R, Box<EvalAltResult>>,
{
    CONTEXT.with(|c| {
        let mut ctx = c.borrow_mut();
        let ctx = ctx.as_mut().ok_or("No execution context")?;
        f(ctx)
    })
}
