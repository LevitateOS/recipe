# Recipe Dependency Management Strategy

This document describes the dependency management architecture in LevitateOS's `recipe` package manager.

## Overview

The `recipe` directory contains a Rust-based package manager with a distinctive philosophy: **"Code Over Config"**. Recipes are executable Rhai scripts, not declarative configurations, and the recipe files themselves serve as the state database.

## Directory Structure

```
recipe/
├── Cargo.toml              # Project manifest
├── README.md               # User documentation
├── PHASES.md               # Lifecycle documentation
├── src/
│   ├── lib.rs              # Public API, RecipeEngine
│   ├── bin/recipe.rs       # CLI entry point
│   └── core/
│       ├── deps.rs         # Dependency resolution engine
│       ├── lifecycle.rs    # Execution state machine
│       ├── recipe_state.rs # Recipe variable read/write
│       ├── context.rs      # Thread-local execution context
│       └── output.rs       # Terminal formatting
│   └── helpers/            # Recipe-facing functions
│       ├── acquire.rs, build.rs, install.rs
│       ├── filesystem.rs, io.rs, env.rs
│       ├── command.rs, http.rs, process.rs
└── examples/
    ├── *.rhai              # Example recipes
    └── lib/*.rhai          # Reusable recipe libraries
```

## Dependency Definition

Recipes declare dependencies as an optional array variable:

```rhai
let name = "myapp";
let version = "1.0.0";
let deps = ["readline", "ncurses"];  // Dependencies

fn acquire() {
    // Download source
}

fn install() {
    // Install to PREFIX
}
```

### Required Recipe Elements

| Element | Type | Description |
|---------|------|-------------|
| `name` | String | Package name |
| `version` | String | Package version |
| `installed` | Boolean | Installation status |
| `acquire()` | Function | Download/copy source materials |
| `install()` | Function | Copy files to PREFIX |

### Optional Elements

| Element | Type | Description |
|---------|------|-------------|
| `deps` | Array | Direct dependencies |
| `description` | String | Human-readable description |
| `build()` | Function | Compile/transform source |
| `is_installed()` | Function | Custom installation check |
| `check_update()` | Function | Return new version if available |
| `remove()` | Function | Custom uninstall logic |

## Dependency Resolution Algorithm

Located in `src/core/deps.rs`, the resolution engine uses **topological sorting with cycle detection** (inspired by pacman/libalpm).

### Process Flow

1. **Build Graph** - Scan recipes directory, extract `deps` from each `.rhai` file
2. **Topological Sort** - Order packages so dependencies install first
3. **Validate Dependencies** - Ensure all declared dependencies exist
4. **Cycle Detection** - Prevent circular dependency chains
5. **Filter Uninstalled** - Identify which packages need installation

### Graph Structure

```rust
pub struct DepGraph {
    edges: HashMap<String, Vec<String>>,  // package -> dependencies
    paths: HashMap<String, PathBuf>,      // package -> recipe path
}
```

### Supported Patterns

The algorithm handles:

- **Linear chains**: A → B → C (tested with 10+ levels)
- **Diamond patterns**: Multiple packages sharing a common dependency
- **Wide graphs**: One package depending on 10+ siblings
- **Duplicate dependencies**: Automatic deduplication

Example dependency graph:

```
           myapp
          /  |  \
      web  db   auth
       |    |    |
      http json crypto
        \   |   /
         \  |  /
          core
```

Install order: `core` → `http`, `json`, `crypto` → `web`, `db`, `auth` → `myapp`

## State Persistence

Unlike traditional package managers that use `/var/lib/*` databases, recipe files are **self-documenting state stores**.

After installation, the recipe is updated in-place:

```rhai
let name = "ripgrep";
let version = "14.0.0";
let installed = true;
let installed_version = "14.0.0";
let installed_at = 1705326600;  // Unix timestamp
let installed_files = [
    "/usr/local/bin/rg",
    "/usr/local/share/man/man1/rg.1"
];
```

### Querying State

```bash
# List installed packages
grep -l "installed = true" recipes/*.rhai

# Find packages with specific dependency
grep -l 'deps.*"openssl"' recipes/*.rhai
```

## CLI Commands

```bash
# Installation
recipe install <name>              # Install single package (no deps)
recipe install --deps <name>       # Install with all dependencies

# Dependency queries
recipe deps <name>                 # Show direct dependencies
recipe deps <name> --resolve       # Show resolved install order

# Package management
recipe remove <name>               # Remove package
recipe list                        # List all packages and status
recipe info <name>                 # Show package details
```

