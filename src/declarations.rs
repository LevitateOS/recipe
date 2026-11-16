//! The literal declarations of a recipe (docs/recipe-format.md, sections 1
//! and 2), read from its compiled source without running anything: the API
//! line, `ABOUT`, `PACKAGE`, `SOURCES`, `FILES`, `PACKAGES` with the pins file
//! beside the recipe, `ACCOUNTS`, `UPSTREAM`, `RERUN`, the functions it
//! defines and the modules it imports. `NEEDS`, `deps` and `build_deps` are
//! read by [`crate::needs`], which calls [`read`].
//!
//! A base (`//! extends:`) and its child combine whole: each declaration comes
//! from the child when the child has it, else from the base (decision 11).
//! Old forms (`ctx`, `RUNTIME`, the hooks) are refused with the line to change.

use anyhow::{Context, Result, bail, ensure};
use rhai::{AST, ASTFlags, Expr, Position, Stmt};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::needs::{Needs, PathKind, literal};
use crate::pins::Pins;

/// The one recipe API this engine reads.
pub const API: i64 = 1;

/// Where a recipe author reads how to write the current form.
const FORMAT_DOC: &str = "docs/recipe-format.md";

/// The functions the engine calls, with the arities it calls them with.
const ENGINE_FUNCTIONS: [(&str, &[usize], &str); 5] = [
    ("build", &[2], "build(src, out)"),
    (
        "configure",
        &[1, 2],
        "configure(tree) or configure(tree, src)",
    ),
    ("verify", &[1], "verify(out)"),
    ("latest", &[0], "latest()"),
    ("cleanup", &[1], "cleanup(reason)"),
];

/// The functions `recipe list` reports (`functions`).
const LISTED_FUNCTIONS: [&str; 4] = ["build", "configure", "verify", "latest"];

/// Hook names of API 2, refused so a half converted recipe fails at once.
const OLD_HOOKS: [&str; 6] = [
    "acquire",
    "install",
    "remove",
    "is_acquired",
    "is_built",
    "is_installed",
];

/// Top-level declarations this module reads, each `const` and literal.
const DECLARATIONS: [&str; 8] = [
    "ABOUT", "PACKAGE", "SOURCES", "FILES", "PACKAGES", "ACCOUNTS", "UPSTREAM", "RERUN",
];

/// The engine's constants (section 4): a recipe reads them and never
/// declares them.
pub const ENGINE_CONSTANTS: [&str; 8] = [
    "PREFIX",
    "JOBS",
    "HOST_LIBC",
    "PACKAGE_FORMAT",
    "INPUTS",
    "DEFINES",
    "DEPS",
    "WORK",
];

/// Archive formats a source may have, with the file name endings that say it.
const ARCHIVES: [(&str, &[&str]); 6] = [
    ("tar.gz", &[".tar.gz", ".tgz"]),
    ("tar.xz", &[".tar.xz", ".txz"]),
    ("tar.bz2", &[".tar.bz2", ".tbz2"]),
    ("tar.zst", &[".tar.zst", ".tzst"]),
    ("zip", &[".zip"]),
    ("rpm", &[".rpm"]),
];

/// The source name `src.packages` takes in a recipe with `PACKAGES`.
pub const PACKAGES_SOURCE: &str = "packages";

/// `PACKAGE`: the program's name, version and description.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Package {
    /// The file name without `.rhai` (decision 3).
    pub name: String,
    /// `PACKAGE.version`, or the pins file's `version` line for a group.
    pub version: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Libc {
    Glibc,
    Musl,
}

impl Libc {
    fn parse(value: &str, what: &str) -> Result<Self> {
        match value {
            "glibc" => Ok(Self::Glibc),
            "musl" => Ok(Self::Musl),
            _ => bail!("{what} is \"glibc\" or \"musl\", not {value:?}"),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Glibc => "glibc",
            Self::Musl => "musl",
        }
    }
}

/// Where a source comes from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Origin {
    Url {
        url: String,
        sha256: String,
    },
    Git {
        git: String,
        commit: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
    },
}

/// One entry of `SOURCES` (section 2.4).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Source {
    #[serde(flatten)]
    pub origin: Origin,
    /// The saved file's name (`file`, else the last part of the URL); `None`
    /// for a `git` source.
    pub file: Option<String>,
    /// The archive format it is unpacked as; `None` when it stays one file
    /// (or is a checkout).
    pub format: Option<String>,
    /// A single top folder is left out when unpacked.
    pub strip: bool,
    /// Only fetched on this C library.
    pub libc: Option<Libc>,
}

impl Source {
    /// `src.NAME` is a folder (an unpacked archive or a checkout), not one file.
    pub fn is_folder(&self) -> bool {
        matches!(self.origin, Origin::Git { .. }) || self.format.is_some()
    }
}

