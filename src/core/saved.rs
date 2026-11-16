//! Saved values and resuming (docs/recipe-format.md, section 7): the helpers
//! `save`, `saved`, `checkpoint` and `reached`, and the one run's saved state
//! they work on.
//!
//! The rule is the old one: a value is saved only together with the files of
//! its checkpoint (the coordinator keeps both as one generation, or neither),
//! and a saved value never proves that work is done. `reached(name)` is true
//! only when this run was restored from checkpoint `name` or a later one.
//!
//! The worker runs one recipe at a time on one thread; the state lives in a
//! thread-local [`Session`] for the length of one operation ([`begin`]).

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use rhai::{Dynamic, EvalAltResult};

use crate::worker::protocol::SavedState;

/// Reports one checkpoint (the recipe identity and its saved state) to the
/// coordinator and returns once the coordinator has kept it with the files.
pub(crate) type CheckpointSink = Box<dyn FnMut(&Path, &serde_json::Value) -> Result<()>>;

struct Session {
    recipe: PathBuf,
    /// The values saved so far and the checkpoints reached, the restored
    /// ones first.
    state: SavedState,
    /// The checkpoints the restored generation had reached.
    restored: Vec<String>,
    /// `checkpoint` is open only while `build` or `configure` runs.
    checkpoints_open: bool,
    sink: CheckpointSink,
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// Ends the session when dropped.
pub(crate) struct Guard(());

impl Drop for Guard {
    fn drop(&mut self) {
        SESSION.with(|session| session.borrow_mut().take());
    }
}

/// Start the session of one operation on `recipe`, with the saved state the
/// coordinator restored (with its files), if any.
pub(crate) fn begin(recipe: PathBuf, restored: Option<SavedState>, sink: CheckpointSink) -> Guard {
    let state = restored.unwrap_or_default();
    let session = Session {
        recipe,
        restored: state.reached.clone(),
        state,
        checkpoints_open: false,
        sink,
    };
    SESSION.with(|cell| *cell.borrow_mut() = Some(session));
    Guard(())
}

/// The saved state now: what a result carries for the coordinator to keep
/// with the final files.
pub(crate) fn current() -> SavedState {
    SESSION.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|session| session.state.clone())
            .unwrap_or_default()
    })
}

/// Whether this run was restored from a checkpoint. A checkpoint is taken
/// only inside `build` or `configure`, after Recipe unpacked the sources and
/// the packages and placed `FILES`; the restored work folders and output
/// already hold all of that, so Recipe does not do it again.
pub(crate) fn resumed() -> bool {
    SESSION.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|session| !session.restored.is_empty())
    })
}

/// Run `f` (a call of `build` or `configure`) with `checkpoint` open.
pub(crate) fn with_checkpoints<T>(f: impl FnOnce() -> T) -> T {
    let set = |open: bool| {
        SESSION.with(|cell| {
            if let Some(session) = cell.borrow_mut().as_mut() {
                session.checkpoints_open = open;
            }
        })
    };
    set(true);
    let result = f();
    set(false);
    result
}

fn with_session<T>(f: impl FnOnce(&mut Session) -> Result<T>) -> Result<T, Box<EvalAltResult>> {
    SESSION.with(|cell| {
        let mut cell = cell.borrow_mut();
        let session = cell
            .as_mut()
            .ok_or_else(|| anyhow!("saved values exist only while Recipe runs a recipe"))
            .map_err(text)?;
        f(session).map_err(text)
    })
}

fn text(error: anyhow::Error) -> Box<EvalAltResult> {
    format!("{error:#}").into()
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
        bail!("a saved value or checkpoint name is 1 to 128 printable characters");
    }
    Ok(())
}

/// `save(name, value)`: keep a JSON-compatible value under `name`. It is
/// saved with the files at the next checkpoint, or at the end of the run.
pub(crate) fn save(name: &str, value: Dynamic) -> Result<(), Box<EvalAltResult>> {
    with_session(|session| {
        check_name(name)?;
        let value = json(&value, &format!("save({name:?}, ...)"))?;
        session.state.values.insert(name.to_owned(), value);
        Ok(())
    })
}

/// `saved(name)`: the value saved under `name`, or `()`.
pub(crate) fn saved(name: &str) -> Result<Dynamic, Box<EvalAltResult>> {
    with_session(|session| match session.state.values.get(name) {
        Some(value) => rhai::serde::to_dynamic(value).map_err(|e| anyhow!("{e}")),
        None => Ok(Dynamic::UNIT),
    })
}

/// `checkpoint(name)`: record the work folders, the sources, the output and
/// every saved value as one checkpoint, and wait until it is kept.
pub(crate) fn checkpoint(name: &str) -> Result<(), Box<EvalAltResult>> {
    with_session(|session| {
        check_name(name)?;
        if !session.checkpoints_open {
            bail!(
                "checkpoint({name:?}) is for build(src, out) and configure: an interrupted \
                 build resumes from it"
            );
        }
        session.state.reached.push(name.to_owned());
        let state = session.state.to_value();
        (session.sink)(&session.recipe, &state).map_err(|error| {
            session.state.reached.pop();
            anyhow!("checkpoint({name:?}) was not kept: {error:#}")
        })
    })
}

/// `reached(name)`: whether this run was restored from checkpoint `name` or
/// a later one.
pub(crate) fn reached(name: &str) -> Result<bool, Box<EvalAltResult>> {
    with_session(|session| Ok(session.restored.iter().any(|reached| reached == name)))
}

/// A JSON value for `value`: a map, array, string, boolean, `()` or finite
/// number, at most 64 levels deep. `what` names it in the error.
fn json(value: &Dynamic, what: &str) -> Result<serde_json::Value> {
    fn validate(value: &Dynamic, path: &str, depth: usize) -> Result<()> {
        if depth > 64 {
            bail!("{path}: nesting exceeds 64");
        }
        let value = value.flatten_clone();
        if value.is_unit() || value.is_bool() || value.is_int() || value.is_string() {
            return Ok(());
        }
        if value.is_float() {
            if !value
                .as_float()
                .map_err(|error| anyhow!("{path}: {error}"))?
                .is_finite()
            {
                bail!("{path}: numbers that are not finite cannot be saved");
            }
            return Ok(());
        }
        if let Some(array) = value.read_lock::<rhai::Array>() {
            for (index, child) in array.iter().enumerate() {
                validate(child, &format!("{path}[{index}]"), depth + 1)?;
            }
            return Ok(());
        }
        if let Some(map) = value.read_lock::<rhai::Map>() {
            for (name, child) in map.iter() {
                validate(child, &format!("{path}[{name:?}]"), depth + 1)?;
            }
            return Ok(());
        }
        bail!("{path}: a {} cannot be saved", value.type_name())
    }
    validate(value, what, 0)?;
    rhai::serde::from_dynamic::<serde_json::Value>(value).map_err(|e| anyhow!("{what}: {e}"))
}