### Install with Dependencies Workflow

```rust
Commands::Install { package, with_deps: true } => {
    let install_order = deps::resolve_deps(&package, &recipes_path)?;
    let uninstalled = deps::filter_uninstalled(install_order)?;

    for (name, path) in uninstalled {
        engine.execute(&path)?;  // Execute in dependency order
    }
}
```

## Execution Lifecycle

Each recipe follows a strict phase order:

```
1. is_installed()  → Skip early if already done (optional)
2. acquire()       → Get source materials (required)
3. build()         → Compile/transform (optional)
4. install()       → Copy to PREFIX (required)
```

### Fail-Fast Principle

- Each phase validates prerequisites
- If `acquire()` fails, no time wasted on `build()`
- If `build()` fails, PREFIX remains unmodified
- Provides transactional safety

## Environment Variables

Available to recipe scripts:

| Variable | Description |
|----------|-------------|
| `PREFIX` | Installation prefix (default: `/usr/local`) |
| `BUILD_DIR` | Temporary build directory |
| `ARCH` | Target architecture (`x86_64`, `aarch64`) |
| `NPROC` | Number of CPU cores |

## Module System

Recipes can import shared libraries for common patterns:

```rhai
import "lib/autotools";  // configure/make/make install
import "lib/github";     // GitHub release handling
import "lib/rpm";        // RPM extraction
```

## Comparison with Traditional Package Managers

| Aspect | Traditional PMs | recipe |
|--------|-----------------|--------|
| Recipe format | YAML/TOML/JSON | Executable Rhai scripts |
| State storage | `/var/lib/*` database | Recipe files themselves |
| Executor complexity | High | Minimal |
| Recipe mutability | Read-only | Read-write |
| Remote repos | Required | Optional |

## Integration with LevitateOS

The recipe system integrates with the OS build process:

1. **Build ordering** - Dependency resolution ensures correct compilation order
2. **Initramfs generation** - Recipes define what gets included
3. **Reproducibility** - Dependencies encoded in recipes enable reproducible builds
4. **Customization** - Diamond dependencies allow component swapping (e.g., OpenSSL → BoringSSL)

## Advanced Features

### Lock Files

Prevents concurrent execution of the same recipe:

```rust
acquire_recipe_lock(recipe_path) -> RecipeLock
// Creates .rhai.lock with exclusive lock
```

### Validation

Strict recipe validation before execution:

- All required variables exist and are non-empty
- All required functions defined
- Proper types (name/version must be strings)

---

# Recipe Package Manager: Dependency Management Improvements

## Context

This document outlines improvements for LevitateOS's `recipe` package manager dependency system. Current implementation uses topological sort with cycle detection—correct foundation, but missing several features for robustness.

---

## Priority 1: Security & Integrity

### 1.1 Source Verification

**Problem:** `acquire()` has no integrity checking. MITM or compromised mirrors can inject malicious code.

**Solution:** Add mandatory checksums to recipes.

```rhai
let name = "ripgrep";
let version = "14.0.0";
let sources = [
    #{
        url: "https://github.com/BurntSushi/ripgrep/archive/14.0.0.tar.gz",
        sha256: "a]1b2c3d4e5f6...",
        // Optional: sha512, blake3
    }
];
```

**Implementation:**

```rust
// src/helpers/acquire.rs

use sha2::{Sha256, Digest};

pub fn verify_source(path: &Path, expected_sha256: &str) -> Result<(), AcquireError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    let result = hex::encode(hasher.finalize());
    
    if result != expected_sha256 {
        return Err(AcquireError::IntegrityMismatch {
            expected: expected_sha256.to_string(),
            got: result,
        });
    }
    Ok(())
}
```

**CLI addition:**

```bash
recipe hash <file>              # Generate hash for new recipe
recipe verify <name>            # Re-verify installed package sources
```

---

### 1.2 Signature Verification (Optional Enhancement)

**Problem:** Checksums verify integrity, not authenticity. Compromised recipe repo = compromised checksums.

**Solution:** Optional GPG/minisign signatures for recipes.

```rhai
let signature = "recipe.rhai.sig";  // Detached signature
let signing_key = "RWSGOq2NVecA..."; // minisign public key
```

Lower priority than checksums—only matters if recipes are distributed externally.

---

## Priority 2: Version Management

### 2.1 Version Constraints

**Problem:** `deps = ["openssl"]` provides no version guarantees. Silent breakage when dep updates.

