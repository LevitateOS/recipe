# recipe

Package manager for LevitateOS. Recipes are Rhai scripts. State is stored in the recipe files themselves.

## Status

**Alpha.** Works for basic package installation. No repository system.

| Works | Doesn't work yet |
|-------|------------------|
| Recipe parsing + validation | Central package repository |
| `acquire → build → install` lifecycle | Parallel installation |
| Dependency resolution (toposort) | Conflict detection |
| State persistence in `.rhai` files | Rollback/undo |
| Lock file for reproducible installs | |

## How It Works

Recipes are executable Rhai scripts:

```rhai
let name = "ripgrep";
let version = "14.1.0";
let installed = false;

fn acquire() {
    download(`https://github.com/.../ripgrep-${version}-x86_64-unknown-linux-musl.tar.gz`);
}

fn install() {
    extract("tar.gz");
    install_bin(`ripgrep-${version}-x86_64-unknown-linux-musl/rg`);
}
```

When you run `recipe install ripgrep`:

1. Engine loads and validates the recipe
2. Calls `acquire()` → `build()` (optional) → `install()`
3. Writes `installed = true` and `installed_files = [...]` back to the `.rhai` file

No database. The recipe file IS the package state.

## Commands

### Package Management

```bash
recipe install ripgrep        # Install package
recipe install -d ripgrep     # Install with dependencies
recipe install -n ripgrep     # Dry run (show what would install)
recipe remove ripgrep         # Remove package
recipe remove -f ripgrep      # Force remove (ignore dependents)
```

### Updates

```bash
recipe update                 # Check all packages for updates
recipe update ripgrep         # Check specific package
recipe upgrade                # Upgrade all packages with updates
recipe upgrade ripgrep        # Upgrade specific package
```

### Information

```bash
recipe list                   # Show all recipes + install status
recipe search pattern         # Search recipes by name
recipe info ripgrep           # Show package details
recipe deps ripgrep           # Show direct dependencies
recipe deps --resolve ripgrep # Show full install order
recipe tree ripgrep           # Show dependency tree
recipe why ripgrep            # Show what depends on this package
recipe impact ripgrep         # Show what would break if removed
```

### Maintenance

```bash
recipe orphans                # List orphaned dependencies
recipe autoremove             # Show what would be removed
recipe autoremove --yes       # Actually remove orphans
recipe hash /path/to/file     # Compute sha256/sha512/blake3 hashes
```

### Lock Files

```bash
recipe lock update            # Generate/update recipe.lock
recipe lock show              # Show locked versions
recipe lock verify            # Verify recipes match lock file
recipe install --locked pkg   # Install only if versions match lock
```

## Recipe Requirements

Required variables:
- `name` (String)
- `version` (String)
- `installed` (Boolean)

Required functions:
- `acquire()` - Download/copy source
- `install()` - Install to PREFIX

Optional:
- `build()` - Extract, configure, compile
- `deps` (Array) - Dependencies
- `remove()` - Custom uninstall logic
- `update()` - Check for newer version

## Helper Functions

### Acquire Phase

| Function | Description |
|----------|-------------|
| `download(url)` | Download file |
| `copy(pattern)` | Copy files matching glob |
| `verify_sha256(hash)` | Verify last file (SHA-256) |
| `verify_sha512(hash)` | Verify last file (SHA-512) |
| `verify_blake3(hash)` | Verify last file (BLAKE3) |

### Build Phase

| Function | Description |
|----------|-------------|
| `extract(format)` | Extract tar.gz, tar.xz, tar.bz2, zip |
| `cd(dir)` | Change directory |
| `run(cmd)` | Execute shell command |
| `shell(cmd)` | Alias for `run()` (use when recipe defines own `run()`) |

### Install Phase

| Function | Description |
|----------|-------------|
| `install_bin(pattern)` | Install to PREFIX/bin (0755) |
| `install_lib(pattern)` | Install to PREFIX/lib (0644) |
| `install_man(pattern)` | Install to PREFIX/share/man |
| `install_to_dir(pattern, subdir)` | Install to PREFIX/subdir |
| `install_to_dir(pattern, subdir, mode)` | Install with specific permissions |
| `rpm_install(pattern)` | Extract and install from RPM |

### Filesystem

| Function | Description |
|----------|-------------|
| `exists(path)` | Check if path exists |
| `file_exists(path)` | Check if file exists |
| `dir_exists(path)` | Check if directory exists |
| `mkdir(path)` | Create directory |
| `rm(pattern)` | Remove files matching glob |
| `mv(src, dst)` | Move file |
| `ln(target, link)` | Create symlink |
| `chmod(path, mode)` | Set permissions |
| `read_file(path)` | Read file contents |
| `glob_list(pattern)` | List files matching glob |

### Environment

| Function | Description |
|----------|-------------|
| `env(name)` | Get env var |
| `set_env(name, value)` | Set env var |

### Commands

| Function | Description |
|----------|-------------|
| `run_output(cmd)` | Run command, return stdout |
| `run_status(cmd)` | Run command, return exit code |
| `exec(cmd)` | Execute command directly |
| `exec_output(cmd)` | Execute command, return stdout |

### HTTP / Version Checking

| Function | Description |
|----------|-------------|
| `http_get(url)` | Fetch URL as string |
| `github_latest_release(owner, repo)` | Get latest release tag |
| `github_latest_tag(owner, repo)` | Get latest git tag |
| `parse_version(string)` | Extract version from string |

## Variables Available in Recipes

| Variable | Description |
|----------|-------------|
| `PREFIX` | Install prefix (default: `/usr/local`) |
| `BUILD_DIR` | Temp build directory |
| `ARCH` | Architecture (`x86_64`, `aarch64`) |
| `NPROC` | CPU core count |
| `RPM_PATH` | Path to RPM repository (from environment) |

## Code Structure

```
src/
├── bin/recipe.rs       # CLI (15 commands)
├── lib.rs              # RecipeEngine API
├── core/
│   ├── lifecycle.rs    # install/remove/upgrade logic
│   ├── deps.rs         # Dependency resolution
│   ├── recipe_state.rs # State persistence
│   ├── lockfile.rs     # Lock file handling
│   ├── version.rs      # Version comparison
│   ├── context.rs      # Execution context
│   └── output.rs       # Terminal formatting
└── helpers/            # 33 helper functions
    ├── acquire.rs      # download, copy, verify_*
    ├── build.rs        # extract, cd, run
    ├── install.rs      # install_bin, install_lib, rpm_install
    ├── filesystem.rs   # mkdir, rm, mv, ln, chmod
    ├── http.rs         # http_get, github_*
    └── ...
```

## Known Limitations

- No central repository - you maintain recipes locally
- No conflict detection between packages
- No rollback if install fails midway
- Single-threaded installation

## Building

```bash
cargo build --release
```

## License

MIT
