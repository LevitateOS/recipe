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

## Usage

```bash
recipe install ripgrep      # Install package
recipe remove ripgrep       # Remove package (deletes installed_files)
recipe list                 # Show all recipes + status
recipe info ripgrep         # Show recipe details
recipe deps myapp           # Show dependencies
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

## Helper Functions

### Acquire Phase

| Function | Description |
|----------|-------------|
| `download(url)` | Download file |
| `copy(pattern)` | Copy files matching glob |
| `verify_sha256(hash)` | Verify last file |

### Build Phase

| Function | Description |
|----------|-------------|
| `extract(format)` | Extract tar.gz, tar.xz, zip |
| `cd(dir)` | Change directory |
| `run(cmd)` | Execute shell command |

### Install Phase

| Function | Description |
|----------|-------------|
| `install_bin(pattern)` | Install to PREFIX/bin (0755) |
| `install_lib(pattern)` | Install to PREFIX/lib (0644) |
| `install_man(pattern)` | Install to PREFIX/share/man |
| `install_to_dir(pattern, subdir)` | Install to PREFIX/subdir |

### Utilities

| Function | Description |
|----------|-------------|
| `exists(path)` | Check if path exists |
| `mkdir(path)` | Create directory |
| `rm(path)` | Remove file |
| `mv(src, dst)` | Move file |
| `ln(target, link)` | Create symlink |
| `chmod(path, mode)` | Set permissions |
| `env(name)` | Get env var |
| `run_output(cmd)` | Run command, return stdout |
| `http_get(url)` | Fetch URL as string |
| `github_latest_release(owner, repo)` | Get latest release tag |

Full list: 35 functions. See `src/helpers/` for implementation.

## Variables Available in Recipes

| Variable | Description |
|----------|-------------|
| `PREFIX` | Install prefix (default: `/usr/local`) |
| `BUILD_DIR` | Temp build directory |
| `ARCH` | Architecture (`x86_64`, `aarch64`) |
| `NPROC` | CPU core count |

## Dependencies

```rhai
let deps = ["readline", "ncurses"];
```

```bash
recipe install --deps myapp  # Install with dependencies (toposort order)
```

## Code Structure

```
src/
├── bin/recipe.rs       # CLI
├── lib.rs              # RecipeEngine API
├── core/
│   ├── lifecycle.rs    # install/remove/upgrade logic
│   ├── deps.rs         # Dependency resolution
│   └── recipe_state.rs # State persistence
└── helpers/            # 35 helper functions
    ├── acquire.rs
    ├── build.rs
    ├── install.rs
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