**Solution:** Support version constraint syntax.

```rhai
let deps = [
    "core",                      // Any version (current behavior)
    "openssl >= 3.0.0",          // Minimum version
    "libxml2 >= 2.9, < 2.12",    // Range
    "zlib == 1.2.13",            // Exact pin
    "readline ^8.0",             // Compatible (>=8.0.0, <9.0.0)
    "ncurses ~6.4",              // Patch-level (>=6.4.0, <6.5.0)
];
```

**Implementation:**

```rust
// src/core/version.rs

use semver::{Version, VersionReq};

#[derive(Debug, Clone)]
pub struct Dependency {
    pub name: String,
    pub constraint: Option<VersionReq>,
}

impl Dependency {
    pub fn parse(spec: &str) -> Result<Self, ParseError> {
        // Regex: ^([a-z0-9_-]+)\s*(.*)$
        // Group 1: name, Group 2: version constraint (optional)
        let re = Regex::new(r"^([a-z0-9_-]+)\s*(.*)$")?;
        let caps = re.captures(spec).ok_or(ParseError::InvalidSpec)?;
        
        let name = caps[1].to_string();
        let constraint = if caps[2].is_empty() {
            None
        } else {
            Some(VersionReq::parse(&caps[2])?)
        };
        
        Ok(Dependency { name, constraint })
    }
    
    pub fn satisfied_by(&self, version: &Version) -> bool {
        self.constraint.as_ref().map_or(true, |c| c.matches(version))
    }
}
```

**Resolution change in `deps.rs`:**

```rust
pub fn resolve_deps(target: &str, recipes_path: &Path) -> Result<Vec<ResolvedDep>> {
    let graph = build_graph(recipes_path)?;
    let sorted = topological_sort(&graph, target)?;
    
    // NEW: Validate version constraints
    for dep in &sorted {
        validate_constraints(&dep, &graph)?;
    }
    
    Ok(sorted)
}

fn validate_constraints(dep: &ResolvedDep, graph: &DepGraph) -> Result<()> {
    for constraint in &dep.constraints {
        let provider = graph.get(&constraint.name)?;
        let provider_version = Version::parse(&provider.version)?;
        
        if !constraint.satisfied_by(&provider_version) {
            return Err(DepsError::VersionConflict {
                package: dep.name.clone(),
                requires: constraint.to_string(),
                found: provider.version.clone(),
            });
        }
    }
    Ok(())
}
```

---

### 2.2 Version Conflict Resolution

**Problem:** Diamond dependencies with different version requirements.

```
    app
   /   \
  A     B
   \   /
    C
    
A requires C >= 2.0
B requires C < 2.0
```

**Solution:** Detect and report conflicts clearly. Don't try to auto-resolve—explicit failure is better than implicit breakage.

```rust
#[derive(Debug)]
pub struct VersionConflict {
    pub package: String,
    pub requesters: Vec<(String, VersionReq)>,
    pub available: String,
}

impl fmt::Display for VersionConflict {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "Version conflict for '{}':", self.package)?;
        writeln!(f, "  Available: {}", self.available)?;
        for (requester, req) in &self.requesters {
            writeln!(f, "  {} requires {}", requester, req)?;
        }
        Ok(())
    }
}
```

**Future enhancement:** Slot-based multi-version support (like Gentoo), but only if actually needed.

---

### 2.3 Lock File for Reproducibility

**Problem:** Resolved dependency versions can drift between machines/times.

**Solution:** Optional `recipe.lock` file capturing exact resolved versions.

```toml
# recipe.lock
# Generated by: recipe install --deps myapp
# Timestamp: 2025-01-20T10:30:00Z

[resolved]
myapp = "1.0.0"
openssl = "3.2.1"
zlib = "1.2.13"
readline = "8.2"

[checksums]
openssl = "sha256:abc123..."
zlib = "sha256:def456..."
```

**CLI:**

```bash
recipe install --deps myapp          # Generate/update lock
recipe install --deps --locked myapp # Fail if lock doesn't match
recipe lock update                   # Refresh lock file
```

---

## Priority 3: Dependency Graph Integrity

### 3.1 Reverse Dependency Tracking

**Problem:** `recipe remove foo` doesn't check if other packages depend on foo.

**Solution:** Build reverse dependency map, warn/block on removal.

