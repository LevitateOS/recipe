# Recipe Lifecycle Phases

This document explains the **order** and **reasoning** behind the recipe execution phases.

## Phase Order

```
┌─────────────────────────────────────────────────────────────────┐
│  1. is_installed()  →  Skip early if already done               │
│           ↓                                                     │
│  2. acquire()       →  Get source materials                     │
│           ↓                                                     │
│  3. build()         →  Compile/transform (optional)             │
│           ↓                                                     │
│  4. install()       →  Copy outputs to PREFIX                   │
└─────────────────────────────────────────────────────────────────┘
```

## Phase Details

### 1. `is_installed()` - Guard Phase (Optional)

**Purpose:** Skip the entire recipe if the package is already installed.

**Why First:**
- Avoids wasted downloads and build time
- Idempotency - running the recipe twice has the same result
- Reduces network load and disk churn

**Example:**
```rhai
fn is_installed() {
    file_exists(`${PREFIX}/bin/ripgrep`)
}
```

### 2. `acquire()` - Source Acquisition Phase (Required)

**Purpose:** Get the raw materials needed to build the package.

**Why Second:**
- Nothing can be built without source materials
- Downloads can fail - fail fast before spending time on build setup
- Checksums can be verified before extraction

**Helpers:**
- `download(url)` - Download from URL, sets `last_downloaded`
- `copy(pattern)` - Copy local files matching glob, sets `last_downloaded`
- `verify_sha256(hash)` - Verify integrity of `last_downloaded`

**Example:**
```rhai
fn acquire() {
    download("https://github.com/BurntSushi/ripgrep/releases/download/14.1.0/ripgrep-14.1.0-x86_64-unknown-linux-musl.tar.gz");
    verify_sha256("abc123...");
}
```

### 3. `build()` - Compilation Phase (Optional)

**Purpose:** Transform source into installable artifacts.

**Why Third:**
- Requires source from acquire phase
- May produce artifacts in temporary locations
- Can be skipped for pre-built binaries (RPMs, static binaries)

**Helpers:**
- `extract(format)` - Unpack archive (tar.gz, tar.xz, zip, etc.)
- `cd(dir)` - Change working directory
- `run(cmd)` - Execute shell command

**Example:**
```rhai
fn build() {
    extract("tar.gz");
    cd("ripgrep-14.1.0-x86_64-unknown-linux-musl");
    // No compilation needed for pre-built binary
}
```

### 4. `install()` - Installation Phase (Required)

**Purpose:** Copy final artifacts to their destination in PREFIX.

**Why Last:**
- Requires built/extracted artifacts from previous phases
- Final step - only runs if everything succeeded
- Modifications to PREFIX should be atomic

**Helpers:**
- `install_bin(pattern)` - Install to `PREFIX/bin` with 0755 permissions
- `install_lib(pattern)` - Install to `PREFIX/lib` with 0644 permissions
- `install_man(pattern)` - Install to `PREFIX/share/man/manN/`
- `rpm_install()` - Extract RPM contents to PREFIX

**Example:**
```rhai
fn install() {
    install_bin("rg");
    install_man("doc/*.1");
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
| is_installed | PREFIX state | boolean | none |
| acquire | URLs/paths | files in BUILD_DIR | network, disk |
| build | source files | artifacts | disk, CPU |
| install | artifacts | files in PREFIX | PREFIX modified |

### Transactional Safety

- PREFIX is only modified in the final `install()` phase
- If any earlier phase fails, PREFIX remains unchanged
- Makes rollback trivial (just re-run with clean BUILD_DIR)

## Phase Dependencies

```
is_installed: reads PREFIX
         ↓
   acquire: writes BUILD_DIR, sets last_downloaded
         ↓
     build: reads BUILD_DIR, writes BUILD_DIR, uses current_dir
         ↓
   install: reads BUILD_DIR/current_dir, writes PREFIX
```

## File Layout

```
recipe/src/engine/
├── phases/
│   ├── acquire.rs    # download, copy, verify_sha256
│   ├── build.rs      # extract, cd, run
│   └── install.rs    # install_bin, install_lib, install_man, rpm_install
├── lifecycle.rs      # Orchestrates phase execution order
└── context.rs        # Shared state (PREFIX, BUILD_DIR, current_dir, last_downloaded)
```