/// One entry of `FILES` (section 2.5), with its source resolved.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FileEntry {
    /// The `SOURCES` name it comes from.
    pub source: String,
    /// The path inside the unpacked source; `""` is the whole source.
    pub path: String,
    /// A whole folder, everything in it.
    pub folder: bool,
    /// Relative to the output; `""` or ending in `/` is a folder it goes into.
    pub to: String,
    pub mode: Option<u32>,
}

/// `PACKAGES` (section 2.6): the seeds the pin generator starts from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Packages {
    /// The same packages on every system (`desktop-runtime`).
    Any(Vec<String>),
    /// Seeds per C library; `glibc_i686` are 32-bit glibc seeds.
    ByLibc {
        #[serde(skip_serializing_if = "Vec::is_empty")]
        glibc: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        musl: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        glibc_i686: Vec<String>,
    },
}

/// `UPSTREAM` (section 2.9).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Upstream {
    pub github: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

/// Everything a recipe declares besides `NEEDS` and its dependencies.
#[derive(Clone, Debug, Serialize)]
pub struct Declared {
    pub package: Package,
    /// `ABOUT` as written.
    pub about: Option<serde_json::Value>,
    pub sources: BTreeMap<String, Source>,
    pub files: Vec<FileEntry>,
    pub packages: Option<Packages>,
    /// sysusers.d lines per C library.
    pub accounts: BTreeMap<String, Vec<String>>,
    pub upstream: Option<Upstream>,
    pub rerun: bool,
    /// Of `build`, `configure`, `verify`, `latest`, those it defines.
    pub functions: Vec<&'static str>,
    /// It defines `cleanup(reason)`.
    pub cleanup: bool,
    /// The modules it (or its base, or a module) imports, canonical.
    pub modules: Vec<PathBuf>,
    /// The pins file, for a recipe with `PACKAGES`.
    #[serde(skip)]
    pub pins: Option<Pins>,
}

impl Declared {
    /// The steps the program has (section 6.1), in order.
    pub fn steps(&self) -> Vec<&'static str> {
        let mut steps = Vec::new();
        if !self.sources.is_empty() || self.packages.is_some() {
            steps.push("download");
        }
        if self.functions.contains(&"build") {
            steps.push("build");
        }
        steps.extend(["install", "cleanup"]);
        steps
    }

    /// `upstream` of `recipe list --json`.
    pub fn upstream_json(&self) -> serde_json::Value {
        match (&self.upstream, self.functions.contains(&"latest")) {
            (Some(upstream), _) => serde_json::to_value(upstream).unwrap_or_default(),
            (None, true) => serde_json::json!({"latest": true}),
            (None, false) => serde_json::Value::Null,
        }
    }
}

/// One top-level binding of a file.
struct Binding<'a> {
    value: &'a Expr,
    constant: bool,
    line: Option<usize>,
}