```rust
// src/core/deps.rs

pub fn reverse_deps(package: &str, recipes_path: &Path) -> Result<Vec<String>> {
    let graph = build_graph(recipes_path)?;
    
    graph.edges
        .iter()
        .filter(|(_, deps)| deps.contains(&package.to_string()))
        .map(|(name, _)| name.clone())
        .collect()
}
```

**CLI behavior:**

```bash
$ recipe remove openssl
Error: Cannot remove 'openssl' - required by:
  - curl
  - wget
  - python

Use --force to remove anyway (will break dependents)
```

```rust
Commands::Remove { package, force } => {
    let rdeps = deps::reverse_deps(&package, &recipes_path)?;
    let installed_rdeps: Vec<_> = rdeps
        .into_iter()
        .filter(|p| is_installed(p))
        .collect();
    
    if !installed_rdeps.is_empty() && !force {
        eprintln!("Cannot remove '{}' - required by:", package);
        for rdep in &installed_rdeps {
            eprintln!("  - {}", rdep);
        }
        return Err(RemoveError::HasDependents);
    }
    
    // Proceed with removal
}
```

---

### 3.2 Orphan Detection

**Problem:** After removing packages, dependencies may no longer be needed.

**Solution:** Track "explicitly installed" vs "installed as dependency".

```rhai
let installed = true;
let installed_as_dep = true;  // NEW: Was this pulled in automatically?
```

**CLI:**

```bash
recipe orphans              # List packages installed as deps with no dependents
recipe autoremove           # Remove orphans
```

```rust
pub fn find_orphans(recipes_path: &Path) -> Result<Vec<String>> {
    let graph = build_graph(recipes_path)?;
    
    graph.packages()
        .filter(|p| p.installed && p.installed_as_dep)
        .filter(|p| reverse_deps(&p.name, recipes_path)?.iter()
            .all(|rdep| !is_installed(rdep)))
        .map(|p| p.name.clone())
        .collect()
}
```

---

### 3.3 Dependency Graph Visualization

**Problem:** Complex dependency trees are hard to debug mentally.

**Solution:** DOT format export for graphviz.

```bash
recipe deps myapp --graph | dot -Tpng -o deps.png
recipe deps myapp --graph --installed-only  # Only show what's actually installed
```

```rust
pub fn to_dot(graph: &DepGraph, root: &str) -> String {
    let mut out = String::from("digraph deps {\n");
    out.push_str("  rankdir=TB;\n");
    out.push_str("  node [shape=box];\n");
    
    for (pkg, deps) in &graph.edges {
        for dep in deps {
            out.push_str(&format!("  \"{}\" -> \"{}\";\n", pkg, dep));
        }
    }
    
    out.push_str("}\n");
    out
}
```

---

## Priority 4: Robustness

### 4.1 Atomic Installation with Rollback

**Problem:** Partial `install()` failure leaves system in inconsistent state.

**Solution:** Stage to temp directory, atomic move on success.

```rust
// src/core/lifecycle.rs

pub fn execute_install(recipe: &Recipe) -> Result<()> {
    let staging = tempdir()?;
    let staging_prefix = staging.path().join("root");
    
    // Run install() with PREFIX pointing to staging
    env::set_var("PREFIX", &staging_prefix);
    recipe.call_install()?;
    
    // Collect installed files
    let files = walk_dir(&staging_prefix)?;
    
    // Atomic move to real prefix
    for file in &files {
        let relative = file.strip_prefix(&staging_prefix)?;
        let target = Path::new(&recipe.prefix).join(relative);
        
        // Create parent dirs
        fs::create_dir_all(target.parent().unwrap())?;
        
        // Atomic rename (same filesystem) or copy+delete
        rename_or_copy(&file, &target)?;
    }
    
    // Update recipe with installed files list
    recipe.set_installed_files(files)?;
    
    Ok(())
}
```

**Rollback on failure:**

```rust
pub fn execute_with_rollback(recipe: &Recipe) -> Result<()> {
    let backup = backup_existing_files(recipe)?;
    
    match execute_install(recipe) {
        Ok(()) => {
            cleanup_backup(&backup)?;
            Ok(())
        }
        Err(e) => {
            restore_backup(&backup)?;
            Err(e)
        }
    }
}
```

---

### 4.2 Partial Failure Recovery

**Problem:** `recipe install --deps myapp` installs A, B, then fails on C. A and B are now installed but myapp isn't usable.

**Solution:** Transaction log + resume capability.

```rust
// .recipe-transaction.json
{
    "target": "myapp",
    "plan": ["core", "openssl", "zlib", "curl", "myapp"],
    "completed": ["core", "openssl"],
    "failed": "zlib",
    "error": "checksum mismatch",
    "timestamp": "2025-01-20T10:30:00Z"
}
```

