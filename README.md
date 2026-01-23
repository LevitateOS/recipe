# recipe

> **STOP. READ. THEN ACT.** Before writing code, read the existing modules. Before deleting anything, read it first. See `STOP_READ_THEN_ACT.md` in project root.

Rhai-based package manager for LevitateOS.

## Status

| Metric | Value |
|--------|-------|
| Stage | Alpha |
| Target | x86_64 Linux |
| Last verified | 2026-01-23 |

### Works

- Recipe parsing and validation
- Lifecycle execution (acquire → build → install)
- Dependency resolution (topological sort)
- State persistence in recipe files
- ~35 helper functions for recipes

### Incomplete / Stubbed

- Recipe repository/index system
- Parallel installation

### Known Issues

- See [GitHub Issues](https://github.com/LevitateOS/recipe/issues)

---

## Author

<!-- HUMAN WRITTEN - DO NOT MODIFY -->

[Waiting for human input]

<!-- END HUMAN WRITTEN -->

---

## Philosophy

**recipe is not like other package managers.**

| Traditional PMs | recipe |
|-----------------|--------|
| Recipes are declarative specs | Recipes are **executable code** |
| State lives in a database (`/var/lib/dpkg`, `/var/lib/pacman`) | State lives **in the recipe file itself** |
| Package manager is complex, recipes are simple | Executor is simple, **recipes do the work** |
| Recipes are read-only | Recipes are **living data files** - the engine writes back to them |

### No Database

There is no `/var/lib/recipe/db`. To know what's installed:

```bash
grep -l "installed = true" recipes/*.rhai
```

The recipe file IS the database entry. When you install a package, the engine writes:

```rhai
let installed = true;
let installed_version = "1.0.0";
let installed_at = "2024-01-15T10:30:00Z";
let installed_files = ["/usr/local/bin/ripgrep", "/usr/local/share/man/man1/rg.1"];
```

...directly into the .rhai file.

### Recipes Are Code

Recipes aren't YAML/TOML/JSON configs. They're Rhai scripts that **run**:

```rhai
let name = "ripgrep";
let version = "14.1.0";
let installed = false;

fn acquire() {
    let url = `https://github.com/BurntSushi/ripgrep/releases/download/${version}/ripgrep-${version}-x86_64-unknown-linux-musl.tar.gz`;
    download(url);
}

fn build() {
    extract("tar.gz");
}

fn install() {
    install_bin(`ripgrep-${version}-x86_64-unknown-linux-musl/rg`);
    install_man(`ripgrep-${version}-x86_64-unknown-linux-musl/doc/rg.1`);
}
```

This means recipes can:
- Use variables, conditionals, loops
- Call helper functions
- Compute URLs dynamically
- Handle edge cases with real logic

### Simple Executor

The Rust engine provides helpers, not policy:

**Acquire phase:** `download()`, `copy()`, `verify_sha256()`
**Build phase:** `extract()`, `cd()`, `run()`
**Install phase:** `install_bin()`, `install_lib()`, `install_man()`, `install_to_dir()`
**Utilities:** `exists()`, `file_exists()`, `dir_exists()`, `mkdir()`, `rm()`, `mv()`, `ln()`, `chmod()`
**IO:** `read_file()`, `glob_list()`
**Environment:** `env()`, `set_env()`
**Commands:** `run_output()`, `run_status()`, `exec()`, `exec_output()`
**HTTP:** `http_get()`, `github_latest_release()`, `github_latest_tag()`, `parse_version()`

Everything else happens in the recipe. The engine doesn't know about configure scripts, make, cmake, meson, cargo, or anything specific. Recipes encode that knowledge.

## Recipe Structure

### Required Variables

Every recipe **must** have these variables:

| Variable | Type | Description |
|----------|------|-------------|
| `name` | String | Package name (non-empty) |
| `version` | String | Package version (non-empty) |
| `installed` | Boolean | Installation status tracking |

### Required Functions

Every recipe **must** have these functions:

| Function | Description |
|----------|-------------|
| `acquire()` | Get source materials (download, copy, etc.) |
| `install()` | Install files to PREFIX |

### Conditional Requirements

When `installed = true`, these variables are **also required**:

| Variable | Type | Description |
|----------|------|-------------|
| `installed_version` | String | Version that was installed |
| `installed_files` | Array | List of installed file paths |

### Optional Variables and Functions

| Item | Type | Description |
|------|------|-------------|
| `description` | String | Package description |
| `deps` | Array | Dependencies (e.g., `["readline", "ncurses"]`) |
| `build()` | Function | Transform source (extract, configure, compile) |
| `is_installed()` | Function | Custom check beyond `installed` variable |
| `check_update()` | Function | Return new version if available |
| `remove()` | Function | Custom uninstall logic |

### Validation

The engine validates recipes before execution:

```
Invalid recipe 'mypackage' (/path/to/mypackage.rhai):
  - missing required variable: `let name = ...;`
  - missing required variable: `let installed = ...;`
  - missing required function: `fn acquire() { ... }`