/// Read the declarations of a compiled recipe. `needs` is its parsed `NEEDS`.
pub(crate) fn read(
    compiled: &crate::core::executor::CompiledRecipe,
    needs: &Needs,
    search_path: Option<&Path>,
) -> Result<Declared> {
    let recipe = &compiled.recipe_path;
    let mut files: Vec<(&Path, &AST)> = vec![(recipe.as_path(), &compiled.source_ast)];
    if let (Some(base), Some(ast)) = (&compiled.base_path, &compiled.base_ast) {
        files.push((base.as_path(), ast));
    }
    let mut child = BTreeMap::new();
    let mut base = BTreeMap::new();
    for (index, (path, ast)) in files.iter().enumerate() {
        check_api(path, ast)?;
        check_old_forms(path, ast)?;
        let bindings = top_level(path, ast)?;
        if index == 0 {
            child = bindings;
        } else {
            base = bindings;
        }
    }
    // Whole declarations: the child's, else the base's.
    let mut bindings = base;
    bindings.extend(child);
    let at = |name: &str| -> Result<Option<serde_json::Value>> {
        let Some(binding) = bindings.get(name) else {
            return Ok(None);
        };
        let line = binding
            .line
            .map(|line| format!(" (line {line})"))
            .unwrap_or_default();
        ensure!(
            binding.constant,
            "{name}{line} must be const: `const {name} = ...;`"
        );
        Ok(Some(literal(binding.value).with_context(|| {
            format!("{name}{line} must be literal data: no helper calls, variables or arithmetic")
        })?))
    };

    let mut defined: BTreeMap<&str, usize> = BTreeMap::new();
    for function in compiled.ast.iter_functions() {
        if let Some((name, arities, form)) = ENGINE_FUNCTIONS
            .iter()
            .find(|(name, ..)| *name == function.name)
        {
            ensure!(
                arities.contains(&function.params.len()),
                "{} takes {}; the recipe defines {}({})",
                name,
                form,
                name,
                function.params.join(", ")
            );
            defined.insert(name, function.params.len());
        }
    }
    let functions: Vec<&'static str> = LISTED_FUNCTIONS
        .into_iter()
        .filter(|name| defined.contains_key(name))
        .collect();
    let has_build = defined.contains_key("build");

    let stem = recipe
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context("the recipe's file name is not UTF-8")?
        .to_owned();

    let about = at("ABOUT")?;
    if let Some(about) = &about {
        check_about(about)?;
    }

    let packages = at("PACKAGES")?.map(parse_packages).transpose()?;
    let pins_path = Pins::path_of(recipe);
    let pins = match &packages {
        Some(packages) => {
            ensure!(
                pins_path.is_file(),
                "PACKAGES needs its pins file {} beside the recipe; `cargo xtask base groups <distro>` writes it from the seeds",
                pins_path.display()
            );
            let pins = Pins::read(&pins_path)?;
            check_pins(&pins, packages, needs)?;
            Some(pins)
        }
        None => {
            ensure!(
                !pins_path.exists(),
                "{} is beside a recipe without PACKAGES; declare PACKAGES or delete the file",
                pins_path.display()
            );
            None
        }
    };

    let package = parse_package(at("PACKAGE")?, &stem, pins.as_ref(), needs)?;

    let sources = match at("SOURCES")? {
        Some(value) => parse_sources(value, needs)?,
        None => BTreeMap::new(),
    };

    let files = match at("FILES")? {
        Some(value) => parse_files(value, &sources, packages.is_some(), needs)?,
        None => Vec::new(),
    };

    if packages.is_some() && !has_build {
        ensure!(
            needs.outputs.len() == 1 && needs.outputs.values().all(|k| *k == PathKind::Directory),
            "a recipe with PACKAGES and no build(src, out) has one directory output (Recipe unpacks the packages into it)"
        );
    }
    if defined.contains_key("configure") {
        ensure!(
            packages.is_some() && !has_build,
            "configure(tree) is called only in a recipe with PACKAGES and no build(src, out); \
             a recipe with its own build does its configuration there"
        );
    }

    let accounts = match at("ACCOUNTS")? {
        Some(value) => parse_accounts(value, needs)?,
        None => BTreeMap::new(),
    };

    let upstream = at("UPSTREAM")?.map(parse_upstream).transpose()?;
    ensure!(
        !(upstream.is_some() && defined.contains_key("latest")),
        "the recipe has both UPSTREAM and latest(); keep one: UPSTREAM for a GitHub release, latest() for any other page"
    );

    let rerun = match at("RERUN")? {
        None => false,
        Some(serde_json::Value::Bool(true)) => true,
        Some(_) => bail!("RERUN is `const RERUN = true;` or left out"),
    };

    let modules = modules(&files_of(compiled), search_path)?;

    Ok(Declared {
        package,
        about,
        sources,
        files,
        packages,
        accounts,
        upstream,
        rerun,
        functions,
        cleanup: defined.contains_key("cleanup"),
        modules,
        pins,
    })
}

fn files_of(compiled: &crate::core::executor::CompiledRecipe) -> Vec<(PathBuf, AST)> {
    let mut files = vec![(compiled.recipe_path.clone(), compiled.source_ast.clone())];
    if let (Some(base), Some(ast)) = (&compiled.base_path, &compiled.base_ast) {
        files.push((base.clone(), ast.clone()));
    }
    files
}

fn line_of(position: Position) -> String {
    position
        .line()
        .map(|line| format!("line {line}: "))
        .unwrap_or_default()
}

/// `const RECIPE_API = 1;` is the first statement of every recipe and base.
fn check_api(path: &Path, ast: &AST) -> Result<()> {
    let file = path.display();
    let must = format!(
        "{file}: a recipe starts with `const RECIPE_API = 1;` (this Recipe reads API 1; {FORMAT_DOC} describes it)"
    );
    let mut declared = 0;
    for (index, statement) in ast.statements().iter().enumerate() {
        let Stmt::Var(binding, flags, position) = statement else {
            continue;
        };
        if binding.0.name != "RECIPE_API" {
            continue;
        }
        declared += 1;
        match &binding.1 {
            Expr::IntegerConstant(API, _) if flags.contains(ASTFlags::CONSTANT) => {}
            Expr::IntegerConstant(version, _) => bail!(
                "{file}: {}RECIPE_API = {version} is an older recipe form; this Recipe reads API 1 only. \
                 Rewrite the recipe as {FORMAT_DOC} describes (declarations and build(src, out) instead of ctx and hooks) \
                 and declare `const RECIPE_API = 1;`",
                line_of(*position)
            ),
            _ => bail!("{must}"),
        }
        ensure!(index == 0, "{}{must}", line_of(*position));
    }
    ensure!(declared == 1, "{must}");
    Ok(())
}

