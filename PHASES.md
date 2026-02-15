# Recipe Lifecycle Phases

This document explains the **order** and **reasoning** behind the recipe execution phases as
implemented by `tools/recipe` today.

## Phase Order

```
┌─────────────────────────────────────────────────────────────────┐
│  1. is_installed(ctx)  →  Skip early if already done            │
│           ↓                                                     │
│  2. acquire(ctx)       →  Get source materials                  │
│           ↓                                                     │
│  3. build(ctx)         →  Compile/transform (optional)          │
│           ↓                                                     │
│  4. install(ctx)       →  Copy outputs to destination paths     │
└─────────────────────────────────────────────────────────────────┘
```

Notes:

- Recipes use a **ctx-map** pattern: `let ctx = #{ ... };` holds state, and each phase function
  takes `ctx` and returns the updated `ctx`.
- The `is_*` functions are **checks**: if a check function exists and does **not** throw, the
  engine considers that phase satisfied. If it throws, the phase runs.

## Phase Details

### 1. `is_installed(ctx)` - Guard Check (Optional)

**Purpose:** Skip the entire recipe if the package is already installed.

**Why First:**
- Avoids wasted downloads and build time
- Idempotency - running the recipe twice has the same result
- Reduces network load and disk churn

**Example (throw means "not installed"):**
```rhai
fn is_installed(ctx) {
    if !ctx.installed { throw "not installed"; }
    ctx
}
```

### 2. `acquire(ctx)` - Source Acquisition Phase (Required)

**Purpose:** Get the raw materials needed to build the package.

**Why Second:**
- Nothing can be built without source materials
- Downloads can fail - fail fast before spending time on build setup
- Checksums can be verified before extraction

**Helpers (current implementation):**
- `download(url, dest) -> String`
- `verify_sha256(path, expected) -> ()`
- `http_get(url) -> String` (for fetching remote checksums/metadata)
- `check_disk_space(path, bytes) -> ()`

**Example:**
```rhai
fn is_acquired(ctx) {
    if ctx.archive == "" || !is_file(ctx.archive) { throw "not acquired"; }
    ctx
}

fn acquire(ctx) {
    mkdir(BUILD_DIR);
    let archive = download(ctx.url, join_path(BUILD_DIR, "src.tar.gz"));
    verify_sha256(archive, ctx.sha256);
    ctx.archive = archive;
    ctx
}
```

### 3. `build(ctx)` - Compilation Phase (Optional)

**Purpose:** Transform source into installable artifacts.

**Why Third:**
- Requires source from acquire phase
- May produce artifacts in temporary locations
- Can be skipped for pre-built binaries (RPMs, static binaries)

**Helpers (current implementation):**
- `extract(archive, dest) -> ()` (auto-detect format)
- `shell(cmd) -> ()` / `shell_in(dir, cmd) -> ()`
- `exec(cmd, args) -> int` / `exec_output(cmd, args) -> String`

**Example:**
```rhai
fn is_built(ctx) {
    if ctx.src_dir == "" || !is_dir(ctx.src_dir) { throw "not built"; }
    ctx
}

fn build(ctx) {
    extract(ctx.archive, BUILD_DIR);
    ctx.src_dir = join_path(BUILD_DIR, "ripgrep-" + ctx.version);
    ctx
}
```

### 4. `install(ctx)` - Installation Phase (Required)

**Purpose:** Copy/move final artifacts to their destination paths (often under a prefix/sysroot).

**Why Last:**
- Requires built/extracted artifacts from previous phases
- Final step - only runs if everything succeeded
- Modifications should be atomic (A/B/sysroot composition adds more rules; see `tools/recipe/REQUIREMENTS.md`)

**Helpers (current implementation):**
- Filesystem: `mkdir`, `mv`, `ln`, `chmod`, `rm`, `write_file`, etc.
- Or run an installer via `shell`/`shell_in`.

**Example:**
```rhai
fn install(ctx) {
    let bin_dir = join_path(ctx.prefix, "bin");
    mkdir(bin_dir);

    let src = join_path(ctx.src_dir, "rg");
    let dst = join_path(bin_dir, "rg");
    mv(src, dst);
    chmod(dst, 0o755);

    ctx.installed = true;
    ctx
}
```

## Why This Order?

### Fail-Fast Principle

Each phase validates prerequisites before proceeding:

1. **is_installed** → Don't start if unnecessary
2. **acquire** → Don't build if source unavailable
3. **build** → Don't install if compilation fails
4. **install** → Success only after all steps complete

### Separation of Concerns

Each phase has a clear responsibility:

| Phase | Input | Output | Side Effects |
|-------|-------|--------|--------------|
| is_installed | ctx + filesystem | ctx | none |
| acquire | URLs/paths | files in BUILD_DIR | network, disk |
| build | acquired inputs | artifacts in BUILD_DIR | disk, CPU |
| install | artifacts | files in destination paths | destination modified |

### Transactional Safety

- Destination paths are only modified in the final `install()` phase
- If any earlier phase fails, destination paths remain unchanged
- Makes rollback trivial (just re-run with clean BUILD_DIR)

## Phase Dependencies

```
is_installed: reads ctx + filesystem
         ↓
   acquire: writes BUILD_DIR
         ↓
     build: reads/writes BUILD_DIR
         ↓
   install: writes destination paths (often derived from ctx/defines)
```

## File Layout

```
tools/recipe/src/
├── bin/recipe.rs         # CLI wrapper
├── core/executor.rs      # phase orchestration
├── core/ctx.rs           # ctx block persistence
└── helpers/              # Rhai helper API (see HELPERS_AUDIT.md)
```