```

## Lifecycle

```
acquire() → build() → install()
```

1. **acquire()** - Get source/binaries (download, copy, git clone)
2. **build()** - Transform source (extract, configure, compile) - *optional*
3. **install()** - Copy files to PREFIX

Optional hooks:
- **is_installed()** - Custom check beyond `installed` variable
- **check_update()** - Return new version if available
- **remove()** - Custom uninstall logic

### Upgrade Flow

The engine compares `installed_version` with `version`:
- If different, the package is re-installed with the new version
- If same, the upgrade is skipped

```bash
recipe upgrade ripgrep  # Only reinstalls if version differs
```

## Dependencies

Recipes can declare dependencies:

```rhai
let name = "myapp";
let version = "1.0.0";
let installed = false;
let deps = ["readline", "ncurses"];

fn acquire() {}
fn install() {}
```

Dependency commands:

```bash
recipe deps myapp           # Show direct dependencies
recipe deps myapp --resolve # Show full install order (topological sort)
recipe install --deps myapp # Install package with all dependencies
```

## Usage

```bash
recipe install ripgrep      # Run acquire → build → install
recipe install --deps myapp # Install with dependencies
recipe remove ripgrep       # Remove installed files
recipe update ripgrep       # Check for updates
recipe upgrade ripgrep      # Reinstall if newer version in recipe
recipe list                 # Show all recipes and status
recipe info ripgrep         # Show recipe details
recipe search grep          # Find recipes by name/description
recipe deps myapp           # Show dependencies
```

## Variables Available in Recipes

| Variable | Description |
|----------|-------------|
| `PREFIX` | Installation prefix (default: `/usr/local`) |
| `BUILD_DIR` | Temporary build directory |
| `ARCH` | Target architecture (`x86_64`, `aarch64`) |
| `NPROC` | Number of CPU cores |

## Helper Functions Reference

### Acquire Phase

| Function | Description |
|----------|-------------|
| `download(url)` | Download file from URL |
| `copy(pattern)` | Copy files matching glob pattern |
| `verify_sha256(hash)` | Verify last downloaded/copied file |

### Build Phase

| Function | Description |
|----------|-------------|
| `extract(format)` | Extract archive (`tar.gz`, `tar.xz`, `tar.bz2`, `zip`) |
| `cd(dir)` | Change working directory |
| `run(cmd)` | Execute shell command (fails on non-zero exit) |

### Install Phase

| Function | Description |
|----------|-------------|
| `install_bin(pattern)` | Install to `PREFIX/bin` with mode 0755 |
| `install_lib(pattern)` | Install to `PREFIX/lib` with mode 0644 |
| `install_man(pattern)` | Install to `PREFIX/share/man/man{N}` |
| `install_to_dir(pattern, subdir)` | Install to `PREFIX/{subdir}` |
| `install_to_dir(pattern, subdir, mode)` | Install with custom mode |

### Filesystem Utilities

| Function | Description |
|----------|-------------|
| `exists(path)` | Check if path exists (file or dir) |
| `file_exists(path)` | Check if file exists |
| `dir_exists(path)` | Check if directory exists |
| `mkdir(path)` | Create directory (recursive) |
| `rm(path)` | Remove file |
| `mv(src, dst)` | Move/rename file |
| `ln(target, link)` | Create symlink |
| `chmod(path, mode)` | Set file permissions |

### IO and Environment

| Function | Description |
|----------|-------------|
| `read_file(path)` | Read file contents as string |
| `glob_list(pattern)` | List files matching glob pattern |
| `env(name)` | Get environment variable |
| `set_env(name, value)` | Set environment variable |

### Command Execution

| Function | Description |
|----------|-------------|
| `run_output(cmd)` | Run command, return stdout |
| `run_status(cmd)` | Run command, return exit code |
| `exec(program, args)` | Execute program directly, return exit code |
| `exec_output(program, args)` | Execute program, return stdout |

### HTTP Utilities

| Function | Description |
|----------|-------------|
| `http_get(url)` | Fetch URL content as string |
| `github_latest_release(owner, repo)` | Get latest GitHub release tag |
| `github_latest_tag(owner, repo)` | Get latest GitHub tag |
| `parse_version(tag)` | Strip version prefixes (`v`, `release-`, `version-`) |

## Codebase Structure

```
src/
├── bin/recipe.rs       # CLI entry point
├── lib.rs              # Public API, RecipeEngine struct
├── core/               # Infrastructure
│   ├── mod.rs          # Exports
│   ├── lifecycle.rs    # execute, remove, update, upgrade state machines
│   ├── context.rs      # Thread-local execution state
│   ├── recipe_state.rs # Persistent variables (installed, version, etc.)
│   ├── deps.rs         # Dependency resolution (topological sort)
│   ├── lockfile.rs     # Lock file for concurrent access
│   ├── version.rs      # Version parsing and comparison
│   └── output.rs       # Terminal formatting, progress bars
└── helpers/            # Recipe-facing functions (~35 functions)
    ├── mod.rs          # Register all helpers with Rhai engine
    ├── acquire.rs      # download, copy, verify_sha256
    ├── build.rs        # extract, cd, run
    ├── install.rs      # install_bin, install_lib, install_man
    ├── filesystem.rs   # mkdir, rm, mv, ln, chmod, exists
    ├── io.rs           # read_file, glob_list
    ├── env.rs          # get_env, set_env
    ├── command.rs      # run_output, run_status
    ├── http.rs         # http_get, github_*, parse_version
    └── process.rs      # exec, exec_output
```

**core/** is the infrastructure - state machines, dependency resolution, output formatting.

**helpers/** is recipe-facing - every function a recipe author can call.

## Why This Design?

1. **Simplicity** - No database to corrupt, no complex state machine
2. **Transparency** - `cat recipe.rhai` shows exactly what's installed and how
3. **Flexibility** - Code can handle any edge case
4. **Debuggability** - Recipe failing? Add `print()` statements
5. **Version control** - Recipes are just files, track them in git
6. **Validation** - Invalid recipes fail loudly with clear error messages

## Comparison with Other Package Managers

| Feature | apt/dnf | pacman | Homebrew | recipe |
|---------|---------|--------|----------|--------|
| State storage | `/var/lib/*` DB | `/var/lib/pacman` | SQLite + git | In recipe files |
| Recipe format | Control files | PKGBUILD (bash) | Ruby DSL | Rhai scripts |
| Recipe mutability | Read-only | Read-only | Read-only | **Read-write** |
| Executor complexity | High | Medium | High | **Simple** |
| Remote repos | Required | Required | Required | **Optional** |
| Validation | Limited | Limited | Limited | **Strict** |

## License

MIT