/// Refuse `ctx`, `RUNTIME` and the API 2 hooks, naming the line.
fn check_old_forms(path: &Path, ast: &AST) -> Result<()> {
    use rhai::ASTNode;
    let file = path.display();
    for function in ast.iter_fn_def() {
        let line = line_of(function.body.position());
        let name = function.name.as_str();
        ensure!(
            !OLD_HOOKS.contains(&name),
            "{file}: {line}fn {name}(...) is an API 2 hook, which API 1 does not call. \
             Recipe downloads SOURCES, places FILES, installs and decides what is done itself; \
             the recipe's own work goes into build(src, out) ({FORMAT_DOC}, section 3)"
        );
        ensure!(
            !function.params.iter().any(|param| param == "ctx"),
            "{file}: {line}fn {name} takes ctx, the API 2 context. API 1 has no ctx: \
             build(src, out) gets the sources and the output, and the engine's constants \
             (PREFIX, JOBS, HOST_LIBC, INPUTS, DEFINES, DEPS, WORK) are visible everywhere ({FORMAT_DOC}, sections 3 and 4)"
        );
    }
    let mut problem = None;
    ast.walk(&mut |nodes| {
        let found = match nodes.last() {
            Some(ASTNode::Stmt(Stmt::Var(binding, _, position)))
                if matches!(binding.0.name.as_str(), "ctx" | "RUNTIME") =>
            {
                Some((binding.0.name.to_string(), *position))
            }
            Some(ASTNode::Expr(Expr::Variable(variable, _, position)))
                if matches!(variable.1.as_str(), "ctx" | "RUNTIME") =>
            {
                Some((variable.1.to_string(), *position))
            }
            _ => None,
        };
        if let Some(found) = found {
            problem = Some(found);
            return false;
        }
        true
    });
    if let Some((name, position)) = problem {
        let line = line_of(position);
        if name == "ctx" {
            bail!(
                "{file}: {line}ctx is the API 2 context, which API 1 does not have. State the facts as \
                 declarations (PACKAGE, SOURCES, FILES, PACKAGES) and write the work in build(src, out) ({FORMAT_DOC}, sections 1 to 3)"
            );
        }
        bail!(
            "{file}: {line}RUNTIME is the API 2 runtime map, which API 1 does not have. Use the engine's \
             constants instead: PREFIX, JOBS, HOST_LIBC, PACKAGE_FORMAT, INPUTS, DEFINES, DEPS, WORK ({FORMAT_DOC}, section 4)"
        );
    }
    Ok(())
}

/// The top-level bindings of one file that this module reads, and a check
/// that the file declares none of the engine's constants.
fn top_level<'a>(path: &Path, ast: &'a AST) -> Result<BTreeMap<String, Binding<'a>>> {
    let mut bindings = BTreeMap::new();
    for statement in ast.statements() {
        let Stmt::Var(binding, flags, position) = statement else {
            continue;
        };
        let name = binding.0.name.as_str();
        ensure!(
            !ENGINE_CONSTANTS.contains(&name),
            "{}: {}{name} is one of the engine's constants; read it, do not declare it (rename the recipe's own)",
            path.display(),
            line_of(*position)
        );
        if !DECLARATIONS.contains(&name) {
            continue;
        }
        ensure!(
            bindings
                .insert(
                    name.to_owned(),
                    Binding {
                        value: &binding.1,
                        constant: flags.contains(ASTFlags::CONSTANT),
                        line: position.line(),
                    },
                )
                .is_none(),
            "{}: {}{name} is declared twice",
            path.display(),
            line_of(*position)
        );
    }
    Ok(bindings)
}

