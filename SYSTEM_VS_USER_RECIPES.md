# System vs User Recipes

LevitateOS distinguishes between **system recipes** (base OS packages) and **user recipes** (user-installed software).

## System Recipes

**Location:** `/etc/recipe/repos/rocky10/`

System recipes are:
- Generated automatically at build time from Rocky Linux RPMs
- Installed system-wide in `/usr/bin`, `/usr/sbin`, `/usr/lib64`, etc.
- Managed by root/administrator
- Shared by all users
- RPM shims that download from Rocky mirrors

Example: `/etc/recipe/repos/rocky10/less.rhai`

```rhai
let name = "less";
let version = "661";
let release = "3.el10";
let installed = true;
let installed_files = ["/usr/bin/less", ...];

fn check_update() {
    // Check Rocky repodata for newer version
    rocky_check_update(name, mirror, repo)
}
```

### Why RPM Shims?

Rocky Linux is a slow-moving enterprise distro with stable, well-tested packages. For BASE system packages, there's no benefit to building from source - we'd just be recreating the same binaries with more work and more room for error.

RPM shims:
- Download pre-built binaries from Rocky mirrors
- Track installed version and files
- Enable `recipe update` to check Rocky repodata
- Enable `recipe upgrade` to install newer versions

## User Recipes

**Location:** `~/.local/share/recipe/recipes/`

User recipes are:
- Written by users or from community repositories
- Installed per-user in `~/.local/bin`, `~/.local/lib`, etc.
- Managed by individual users
- Build from upstream source (real recipes, not shims)

Example: `~/.local/share/recipe/recipes/ripgrep.rhai`

```rhai
let name = "ripgrep";
let version = "14.1.0";
let source = "https://github.com/BurntSushi/ripgrep/releases/...";

fn build() {
    // Build from source with cargo
    run("cargo build --release");
}
```

## Directory Structure

```
/etc/recipe/                       # SYSTEM (root-managed)
├── recipe.conf
└── repos/
    └── rocky10/                   # Base packages (RPM shims)
        ├── bash.rhai
        ├── coreutils.rhai
        ├── less.rhai
        └── ... (~209 packages)

~/.local/share/recipe/             # USER (per-user)
└── recipes/
    ├── ripgrep.rhai               # User installs things here
    └── fd.rhai
```

## Search Order

When `recipe` looks for a package:

1. User recipes (`~/.local/share/recipe/recipes/`)
2. System recipes (`/etc/recipe/repos/rocky10/`)

This allows users to override system packages with custom versions if needed.

## Commands

```bash
# List all installed packages (system + user)
recipe list

# Show package info
recipe info less

# Check for updates (Phase 2)
recipe update less

# Upgrade to newer version (Phase 2)
recipe upgrade less
```

## Implementation Notes

### Build Time (leviso)

1. `RpmExtractor` extracts RPM contents to staging
2. `RecipeGenerator` creates `.rhai` files from RPM metadata
3. Recipes placed in `staging/etc/recipe/repos/rocky10/`

### Runtime Queries

1. `recipe list` reads all `.rhai` files in `RECIPE_PATH`
2. Files with `installed = true` are shown as installed
3. `installed_files` array lists what files belong to the package

### Updates (Phase 2)

1. Fetch Rocky repodata (`repomd.xml`, `primary.xml.gz`)
2. Parse for package version
3. Compare with `installed_version` in recipe
4. If newer: download RPM, extract, update recipe state