**CLI:**

```bash
recipe install --deps myapp   # Fails at zlib
# User fixes the issue
recipe resume                 # Continue from zlib
# Or:
recipe rollback               # Remove core, openssl that were installed
```

---

### 4.3 Dry Run Mode

**Problem:** User wants to see what would happen without doing it.

**Solution:** `--dry-run` flag.

```bash
$ recipe install --deps --dry-run myapp
Would install (in order):
  1. core (not installed)
  2. openssl >= 3.0 (not installed)
  3. zlib (already installed: 1.2.13 ✓)
  4. curl (not installed)
  5. myapp (not installed)

Total: 4 packages to install, 1 already satisfied
```

---

## Priority 5: Query & Introspection

### 5.1 Enhanced Query Commands

```bash
# Why is this package installed?
$ recipe why openssl
openssl is required by:
  └─ curl (direct)
      └─ myapp (direct, explicitly installed)

# What would break if I remove this?
$ recipe impact openssl
Removing openssl would break:
  - curl (depends on openssl >= 3.0)
  - python (depends on openssl)
  - wget (depends on openssl)

# Show dependency tree
$ recipe tree myapp
myapp 1.0.0
├── curl 8.5.0
│   ├── openssl 3.2.1
│   │   └── zlib 1.2.13
│   └── zlib 1.2.13 (shared)
└── readline 8.2
    └── ncurses 6.4

# Find packages by pattern
$ recipe search "ssl"
openssl 3.2.1 [installed]
  TLS/SSL toolkit
libressl 3.8.0
  OpenBSD fork of OpenSSL
mbedtls 3.5.0
  Lightweight TLS library
```

---

### 5.2 Machine-Readable Output

```bash
recipe list --json
recipe deps myapp --json
recipe info openssl --json
```

```json
{
  "name": "openssl",
  "version": "3.2.1",
  "installed": true,
  "installed_at": 1705326600,
  "deps": ["zlib"],
  "reverse_deps": ["curl", "python", "wget"],
  "files": ["/usr/local/lib/libssl.so.3", "..."]
}
```

---

## Priority 6: Optional Dependencies

### 6.1 Optional/Conditional Dependencies

**Problem:** Some features require optional deps (e.g., `vim` with/without Python support).

**Solution:** Optional deps with feature flags.

```rhai
let name = "vim";
let version = "9.1";
let deps = ["ncurses"];
let optional_deps = #{
    python: ["python3"],
    lua: ["lua"],
    clipboard: ["xclip"],
};
let default_features = ["python"];
```

**CLI:**

```bash
recipe install vim                      # Default features (python)
recipe install vim --features=lua,clipboard
recipe install vim --all-features
recipe install vim --no-default-features
```

---

### 6.2 Build-time vs Runtime Dependencies

**Problem:** Some deps only needed for building, not runtime.

**Solution:** Separate dep types.

```rhai
let deps = ["openssl", "zlib"];           // Runtime deps
let build_deps = ["cmake", "ninja"];       // Build-time only
let check_deps = ["pytest"];               // Test-time only
```

Build deps don't trigger reverse-dep warnings on removal.

---

## Implementation Order

Recommended implementation sequence based on impact/effort ratio:

| Phase | Feature | Effort | Impact |
|-------|---------|--------|--------|
| 1 | Source checksums (1.1) | Low | Critical |
| 2 | Reverse dep tracking (3.1) | Low | High |
| 3 | Dry run mode (4.3) | Low | Medium |
| 4 | Version constraints (2.1) | Medium | High |
| 5 | Atomic install (4.1) | Medium | High |
| 6 | Orphan detection (3.2) | Low | Medium |
| 7 | `recipe tree` / `recipe why` (5.1) | Low | Medium |
| 8 | Lock file (2.3) | Medium | Medium |
| 9 | Transaction log (4.2) | Medium | Medium |
| 10 | Optional deps (6.1) | High | Low |
| 11 | Signatures (1.2) | Medium | Low |

---

## Summary

Current state: Functional foundation with correct core algorithm.

Critical gaps:
1. No integrity verification (security risk)
2. No version constraints (silent breakage risk)
3. No reverse dep tracking (removal breaks things)
4. No atomic install (partial failure risk)

The "Code Over Config" philosophy is sound—these improvements maintain that philosophy while adding necessary safety rails.