/// The modules a recipe imports, recursively: literal paths, resolved beside
/// the importing file, then in the search path. A module has no `RECIPE_API`
/// and no `NEEDS`.
fn modules(files: &[(PathBuf, AST)], search_path: Option<&Path>) -> Result<Vec<PathBuf>> {
    use rhai::ASTNode;
    let engine = crate::core::invocation::compile_engine();
    let mut pending: Vec<(PathBuf, AST)> = files.to_vec();
    let mut found = BTreeSet::new();
    let mut depth = 0;
    while let Some((path, ast)) = pending.pop() {
        depth += 1;
        ensure!(depth < 256, "too many imported modules");
        let mut targets = Vec::new();
        let mut computed = None;
        ast.walk(&mut |nodes| {
            if let Some(ASTNode::Stmt(Stmt::Import(import, position))) = nodes.last() {
                match &import.0 {
                    Expr::StringConstant(target, _) => targets.push(target.to_string()),
                    _ => computed = Some(*position),
                }
            }
            true
        });
        if let Some(position) = computed {
            bail!(
                "{}: {}import takes a literal path (`import \"look.rhai\" as look;`), so Recipe can stage the module",
                path.display(),
                line_of(position)
            );
        }
        for target in targets {
            crate::recipes::check_related_target(&target)
                .map_err(|problem| anyhow::anyhow!("module {target:?} {problem}"))?;
            let file = crate::recipes::module_file(&target);
            let module =
                crate::recipes::find_related(&file, &path, search_path).with_context(|| {
                    format!(
                        "{}: module {target:?} not found beside it or in the recipe folder",
                        path.display()
                    )
                })?;
            let module = module.canonicalize().unwrap_or(module);
            if !found.insert(module.clone()) {
                continue;
            }
            let text = std::fs::read_to_string(&module)
                .with_context(|| format!("cannot read module {}", module.display()))?;
            let ast = engine
                .compile(&text)
                .map_err(|e| anyhow::anyhow!("cannot compile module {}: {e}", module.display()))?;
            for statement in ast.statements() {
                if let Stmt::Var(binding, _, position) = statement
                    && matches!(binding.0.name.as_str(), "RECIPE_API" | "NEEDS")
                {
                    bail!(
                        "{}: {}a module has no {}; it is imported by recipes, which declare it",
                        module.display(),
                        line_of(*position),
                        binding.0.name
                    );
                }
            }
            check_old_forms(&module, &ast)?;
            pending.push((module, ast));
        }
    }
    Ok(found.into_iter().collect())
}

fn object(
    value: serde_json::Value,
    what: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    match value {
        serde_json::Value::Object(map) => Ok(map),
        _ => bail!("{what} must be a literal map #{{ ... }}"),
    }
}

fn string(value: &serde_json::Value, what: &str) -> Result<String> {
    value
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("{what} must be a literal string"))
}

fn strings(value: &serde_json::Value, what: &str) -> Result<Vec<String>> {
    value
        .as_array()
        .with_context(|| format!("{what} must be a literal list of strings"))?
        .iter()
        .map(|item| string(item, what))
        .collect()
}

fn check_about(about: &serde_json::Value) -> Result<()> {
    let map = about.as_object().context("ABOUT must be a literal map")?;
    if let Some(steps) = map.get("steps") {
        let steps = steps
            .as_object()
            .context("ABOUT.steps must be a map from step to text")?;
        for (step, text) in steps {
            ensure!(
                matches!(step.as_str(), "download" | "build" | "install"),
                "ABOUT.steps.{step}: the steps are download, build and install (section 6 of {FORMAT_DOC})"
            );
            ensure!(text.is_string(), "ABOUT.steps.{step} must be text");
        }
    }
    crate::clibrary::declared(about)?;
    Ok(())
}

fn parse_package(
    value: Option<serde_json::Value>,
    stem: &str,
    pins: Option<&Pins>,
    needs: &Needs,
) -> Result<Package> {
    let mut package = Package {
        name: stem.to_owned(),
        version: None,
        description: None,
    };
    if let Some(value) = value {
        for (field, value) in object(value, "PACKAGE")? {
            let text = string(&value, &format!("PACKAGE.{field}"))?;
            match field.as_str() {
                "name" => ensure!(
                    text == stem,
                    "PACKAGE.name is {text:?}, but the file is {stem}.rhai; a recipe is found by its file name, \
                     so they agree (or leave name out)"
                ),
                "version" => {
                    ensure!(
                        !text.trim().is_empty() && !text.contains(char::is_whitespace),
                        "PACKAGE.version is one word"
                    );
                    package.version = Some(text);
                }
                "description" => package.description = Some(text),
                _ => bail!("PACKAGE has name, version and description; not {field}"),
            }
        }
    }
    match pins {
        Some(pins) => {
            ensure!(
                package.version.is_none(),
                "a recipe with PACKAGES takes its version from the `version` line of {}; leave PACKAGE.version out",
                pins.path.display()
            );
            package.version = Some(pins.version.clone());
        }
        None => {
            let installed = needs
                .outputs
                .keys()
                .any(|name| name == "package" || name == "system");
            ensure!(
                package.version.is_some() || !installed,
                "PACKAGE.version is needed for a recipe with a {} output: receipts and updates compare it",
                if needs.outputs.contains_key("package") {
                    "package"
                } else {
                    "system"
                }
            );
        }
    }
    Ok(package)
}

/// The `host:port` a URL connects to.
fn endpoint(url: &str, what: &str) -> Result<String> {
    let parsed = url::Url::parse(url).with_context(|| format!("{what} is not a URL: {url}"))?;
    ensure!(
        parsed.scheme() == "https",
        "{what} is an https:// address, not {url}"
    );
    let host = parsed
        .host_str()
        .with_context(|| format!("{what} has no host: {url}"))?;
    let port = parsed.port_or_known_default().unwrap_or(443);
    Ok(format!("{}:{port}", host.to_ascii_lowercase()))
}

