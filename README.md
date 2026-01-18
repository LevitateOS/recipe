# recipe

Rhai-based package manager for LevitateOS.

## Philosophy

**recipe is not like other package managers.**

| Traditional PMs | recipe |
|-----------------|--------|
| Recipes are declarative specs | Recipes are **executable code** |
| State lives in a database (`/var/lib/dpkg`, `/var/lib/pacman`) | State lives **in the recipe file itself** |
| Package manager is complex, recipes are simple | Executor is minimal, **recipes do the work** |
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

### Minimal Executor

The Rust engine provides helpers, not policy:

**Acquire phase:** `download()`, `copy()`, `verify_sha256()`
**Build phase:** `extract()`, `cd()`, `run()`
**Install phase:** `install_bin()`, `install_lib()`, `install_man()`
**Utilities:** `exists()`, `mkdir()`, `rm()`, `mv()`, `ln()`, `chmod()`
**HTTP:** `http_get()`, `github_latest_release()`, `github_latest_tag()`

Everything else happens in the recipe. The engine doesn't know about configure scripts, make, cmake, meson, cargo, or anything specific. Recipes encode that knowledge.

### Everything Should Be Possible

If a recipe needs to:
- Download from a weird FTP server with custom auth
- Patch source code before building
- Install to non-standard locations
- Run post-install scripts
- Check if dependencies are met

...it can. It's just code.

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

## Usage

```bash
recipe install ripgrep      # Run acquire → build → install
recipe remove ripgrep       # Remove installed files
recipe update ripgrep       # Check for updates
recipe upgrade ripgrep      # Reinstall if newer version in recipe
recipe list                 # Show all recipes and status
recipe info ripgrep         # Show recipe details
recipe search grep          # Find recipes by name/description
```

## Variables Available in Recipes

| Variable | Description |
|----------|-------------|
| `PREFIX` | Installation prefix (default: `/usr/local`) |
| `BUILD_DIR` | Temporary build directory |
| `ARCH` | Target architecture (`x86_64`, `aarch64`) |
| `NPROC` | Number of CPU cores |

## Why This Design?

1. **Simplicity** - No database to corrupt, no complex state machine
2. **Transparency** - `cat recipe.rhai` shows exactly what's installed and how
3. **Flexibility** - Code can handle any edge case
4. **Debuggability** - Recipe failing? Add `print()` statements
5. **Version control** - Recipes are just files, track them in git

## Comparison with Other Package Managers

| Feature | apt/dnf | pacman | Homebrew | recipe |
|---------|---------|--------|----------|--------|
| State storage | `/var/lib/*` DB | `/var/lib/pacman` | SQLite + git | In recipe files |
| Recipe format | Control files | PKGBUILD (bash) | Ruby DSL | Rhai scripts |
| Recipe mutability | Read-only | Read-only | Read-only | **Read-write** |
| Executor complexity | High | Medium | High | **Minimal** |
| Remote repos | Required | Required | Required | **Optional** |

## License

MIT