fn require_endpoint(needs: &Needs, url: &str, what: &str) -> Result<()> {
    let endpoint = endpoint(url, what)?;
    ensure!(
        needs.network.contains(&endpoint),
        "{what}: {endpoint} is not in NEEDS.network; add \"{endpoint}\" there, and every host it redirects to"
    );
    Ok(())
}

fn is_hex(text: &str, lengths: &[usize]) -> bool {
    lengths.contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_sources(value: serde_json::Value, needs: &Needs) -> Result<BTreeMap<String, Source>> {
    let mut sources = BTreeMap::new();
    for (name, value) in object(value, "SOURCES")? {
        let what = format!("SOURCES.{name}");
        ensure!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                && !name.as_bytes()[0].is_ascii_digit(),
            "{what}: a source name is letters, digits and _ (it is read as src.{name})"
        );
        ensure!(
            name != PACKAGES_SOURCE,
            "{what}: `packages` is the name src.packages takes in a recipe with PACKAGES; rename the source"
        );
        let map = object(value, &what)?;
        let text = |field: &str| -> Result<Option<String>> {
            map.get(field)
                .map(|value| string(value, &format!("{what}.{field}")))
                .transpose()
        };
        let flag = |field: &str| -> Result<Option<bool>> {
            map.get(field)
                .map(|value| {
                    value
                        .as_bool()
                        .with_context(|| format!("{what}.{field} is true or false"))
                })
                .transpose()
        };
        for field in map.keys() {
            ensure!(
                matches!(
                    field.as_str(),
                    "url"
                        | "sha256"
                        | "file"
                        | "unpack"
                        | "format"
                        | "strip"
                        | "libc"
                        | "git"
                        | "commit"
                        | "tag"
                ),
                "{what}.{field} is not a source field (url, sha256, file, unpack, format, strip, libc, git, commit, tag)"
            );
        }
        let libc = text("libc")?
            .map(|value| Libc::parse(&value, &format!("{what}.libc")))
            .transpose()?;
        let source = match (text("url")?, text("git")?) {
            (Some(url), None) => {
                ensure!(
                    map.get("commit").is_none() && map.get("tag").is_none(),
                    "{what}: commit and tag go with git, not url"
                );
                require_endpoint(needs, &url, &format!("{what}.url"))?;
                let sha256 = text("sha256")?
                    .with_context(|| format!("{what} needs sha256, the SHA-256 of the download"))?;
                ensure!(
                    is_hex(&sha256, &[64]),
                    "{what}.sha256 is 64 lower-case hex digits"
                );
                let file = match text("file")? {
                    Some(file) => file,
                    None => {
                        let parsed = url::Url::parse(&url)?;
                        ensure!(
                            parsed.query().is_none(),
                            "{what}.url ends in a query; name the saved file with file: \"...\""
                        );
                        parsed
                            .path_segments()
                            .and_then(|mut segments| segments.next_back())
                            .unwrap_or_default()
                            .to_owned()
                    }
                };
                ensure!(
                    crate::catalog::is_file_name(&file),
                    "{what}: the saved file's name {file:?} is letters, digits, '.', '_', '+' and '-'; set file: \"...\""
                );
                let unpack = flag("unpack")?.unwrap_or(true);
                let format = match text("format")? {
                    Some(format) => {
                        ensure!(
                            ARCHIVES.iter().any(|(name, _)| *name == format),
                            "{what}.format is one of tar.gz, tar.xz, tar.bz2, tar.zst, zip, rpm; not {format:?}"
                        );
                        ensure!(
                            unpack,
                            "{what}: format with unpack: false says nothing; leave format out"
                        );
                        Some(format)
                    }
                    None if unpack => ARCHIVES
                        .iter()
                        .find(|(_, endings)| endings.iter().any(|ending| file.ends_with(ending)))
                        .map(|(name, _)| (*name).to_owned()),
                    None => None,
                };
                let strip = flag("strip")?.unwrap_or(true);
                ensure!(
                    strip || format.is_some(),
                    "{what}: strip is for archives; this source stays one file"
                );
                Source {
                    origin: Origin::Url { url, sha256 },
                    file: Some(file),
                    format,
                    strip,
                    libc,
                }
            }
            (None, Some(git)) => {
                for field in ["sha256", "file", "unpack", "format", "strip"] {
                    ensure!(
                        map.get(field).is_none(),
                        "{what}: a git source takes git, commit, tag and libc; not {field}"
                    );
                }
                require_endpoint(needs, &git, &format!("{what}.git"))?;
                let commit = text("commit")?
                    .with_context(|| format!("{what} needs commit, the full commit id"))?;
                ensure!(
                    is_hex(&commit, &[40, 64]),
                    "{what}.commit is a full commit id (40 or 64 lower-case hex digits)"
                );
                Source {
                    origin: Origin::Git {
                        git,
                        commit,
                        tag: text("tag")?,
                    },
                    file: None,
                    format: None,
                    strip: true,
                    libc,
                }
            }
            (Some(_), Some(_)) => bail!("{what} has url or git, not both"),
            (None, None) => bail!("{what} needs url (with sha256) or git (with commit)"),
        };
        sources.insert(name, source);
    }
    Ok(sources)
}

/// A relative path without `.`, `..` or empty parts; `trailing` allows one
/// ending `/`.
fn check_relative(path: &str, what: &str) -> Result<()> {
    ensure!(
        !path.starts_with('/'),
        "{what} {path:?} is relative (no leading /)"
    );
    let inner = path.strip_suffix('/').unwrap_or(path);
    if inner.is_empty() {
        return Ok(());
    }
    for part in inner.split('/') {
        ensure!(
            !part.is_empty() && part != "." && part != "..",
            "{what} {path:?} has an empty, . or .. part"
        );
    }
    Ok(())
}

fn parse_files(
    value: serde_json::Value,
    sources: &BTreeMap<String, Source>,
    with_packages: bool,
    needs: &Needs,
) -> Result<Vec<FileEntry>> {
    let entries = value
        .as_array()
        .context("FILES must be a literal list of [from, to] or [from, to, mode]")?;
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    ensure!(
        !sources.is_empty(),
        "FILES places files of SOURCES; the recipe declares no SOURCES"
    );
    ensure!(
        needs.outputs.len() == 1,
        "FILES is for a recipe with one output; with several outputs, build(src, out) places each file"
    );
    let (output, kind) = needs.outputs.iter().next().expect("one output");
    // With one source, paths are inside it; a plain-file source is always
    // named, and in a group the packages count as a source.
    let only = (sources.len() == 1 && !with_packages)
        .then(|| sources.iter().next().expect("one source"))
        .filter(|(_, source)| source.is_folder());
    let mut files = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let what = format!("FILES[{index}]");
        let parts = entry
            .as_array()
            .filter(|parts| matches!(parts.len(), 2 | 3))
            .with_context(|| format!("{what} is [from, to] or [from, to, mode]"))?;
        let from = string(&parts[0], &format!("{what} from"))?;
        let to = string(&parts[1], &format!("{what} to"))?;
        let mode = match parts.get(2) {
            None => None,
            Some(value) => {
                let mode = value
                    .as_u64()
                    .filter(|mode| *mode <= 0o7777)
                    .with_context(|| format!("{what} mode is a number such as 0o755"))?;
                Some(mode as u32)
            }
        };
        check_relative(&from, &format!("{what} from"))?;
        check_relative(&to, &format!("{what} to"))?;
        let (source, path, folder) = match only {
            Some((name, _)) => (
                name.clone(),
                from.trim_end_matches('/').to_owned(),
                from.is_empty() || from.ends_with('/'),
            ),
            None => {
                let (name, rest) = match from.split_once('/') {
                    Some((name, rest)) => (name, Some(rest)),
                    None => (from.as_str(), None),
                };
                ensure!(
                    name != PACKAGES_SOURCE,
                    "{what}: FILES places files of SOURCES; the packages are unpacked into the tree by Recipe"
                );
                let named = sources.get(name).with_context(|| {
                    format!(
                        "{what} from {from:?} starts with {name:?}, which is not a source; with several sources (the packages count as one) a path starts with the source's name: {}",
                        sources.keys().cloned().collect::<Vec<_>>().join(", ")
                    )
                })?;
                match (rest, named.is_folder()) {
                    (None, false) => (name.to_owned(), String::new(), false),
                    (None, true) => bail!(
                        "{what}: {name} is a folder; write \"{name}/\" for all of it or \"{name}/PATH\" for one file"
                    ),
                    (Some(_), false) => bail!("{what}: {name} is one file; write \"{name}\""),
                    (Some(rest), true) => (
                        name.to_owned(),
                        rest.trim_end_matches('/').to_owned(),
                        rest.is_empty() || rest.ends_with('/'),
                    ),
                }
            }
        };
        let into = to.is_empty() || to.ends_with('/');
        ensure!(
            !folder || into,
            "{what}: a whole folder goes into a folder; end to with / (or \"\" for the output's top)"
        );
        ensure!(
            !(folder && mode.is_some()),
            "{what}: files of a whole folder keep their modes; list a single file beside it to set one"
        );
        if let Some(mode) = mode {
            ensure!(
                mode & 0o6000 == 0 || output != "package",
                "{what}: setuid and setgid bits are refused in a package output"
            );
        }
        if *kind == PathKind::File {
            ensure!(
                entries.len() == 1 && to.is_empty() && !folder,
                "the output {output} is a file: FILES is one entry [from, \"\"] whose from is a file, and it becomes the output"
            );
        }
        files.push(FileEntry {
            source,
            path,
            folder,
            to,
            mode,
        });
    }
    Ok(files)
}

fn parse_packages(value: serde_json::Value) -> Result<Packages> {
    let check = |seeds: &[String], what: &str| -> Result<()> {
        let mut seen = BTreeSet::new();
        for seed in seeds {
            ensure!(
                !seed.is_empty() && !seed.contains(char::is_whitespace),
                "{what}: a package name has no spaces"
            );
            ensure!(seen.insert(seed), "{what} names {seed} twice");
        }
        Ok(())
    };
    match value {
        serde_json::Value::Array(_) => {
            let seeds = strings(&value, "PACKAGES")?;
            check(&seeds, "PACKAGES")?;
            Ok(Packages::Any(seeds))
        }
        serde_json::Value::Object(map) => {
            let mut glibc = Vec::new();
            let mut musl = Vec::new();
            let mut glibc_i686 = Vec::new();
            for (field, value) in map {
                let what = format!("PACKAGES.{field}");
                let seeds = strings(&value, &what)?;
                check(&seeds, &what)?;
                match field.as_str() {
                    "glibc" => glibc = seeds,
                    "musl" => musl = seeds,
                    "glibc_i686" => glibc_i686 = seeds,
                    _ => bail!("PACKAGES has glibc, musl and glibc_i686; not {field}"),
                }
            }
            Ok(Packages::ByLibc {
                glibc,
                musl,
                glibc_i686,
            })
        }
        _ => bail!(
            "PACKAGES is a literal list of package names, or a map from C library to such lists"
        ),
    }
}

/// The pins file fits the seeds: a list has an `any` section, a map has
/// sections only for the C libraries it names; every pin's host is in
/// `NEEDS.network`.
fn check_pins(pins: &Pins, packages: &Packages, needs: &Needs) -> Result<()> {
    use crate::pins::SectionLibc;
    let file = pins.path.display();
    for section in &pins.sections {
        let fits = match (packages, section.libc) {
            (Packages::Any(_), SectionLibc::Any) => true,
            (Packages::Any(_), _) => false,
            (Packages::ByLibc { .. }, SectionLibc::Any) => false,
            (
                Packages::ByLibc {
                    glibc, glibc_i686, ..
                },
                SectionLibc::Glibc,
            ) => !glibc.is_empty() || !glibc_i686.is_empty(),
            (Packages::ByLibc { musl, .. }, SectionLibc::Musl) => !musl.is_empty(),
        };
        ensure!(
            fits,
            "{file} has a section for {} that PACKAGES has no seeds for; rerun the generator",
            section.libc.as_str()
        );
        for pin in &section.packages {
            require_endpoint(needs, &pin.url, &format!("{file}: {}", pin.name))?;
        }
    }
    Ok(())
}

fn parse_accounts(
    value: serde_json::Value,
    needs: &Needs,
) -> Result<BTreeMap<String, Vec<String>>> {
    ensure!(
        needs.outputs.contains_key("system"),
        "ACCOUNTS are made in the root a system tree is written into; the recipe has no system output"
    );
    let mut accounts = BTreeMap::new();
    for (field, value) in object(value, "ACCOUNTS")? {
        let libc = Libc::parse(&field, "an ACCOUNTS key")?;
        let what = format!("ACCOUNTS.{field}");
        let lines = strings(&value, &what)?;
        for line in &lines {
            ensure!(
                matches!(line.get(..2), Some("u " | "g " | "m ")),
                "{what}: {line:?} is a sysusers.d line starting with u, g or m"
            );
        }
        accounts.insert(libc.as_str().to_owned(), lines);
    }
    Ok(accounts)
}

fn parse_upstream(value: serde_json::Value) -> Result<Upstream> {
    let mut github = None;
    let mut prefix = None;
    for (field, value) in object(value, "UPSTREAM")? {
        let text = string(&value, &format!("UPSTREAM.{field}"))?;
        match field.as_str() {
            "github" => {
                let fits = text.split_once('/').is_some_and(|(owner, repo)| {
                    [owner, repo].iter().all(|part| {
                        !part.is_empty()
                            && part
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                    })
                });
                ensure!(fits, "UPSTREAM.github is \"OWNER/REPO\", not {text:?}");
                github = Some(text);
            }
            "prefix" => prefix = Some(text),
            _ => bail!(
                "UPSTREAM has github and prefix; not {field} (a release page of another kind needs fn latest())"
            ),
        }
    }
    Ok(Upstream {
        github: github.context("UPSTREAM needs github: \"OWNER/REPO\"")?,
        prefix,
    })
}
