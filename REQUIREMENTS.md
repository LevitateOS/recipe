# Recipe Package Manager Requirements

**Version:** 1.0.0
**Status:** Specification
**Last Updated:** 2026-01-25

This document defines the complete requirements for the `recipe` package manager.
It is implementation and language agnostic. Any conforming implementation MUST
satisfy all requirements marked as MUST/SHALL. Requirements marked SHOULD are
strongly recommended. Requirements marked MAY are optional enhancements.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Design Principles](#2-design-principles)
3. [Recipe File Format](#3-recipe-file-format)
4. [Lifecycle Phases](#4-lifecycle-phases)
5. [CLI Interface](#5-cli-interface)
6. [Helper Functions](#6-helper-functions)
7. [State Management](#7-state-management)
8. [Dependency Resolution](#8-dependency-resolution)
9. [Lockfile System](#9-lockfile-system)
10. [Output and Progress](#10-output-and-progress)
11. [Error Handling](#11-error-handling)
12. [Security Considerations](#12-security-considerations)
13. [Environment Variables](#13-environment-variables)
14. [File Permissions](#14-file-permissions)
15. [Network Operations](#15-network-operations)
16. [Archive Handling](#16-archive-handling)
17. [Hash Verification](#17-hash-verification)
18. [Git Operations](#18-git-operations)
19. [Atomic Installation](#19-atomic-installation)
20. [Conformance Testing](#20-conformance-testing)

---

## 1. Overview

### 1.1 Purpose

Recipe is a package manager for LevitateOS that executes user-written scripts
to acquire, build, and install software packages. Unlike traditional package
managers that consume pre-built binary packages, Recipe executes procedural
scripts that define HOW to obtain and install software.

### 1.2 Scope

This specification covers:
- Recipe file format and syntax
- Lifecycle phase execution
- Command-line interface
- Helper function behavior
- State persistence
- Dependency resolution
- Error handling

### 1.3 Definitions

| Term | Definition |
|------|------------|
| Recipe | A script file defining how to install a package |
| Package | A unit of software identified by name and version |
| Phase | A distinct step in the install lifecycle (acquire/build/install) |
| Helper | A built-in function available to recipe scripts |
| Prefix | The installation destination directory |
| Build Directory | Temporary workspace for downloads and compilation |
| Staging | Isolated directory where files are installed before commit |

### 1.4 Goals

1. **Transparency**: Users can read exactly what a package does
2. **Reproducibility**: Same recipe produces same result
3. **Flexibility**: Any installation procedure can be scripted
4. **Safety**: Failed installations do not corrupt the system
5. **Simplicity**: Minimal API surface for recipe authors

---

## 2. Design Principles

### 2.1 Phase Separation

**REQ-DESIGN-001**: The installation process MUST be divided into exactly
three phases: acquire, build, and install.

**REQ-DESIGN-002**: Each phase MUST be independently re-runnable without
side effects on other phases.

**REQ-DESIGN-003**: Phase execution order MUST be: acquire → build → install.

**REQ-DESIGN-004**: The build phase MUST be optional. Implementations MUST
NOT require recipes to define a build phase.

**Rationale**: Phase separation enables caching, debugging, and recovery.
A failed build can be retried without re-downloading sources.

### 2.2 Fail-Fast

**REQ-DESIGN-005**: When a required operation fails, execution MUST stop
immediately with an error.

**REQ-DESIGN-006**: Implementations MUST NOT silently continue after errors.

**REQ-DESIGN-007**: Warnings MUST be distinguishable from errors. Warnings
MUST NOT halt execution.

### 2.3 Idempotency

**REQ-DESIGN-008**: Running the same recipe multiple times MUST produce
the same end state.

**REQ-DESIGN-009**: If a package is already installed at the requested
version, the install command MUST skip all phases.

**REQ-DESIGN-010**: Helper functions SHOULD be idempotent where possible.

### 2.4 Atomicity

**REQ-DESIGN-011**: A failed installation MUST NOT leave partial files
in the prefix directory.

**REQ-DESIGN-012**: Either all files are installed, or none are installed.

**REQ-DESIGN-013**: State persistence MUST use atomic write operations.

### 2.5 Minimal Dependencies

**REQ-DESIGN-014**: The recipe tool MUST NOT require packages it installs
to function (no circular dependencies on itself).

**REQ-DESIGN-015**: External tool requirements (tar, unzip, git) MUST be
documented and kept minimal.

---

## 3. Recipe File Format

### 3.1 File Structure

**REQ-FORMAT-001**: Recipe files MUST use the `.rhai` extension or an
implementation-defined extension for the scripting language.

**REQ-FORMAT-002**: Recipe files MUST be valid UTF-8 text.

**REQ-FORMAT-003**: Recipe files MUST be parseable as both:
- Executable scripts (for lifecycle functions)
- Data files (for metadata extraction without execution)

### 3.2 Required Metadata

**REQ-FORMAT-004**: Every recipe MUST define the following variables:

| Variable | Type | Description |
|----------|------|-------------|
| `name` | String | Package identifier (lowercase, alphanumeric, hyphens) |
| `version` | String | Package version (semantic versioning recommended) |
| `installed` | Boolean | Installation state (initially false) |

**REQ-FORMAT-005**: The `name` variable MUST match the pattern:
`^[a-z][a-z0-9]*(-[a-z0-9]+)*$`

**REQ-FORMAT-006**: The `version` variable SHOULD follow semantic versioning
(MAJOR.MINOR.PATCH) but MAY use any string format.

### 3.3 Optional Metadata

**REQ-FORMAT-007**: Recipes MAY define the following optional variables:

| Variable | Type | Default | Description |
|----------|------|---------|-------------|
| `description` | String | "" | Human-readable package description |
| `deps` | Array | [] | List of dependency specifications |
| `installed_version` | String/None | None | Currently installed version |
| `installed_at` | Integer | 0 | Unix timestamp of installation |
| `installed_files` | Array | [] | List of installed file paths |
| `installed_as_dep` | Boolean | false | Whether installed as dependency |

### 3.4 Required Functions

**REQ-FORMAT-008**: Every recipe MUST define the following functions:

```
fn acquire()  // Download or copy source materials
fn install()  // Copy files to prefix
```

**REQ-FORMAT-009**: The acquire function MUST be called before install.

**REQ-FORMAT-010**: Functions MUST NOT return values (void return type).

### 3.5 Optional Functions

**REQ-FORMAT-011**: Recipes MAY define the following optional functions:

| Function | Purpose | When Called |
|----------|---------|-------------|
| `build()` | Extract, compile, transform | After acquire, before install |
| `is_installed()` | Custom installation check | Before any phase |
| `check_update()` | Query for newer versions | During update command |
| `remove()` | Custom removal logic | During remove command |
| `pre_install()` | Hook before install phase | After build, before install |
| `post_install()` | Hook after install phase | After install completes |
| `pre_remove()` | Hook before removal | Before remove starts |
| `post_remove()` | Hook after removal | After files deleted |

### 3.6 Variables Available in Functions

**REQ-FORMAT-012**: The following variables MUST be available in all functions:

| Variable | Type | Description |
|----------|------|-------------|
| `PREFIX` | String | Installation prefix path |
| `BUILD_DIR` | String | Temporary build directory path |
| `ARCH` | String | Target architecture (x86_64, aarch64, etc.) |
| `NPROC` | Integer | Number of CPU cores |

**REQ-FORMAT-013**: Additional environment-derived variables MAY be available:

| Variable | Source | Description |
|----------|--------|-------------|
| `RPM_PATH` | $RPM_PATH | Path to RPM repository |

### 3.7 Example Recipe

```
let name = "example";
let version = "1.0.0";
let description = "An example package";
let deps = ["dependency-a", "dependency-b >= 2.0"];
let installed = false;

fn acquire() {
    download("https://example.com/example-1.0.0.tar.gz");
    verify_sha256("abc123def456...");
}

fn build() {
    extract("tar.gz");
    cd("example-1.0.0");
    run("./configure --prefix=" + PREFIX);
    run("make -j" + NPROC);
}

fn install() {
    run("make install");
}

fn check_update() {
    github_latest_release("example/example")
}
```

---

## 4. Lifecycle Phases

### 4.1 Phase Overview

```
┌──────────────────────────────────────────────────────────────┐
│                      INSTALLATION FLOW                        │
├──────────────────────────────────────────────────────────────┤
│                                                              │
│  is_installed() ──yes──> SKIP (already installed)           │
│       │                                                      │
│       no                                                     │
│       │                                                      │
│       v                                                      │
│  ┌─────────┐    ┌─────────┐    ┌─────────────┐              │
│  │ ACQUIRE │───>│  BUILD  │───>│ PRE_INSTALL │              │
│  └─────────┘    └─────────┘    └─────────────┘              │
│       │              │               │                       │
│       │         (optional)           │                       │
│       │              │               v                       │
│       │              │         ┌─────────┐                   │
│       │              └────────>│ INSTALL │                   │
│       │                        └─────────┘                   │
│       │                              │                       │
│       │                              v                       │
│       │                        ┌──────────────┐              │
│       │                        │ POST_INSTALL │              │
│       │                        └──────────────┘              │
│       │                              │                       │
│       │                              v                       │
│       │                        ┌──────────┐                  │
│       └───────────────────────>│  COMMIT  │                  │
│                                └──────────┘                  │
│                                      │                       │
│                                      v                       │
│                                 UPDATE STATE                 │
│                                                              │
└──────────────────────────────────────────────────────────────┘
```

### 4.2 Installation Check

**REQ-PHASE-001**: Before executing any phase, implementations MUST check
if the package is already installed.

**REQ-PHASE-002**: If `is_installed()` function exists:
- Call it and use return value
- True = skip installation
- False = proceed with phases

**REQ-PHASE-003**: If `is_installed()` does not exist:
- Check `installed` variable
- True = skip installation
- False = proceed with phases

**REQ-PHASE-004**: Implementations SHOULD compare `installed_version` to
`version` to detect upgrades.

### 4.3 Acquire Phase

**REQ-PHASE-010**: The acquire phase MUST download or copy all source
materials needed for installation.

**REQ-PHASE-011**: Files downloaded during acquire MUST be placed in
`BUILD_DIR`.

**REQ-PHASE-012**: The acquire phase SHOULD verify integrity of downloaded
files using hash verification.

**REQ-PHASE-013**: The acquire phase MUST NOT modify the prefix directory.

**REQ-PHASE-014**: If acquire fails, no state changes MUST persist.

### 4.4 Build Phase

**REQ-PHASE-020**: The build phase is OPTIONAL.

**REQ-PHASE-021**: If defined, build MUST execute after acquire and before
install.

**REQ-PHASE-022**: Build operations MUST occur within `BUILD_DIR`.

**REQ-PHASE-023**: The build phase MUST NOT directly modify the prefix
directory (use install phase for that).

**REQ-PHASE-024**: Compilation commands SHOULD use `NPROC` for parallelism.

### 4.5 Install Phase

**REQ-PHASE-030**: The install phase MUST copy files to the prefix directory.

**REQ-PHASE-031**: During the install phase, `PREFIX` MUST point to a
staging directory, NOT the actual prefix.

**REQ-PHASE-032**: All files created in `PREFIX` during install MUST be
tracked for state management.

**REQ-PHASE-033**: The install phase MUST be the ONLY phase that creates
files in the prefix.

### 4.6 Hooks

**REQ-PHASE-040**: `pre_install()` hook, if defined, MUST execute after
build and before install.

**REQ-PHASE-041**: `post_install()` hook, if defined, MUST execute after
install but before commit.

**REQ-PHASE-042**: Hook failures MUST abort the installation.

**REQ-PHASE-043**: Hooks MUST have access to the same helpers as phases.

### 4.7 Commit

**REQ-PHASE-050**: After successful install phase, staged files MUST be
committed to the actual prefix atomically.

**REQ-PHASE-051**: Commit MUST move files from staging to prefix, not copy.

**REQ-PHASE-052**: If commit fails, staging directory MUST be cleaned up.

**REQ-PHASE-053**: Commit failure MUST NOT leave partial files in prefix.

---

## 5. CLI Interface

### 5.1 Command Structure

**REQ-CLI-001**: The CLI MUST follow the pattern:
```
recipe <command> [options] [arguments]
```

**REQ-CLI-002**: Global options MUST be accepted before the command:
```
recipe --prefix /usr/local install package
```

### 5.2 Global Options

**REQ-CLI-010**: The following global options MUST be supported:

| Option | Short | Argument | Description |
|--------|-------|----------|-------------|
| `--recipes-path` | `-r` | PATH | Recipe directory location |
| `--prefix` | `-p` | PATH | Installation prefix |
| `--build-dir` | `-b` | PATH | Build directory location |
| `--help` | `-h` | - | Show help message |
| `--version` | `-V` | - | Show version |

**REQ-CLI-011**: Default values:
- `--recipes-path`: `$RECIPE_PATH` or `$XDG_DATA_HOME/recipe/recipes`
- `--prefix`: `/usr/local`
- `--build-dir`: System temp directory

### 5.3 Install Command

**REQ-CLI-020**: Syntax: `recipe install <package> [options]`

**REQ-CLI-021**: Options:

| Option | Description |
|--------|-------------|
| `--no-deps` | Skip dependency installation |
| `--dry-run` / `-n` | Show what would be installed |
| `--locked` | Require versions match lockfile |

**REQ-CLI-022**: Behavior:
1. Resolve package name to recipe file
2. Parse recipe for dependencies
3. Resolve all transitive dependencies
4. Filter out already-installed packages
5. Execute phases in dependency order
6. Update recipe state files

**REQ-CLI-023**: The install command MUST accept:
- Package name: `recipe install bash`
- Explicit path: `recipe install ./custom/bash.rhai`

**REQ-CLI-024**: Explicit paths MUST be detected by:
- Contains `/` or `\`
- Ends with `.rhai` or implementation-specific extension

### 5.4 Remove Command

**REQ-CLI-030**: Syntax: `recipe remove <package> [options]`

**REQ-CLI-031**: Options:

| Option | Description |
|--------|-------------|
| `--force` / `-f` | Remove even if other packages depend on it |

**REQ-CLI-032**: Behavior:
1. Check for reverse dependencies (fail if found, unless --force)
2. Call `pre_remove()` hook if defined
3. Delete all files in `installed_files`
4. Clean up empty parent directories
5. Call `post_remove()` hook if defined
6. Call custom `remove()` function if defined
7. Update recipe state

**REQ-CLI-033**: File deletion MUST succeed for all files before marking
package as uninstalled.

**REQ-CLI-034**: If any file deletion fails, the `installed` flag MUST
remain true.

### 5.5 Update Command

**REQ-CLI-040**: Syntax: `recipe update [package]`

**REQ-CLI-041**: If package specified:
- Call `check_update()` for that package
- Report available update or "up to date"

**REQ-CLI-042**: If no package specified:
- Iterate all installed packages
- Report all available updates

**REQ-CLI-043**: `check_update()` returning unit/void MUST be interpreted
as "no update available".

### 5.6 Upgrade Command

**REQ-CLI-050**: Syntax: `recipe upgrade [package]`

**REQ-CLI-051**: Behavior:
1. Check if upgrade needed (version mismatch)
2. Remove old installation
3. Install new version

**REQ-CLI-052**: If no upgrade needed, command MUST succeed without action.

### 5.7 List Command

**REQ-CLI-060**: Syntax: `recipe list`

**REQ-CLI-061**: Output MUST include:
- All available recipes
- Installation status for each
- Installed version if applicable

**REQ-CLI-062**: Installed packages MUST be visually distinguished.

### 5.8 Search Command

**REQ-CLI-070**: Syntax: `recipe search <pattern>`

**REQ-CLI-071**: Search MUST be case-insensitive.

**REQ-CLI-072**: Search MUST match against package name.

**REQ-CLI-073**: Search MAY match against description.

### 5.9 Info Command

**REQ-CLI-080**: Syntax: `recipe info <package>`

**REQ-CLI-081**: Output MUST include:
- Package name and version
- Description
- Installation status
- Dependencies
- Installed files (if installed)
- Installation timestamp (if installed)

### 5.10 Dependency Commands

**REQ-CLI-090**: Syntax: `recipe deps <package> [--resolve]`

**REQ-CLI-091**: Without `--resolve`: Show direct dependencies only.

**REQ-CLI-092**: With `--resolve`: Show full installation order.

**REQ-CLI-093**: Syntax: `recipe tree <package>`

**REQ-CLI-094**: Display dependency tree with visual hierarchy.

**REQ-CLI-095**: Syntax: `recipe why <package>`

**REQ-CLI-096**: Show all packages that depend on the specified package.

**REQ-CLI-097**: Syntax: `recipe impact <package>`

**REQ-CLI-098**: Show all installed packages that would break if removed.

### 5.11 Orphan Commands

**REQ-CLI-100**: Syntax: `recipe orphans`

**REQ-CLI-101**: List packages where:
- `installed = true`
- `installed_as_dep = true`
- No installed package depends on them

**REQ-CLI-102**: Syntax: `recipe autoremove [--dry-run]`

**REQ-CLI-103**: Remove all orphaned packages.

### 5.12 Hash Command

**REQ-CLI-110**: Syntax: `recipe hash <file>`

**REQ-CLI-111**: Output MUST include:
- SHA256 hash
- SHA512 hash
- BLAKE3 hash

**REQ-CLI-112**: Output format MUST be copy-pasteable for recipes.

### 5.13 Lock Commands

**REQ-CLI-120**: Syntax: `recipe lock update`

**REQ-CLI-121**: Generate lockfile from current recipe versions.

**REQ-CLI-122**: Syntax: `recipe lock show`

**REQ-CLI-123**: Display lockfile contents.

**REQ-CLI-124**: Syntax: `recipe lock verify`

**REQ-CLI-125**: Verify current recipes match lockfile.

---

## 6. Helper Functions

### 6.1 General Requirements

**REQ-HELPER-001**: All helper functions MUST be available in all lifecycle
phases.

**REQ-HELPER-002**: Helper errors MUST propagate and halt execution.

**REQ-HELPER-003**: Helpers MUST NOT require imports or module loading.

**REQ-HELPER-004**: Helpers SHOULD provide progress feedback for long
operations.

### 6.2 Download Functions

#### download(url)

**REQ-HELPER-010**: Download a file from URL to BUILD_DIR.

**Parameters:**
- `url`: String - HTTP or HTTPS URL

**Behavior:**
- Extract filename from URL path
- Download to `BUILD_DIR/filename`
- Show progress bar with bytes/total/ETA
- Set context for subsequent `verify_*` calls
- Overwrite if file exists

**Errors:**
- Network unreachable
- HTTP error status (4xx, 5xx)
- Timeout exceeded

#### copy(pattern)

**REQ-HELPER-011**: Copy files matching glob pattern to BUILD_DIR.

**Parameters:**
- `pattern`: String - Glob pattern

**Behavior:**
- Resolve pattern relative to current directory
- Copy all matching files to BUILD_DIR
- Set context for subsequent `verify_*` calls

**Errors:**
- No files match pattern
- Permission denied

### 6.3 Verification Functions

#### verify_sha256(expected)

**REQ-HELPER-020**: Verify SHA-256 hash of last downloaded/copied file.

**Parameters:**
- `expected`: String - Expected hash (hex, case-insensitive)

**Behavior:**
- Compute SHA-256 of file set by download/copy
- Compare against expected (case-insensitive)
- Show progress for files >100MB

**Errors:**
- No file to verify (no prior download/copy)
- Hash mismatch (show expected vs actual)

#### verify_sha512(expected)

**REQ-HELPER-021**: Same as verify_sha256 but with SHA-512.

#### verify_blake3(expected)

**REQ-HELPER-022**: Same as verify_sha256 but with BLAKE3.

### 6.4 Archive Functions

#### extract(format)

**REQ-HELPER-030**: Extract archive to BUILD_DIR.

**Parameters:**
- `format`: String - One of: "tar.gz", "tar.xz", "tar.bz2", "zip"

**Behavior:**
- Extract last downloaded/copied file
- Destination is BUILD_DIR (or current_dir if set by cd)
- Show progress indicator

**Errors:**
- No file to extract
- Unsupported format
- Corrupt archive
- Extraction failed

### 6.5 Directory Functions

#### cd(directory)

**REQ-HELPER-040**: Change current working directory.

**Parameters:**
- `directory`: String - Relative or absolute path

**Behavior:**
- If relative, resolve against current_dir
- Update current_dir in context
- Subsequent run() commands use this directory

**Errors:**
- Directory does not exist
- Not a directory

### 6.6 Command Execution Functions

#### run(command)

**REQ-HELPER-050**: Execute shell command.

**Parameters:**
- `command`: String - Shell command

**Behavior:**
- Execute in current_dir
- Inherit environment
- Wait for completion
- Show output in real-time

**Errors:**
- Command not found
- Non-zero exit code
- Timeout (if applicable)

#### shell(command)

**REQ-HELPER-051**: Alias for run(). Use when recipe defines own run().

#### run_output(command)

**REQ-HELPER-052**: Execute and return stdout.

**Parameters:**
- `command`: String - Shell command

**Returns:** String - stdout content

**Behavior:**
- Same as run() but capture stdout
- Trim trailing whitespace

#### run_status(command)

**REQ-HELPER-053**: Execute and return exit code.

**Parameters:**
- `command`: String - Shell command

**Returns:** Integer - Exit code

**Behavior:**
- Same as run() but don't fail on non-zero
- Return exit code

#### exec(command)

**REQ-HELPER-054**: Execute without shell.

**Parameters:**
- `command`: String - Command with arguments

**Behavior:**
- Split on whitespace
- Execute directly without shell
- Useful for avoiding shell escaping issues

#### exec_output(command)

**REQ-HELPER-055**: Execute without shell, return stdout.

### 6.7 Installation Functions

#### install_bin(pattern)

**REQ-HELPER-060**: Install executables to PREFIX/bin.

**Parameters:**
- `pattern`: String - Glob pattern

**Behavior:**
- Match files in current_dir
- Copy to PREFIX/bin/
- Set permissions to 0755
- Track in installed_files

**Errors:**
- No files match
- Permission denied

#### install_lib(pattern)

**REQ-HELPER-061**: Install libraries to PREFIX/lib.

**Parameters:**
- `pattern`: String - Glob pattern

**Behavior:**
- Match files in current_dir
- Copy to PREFIX/lib/
- Set permissions to 0644
- Track in installed_files

#### install_man(pattern)

**REQ-HELPER-062**: Install man pages to appropriate section.

**Parameters:**
- `pattern`: String - Glob pattern (e.g., "*.1")

**Behavior:**
- Detect man section from filename extension
- Copy to PREFIX/share/man/man{N}/
- Set permissions to 0644
- Track in installed_files

#### install_to_dir(pattern, subdir)

**REQ-HELPER-063**: Install to arbitrary PREFIX subdirectory.

**Parameters:**
- `pattern`: String - Glob pattern
- `subdir`: String - Subdirectory under PREFIX

**Behavior:**
- Match files in current_dir
- Copy to PREFIX/{subdir}/
- Preserve original permissions
- Track in installed_files

#### install_to_dir(pattern, subdir, mode)

**REQ-HELPER-064**: Install with explicit permissions.

**Parameters:**
- `pattern`: String - Glob pattern
- `subdir`: String - Subdirectory under PREFIX
- `mode`: Integer - Unix permissions (e.g., 0o755)

#### rpm_install()

**REQ-HELPER-065**: Extract and install all RPM files in BUILD_DIR.

**Behavior:**
- Find all .rpm files in BUILD_DIR
- Extract using rpm2cpio + cpio
- Copy contents to PREFIX
- Track all extracted files

### 6.8 Filesystem Functions

#### exists(path)

**REQ-HELPER-070**: Check if path exists.

**Parameters:**
- `path`: String - Path to check

**Returns:** Boolean

#### file_exists(path)

**REQ-HELPER-071**: Check if path exists and is a file.

#### dir_exists(path)

**REQ-HELPER-072**: Check if path exists and is a directory.

#### mkdir(path)

**REQ-HELPER-073**: Create directory and parents.

**Parameters:**
- `path`: String - Directory to create

**Behavior:**
- Create all parent directories as needed
- No error if already exists

#### rm(pattern)

**REQ-HELPER-074**: Remove files matching pattern.

**Parameters:**
- `pattern`: String - Glob pattern

**Behavior:**
- Remove all matching files
- Remove directories if empty

#### mv(source, destination)

**REQ-HELPER-075**: Move/rename file or directory.

**Parameters:**
- `source`: String - Source path
- `destination`: String - Destination path

#### ln(target, link)

**REQ-HELPER-076**: Create symbolic link.

**Parameters:**
- `target`: String - Link target (what link points to)
- `link`: String - Link path (where link is created)

#### chmod(path, mode)

**REQ-HELPER-077**: Set file permissions.

**Parameters:**
- `path`: String - File path
- `mode`: Integer - Unix permissions (e.g., 0o755)

### 6.9 I/O Functions

#### read_file(path)

**REQ-HELPER-080**: Read entire file contents.

**Parameters:**
- `path`: String - File path

**Returns:** String - File contents

**Errors:**
- File not found
- Permission denied
- Not a file

#### glob_list(pattern)

**REQ-HELPER-081**: List files matching pattern.

**Parameters:**
- `pattern`: String - Glob pattern

**Returns:** Array of strings - Matching paths

### 6.10 Environment Functions

#### env(name)

**REQ-HELPER-090**: Get environment variable.

**Parameters:**
- `name`: String - Variable name

**Returns:** String - Variable value (empty if not set)

#### set_env(name, value)

**REQ-HELPER-091**: Set environment variable.

**Parameters:**
- `name`: String - Variable name
- `value`: String - Variable value

**Behavior:**
- Set for current process and child processes
- Persists for duration of recipe execution

### 6.11 HTTP Functions

#### http_get(url)

**REQ-HELPER-100**: HTTP GET request.

**Parameters:**
- `url`: String - URL to fetch

**Returns:** String - Response body

**Behavior:**
- Follow redirects
- Respect timeout settings

#### github_latest_release(repo)

**REQ-HELPER-101**: Get latest GitHub release version.

**Parameters:**
- `repo`: String - Repository (format: "owner/repo")

**Returns:** String - Version (v prefix stripped)

**Behavior:**
- Use GitHub API
- Respect rate limits
- Use GITHUB_TOKEN if available

#### github_latest_tag(repo)

**REQ-HELPER-102**: Get latest GitHub tag.

**Parameters:**
- `repo`: String - Repository (format: "owner/repo")

**Returns:** String - Tag name

#### parse_version(string)

**REQ-HELPER-103**: Strip version prefix.

**Parameters:**
- `string`: String - Version string

**Returns:** String - Version without v/release-/version- prefix

#### github_download_release(repo, pattern, dest)

**REQ-HELPER-104**: Download release asset matching pattern.

**Parameters:**
- `repo`: String - Repository (format: "owner/repo")
- `pattern`: String - Asset name pattern (glob)
- `dest`: String - Destination path

**Returns:** String - Path to downloaded file

#### extract_from_tarball(url, pattern, dest)

**REQ-HELPER-105**: Extract specific file from remote tarball.

**Parameters:**
- `url`: String - Tarball URL
- `pattern`: String - File pattern to extract
- `dest`: String - Destination path

### 6.12 Git Functions

#### git_clone(url)

**REQ-HELPER-110**: Clone git repository.

**Parameters:**
- `url`: String - Repository URL

**Returns:** String - Path to cloned repository

**Behavior:**
- Clone to BUILD_DIR/{repo-name}
- Extract repo name from URL
- Skip if already cloned (verify with git rev-parse)
- Re-clone if existing repo is corrupted

**Errors:**
- Invalid URL
- Clone failed
- Network unreachable

#### git_clone_depth(url, depth)

**REQ-HELPER-111**: Shallow clone git repository.

**Parameters:**
- `url`: String - Repository URL
- `depth`: Integer - Clone depth (1-1000000)

**Returns:** String - Path to cloned repository

### 6.13 Disk Functions

#### check_disk_space(path, required)

**REQ-HELPER-120**: Verify sufficient disk space.

**Parameters:**
- `path`: String - Path on target filesystem
- `required`: Integer - Required bytes

**Errors:**
- Insufficient space (with available/required comparison)

---

## 7. State Management

### 7.1 State Location

**REQ-STATE-001**: Package state MUST be stored in the recipe file itself.

**REQ-STATE-002**: The recipe file MUST be updated after successful
installation or removal.

**REQ-STATE-003**: State changes MUST be atomic (temp file + rename).

### 7.2 State Variables

**REQ-STATE-010**: On successful install, update the following:

| Variable | Value |
|----------|-------|
| `installed` | `true` |
| `installed_version` | Current `version` value |
| `installed_at` | Current Unix timestamp |
| `installed_files` | List of all installed file paths |
| `installed_as_dep` | `true` if installed as dependency |

**REQ-STATE-011**: On successful remove, update the following:

| Variable | Value |
|----------|-------|
| `installed` | `false` |
| `installed_version` | None/unit value |
| `installed_at` | `0` |
| `installed_files` | `[]` |

### 7.3 State Parsing

**REQ-STATE-020**: Implementation MUST parse variables WITHOUT executing
the recipe.

**REQ-STATE-021**: Variable parsing MUST handle:
- Inline comments (`// comment`)
- Block comments (`/* comment */`)
- String escaping
- Array syntax

**REQ-STATE-022**: Variable updating MUST preserve:
- Indentation style
- Comment placement
- Unrelated code

### 7.4 Installed Files Tracking

**REQ-STATE-030**: Every file installed via helper functions MUST be
added to `installed_files`.

**REQ-STATE-031**: Paths in `installed_files` MUST be absolute.

**REQ-STATE-032**: Symlinks MUST be tracked separately from their targets.

**REQ-STATE-033**: Directories MUST NOT be tracked (only files).

---

## 8. Dependency Resolution

### 8.1 Dependency Specification

**REQ-DEP-001**: Dependencies MUST be declared in the `deps` array:

```
let deps = ["pkg-a", "pkg-b >= 1.0"];
```

**REQ-DEP-002**: Each dependency entry MUST be a string.

**REQ-DEP-003**: Dependency format:
```
<package-name> [<operator> <version>][, <operator> <version>]...
```

### 8.2 Version Operators

**REQ-DEP-010**: Supported operators:

| Operator | Meaning |
|----------|---------|
| (none) | Any version |
| `=` | Exact version |
| `>` | Greater than |
| `>=` | Greater than or equal |
| `<` | Less than |
| `<=` | Less than or equal |
| `^` | Compatible (same major, if major > 0) |
| `~` | Approximately (same major.minor) |

**REQ-DEP-011**: Multiple constraints MUST be comma-separated:
```
"openssl >= 3.0, < 4.0"
```

### 8.3 Resolution Algorithm

**REQ-DEP-020**: Implementation MUST use topological sort.

**REQ-DEP-021**: Algorithm MUST detect cycles.

**REQ-DEP-022**: Algorithm MUST handle diamond dependencies:
```
A depends on B, C
B depends on D
C depends on D
D installed only once
```

**REQ-DEP-023**: Resolution order MUST install dependencies before
dependents.

### 8.4 Constraint Validation

**REQ-DEP-030**: Version constraints MUST be validated AFTER resolution.

**REQ-DEP-031**: If constraint violated, installation MUST fail with
clear error message showing:
- Package with constraint
- Constraint specification
- Available version
- Why constraint not satisfied

### 8.5 Dependency Installation

**REQ-DEP-040**: Packages installed as dependencies MUST be marked:
```
installed_as_dep = true
```

**REQ-DEP-041**: Packages installed directly MUST have:
```
installed_as_dep = false
```

**REQ-DEP-042**: With `--no-deps`, only the requested package is installed.

### 8.6 Orphan Detection

**REQ-DEP-050**: An orphan is a package where:
- `installed = true`
- `installed_as_dep = true`
- No installed package lists it as a dependency

**REQ-DEP-051**: `recipe orphans` MUST list all orphans.

**REQ-DEP-052**: `recipe autoremove` MUST remove all orphans.

---

## 9. Lockfile System

### 9.1 Lockfile Purpose

The lockfile provides reproducible builds by pinning exact versions.

### 9.2 Lockfile Format

**REQ-LOCK-001**: Lockfile MUST be named `recipe.lock`.

**REQ-LOCK-002**: Lockfile MUST be located in recipes directory root.

**REQ-LOCK-003**: Lockfile format (TOML recommended):

```toml
# recipe.lock - Auto-generated, do not edit manually

[packages]
bash = "5.2.26"
readline = "8.2"
ncurses = "6.4"

[metadata]
generated = "2026-01-25T10:30:00Z"
generator = "recipe 1.0.0"
```

### 9.3 Lockfile Operations

**REQ-LOCK-010**: `lock update` MUST:
- Scan all recipe files
- Extract current versions
- Write to lockfile

**REQ-LOCK-011**: `lock show` MUST:
- Display all locked packages and versions
- Show generation timestamp

**REQ-LOCK-012**: `lock verify` MUST:
- Compare recipe versions to locked versions
- Report all mismatches
- Exit non-zero if any mismatch

### 9.4 Locked Installation

**REQ-LOCK-020**: With `--locked` flag:
- Resolve dependencies normally
- Compare each version to lockfile
- Fail if any mismatch

**REQ-LOCK-021**: Error message MUST show:
- Package name
- Locked version
- Resolved version

---

## 10. Output and Progress

### 10.1 Output Levels

**REQ-OUTPUT-001**: Standard output for normal messages.

**REQ-OUTPUT-002**: Standard error for warnings and errors.

### 10.2 Message Types

**REQ-OUTPUT-010**: Action message:
```
==> Installing bash 5.2.26
```

**REQ-OUTPUT-011**: Numbered action (for batch operations):
```
(1/5) Installing readline
```

**REQ-OUTPUT-012**: Sub-action:
```
  -> Downloading source
```

**REQ-OUTPUT-013**: Detail (supplementary info):
```
     https://example.com/file.tar.gz
```

**REQ-OUTPUT-014**: Success:
```
==> Successfully installed bash
```

**REQ-OUTPUT-015**: Warning (to stderr):
```
warning: Package xyz is deprecated
```

**REQ-OUTPUT-016**: Error (to stderr):
```
error: Package not found: xyz
```

**REQ-OUTPUT-017**: Skip (dimmed):
```
==> bash already installed
```

### 10.3 Progress Indicators

**REQ-OUTPUT-020**: Downloads MUST show progress bar:
```
Downloading [=========>        ] 45% 12.5 MB/s ETA 00:32
```

**REQ-OUTPUT-021**: Progress bar MUST include:
- Visual bar
- Percentage
- Transfer speed
- Estimated time remaining

**REQ-OUTPUT-022**: Long operations MAY show spinner:
```
⠋ Extracting archive...
```

### 10.4 Terminal Detection

**REQ-OUTPUT-030**: Progress indicators MUST be disabled when output
is not a TTY.

**REQ-OUTPUT-031**: Colors MUST be disabled when output is not a TTY.

**REQ-OUTPUT-032**: Colors MAY be forced via environment variable.

---

## 11. Error Handling

### 11.1 Error Categories

**REQ-ERROR-001**: Errors MUST be categorized:

| Category | Examples |
|----------|----------|
| User Error | Package not found, invalid argument |
| Recipe Error | Syntax error, missing function |
| Network Error | Download failed, timeout |
| Filesystem Error | Permission denied, disk full |
| Dependency Error | Cycle detected, constraint violated |
| Internal Error | Implementation bug |

### 11.2 Error Messages

**REQ-ERROR-010**: Error messages MUST include:
- Error category
- What failed
- Why it failed (if known)

**REQ-ERROR-011**: Error messages SHOULD include:
- Suggested resolution
- Relevant file/line if applicable

**REQ-ERROR-012**: Example format:
```
error: Package 'foobar' not found

Searched in: /usr/local/share/recipe/recipes
Did you mean: foobaz, foobarr

hint: Run 'recipe search foo' to find packages
```

### 11.3 Exit Codes

**REQ-ERROR-020**: Exit codes:

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error |
| 2 | Usage error (bad arguments) |
| 3 | Package not found |
| 4 | Dependency error |
| 5 | Network error |
| 6 | Permission error |

### 11.4 State Preservation

**REQ-ERROR-030**: Failed operations MUST NOT corrupt state.

**REQ-ERROR-031**: If installation fails:
- Staging directory MUST be cleaned up
- Recipe file MUST NOT be modified
- PREFIX MUST NOT be modified

**REQ-ERROR-032**: If removal fails partially:
- `installed` flag MUST remain true
- `installed_files` SHOULD be updated to reflect remaining files

---

## 12. Security Considerations

### 12.1 Path Traversal

**REQ-SEC-001**: Package names MUST be validated against pattern:
```
^[a-z][a-z0-9]*(-[a-z0-9]+)*$
```

**REQ-SEC-002**: Installation helpers MUST validate destination is
within PREFIX.

**REQ-SEC-003**: No path component may be `.` or `..`.

### 12.2 Command Injection

**REQ-SEC-010**: URLs MUST be validated before use.

**REQ-SEC-011**: Git URLs MUST only allow:
- `https://`
- `http://`
- `git@`
- `ssh://`

**REQ-SEC-012**: `file://` URLs MUST be rejected.

### 12.3 Hash Verification

**REQ-SEC-020**: Downloaded files SHOULD be verified before extraction.

**REQ-SEC-021**: Hash algorithms MUST be cryptographically secure:
- SHA-256 (minimum)
- SHA-512
- BLAKE3

**REQ-SEC-022**: MD5 and SHA-1 MUST NOT be supported.

### 12.4 Permissions

**REQ-SEC-030**: Files MUST NOT be installed with SUID/SGID bits.

**REQ-SEC-031**: Directories MUST be created with 0755 or stricter.

**REQ-SEC-032**: Configuration files SHOULD be created with 0644.

### 12.5 Network Security

**REQ-SEC-040**: HTTPS SHOULD be preferred over HTTP.

**REQ-SEC-041**: HTTP downloads SHOULD trigger a warning.

**REQ-SEC-042**: Certificate validation MUST NOT be disabled by default.

---

## 13. Environment Variables

### 13.1 Input Variables

**REQ-ENV-001**: The following environment variables affect behavior:

| Variable | Purpose | Default |
|----------|---------|---------|
| `RECIPE_PATH` | Recipes directory | `$XDG_DATA_HOME/recipe/recipes` |
| `RECIPE_HTTP_TIMEOUT` | HTTP timeout seconds | 30 |
| `GITHUB_TOKEN` | GitHub API authentication | (none) |
| `RPM_PATH` | RPM repository path | (none) |
| `XDG_DATA_HOME` | XDG base directory | `$HOME/.local/share` |

### 13.2 Timeout Configuration

**REQ-ENV-010**: `RECIPE_HTTP_TIMEOUT` MUST be parsed as integer seconds.

**REQ-ENV-011**: Valid range: 5-300 seconds.

**REQ-ENV-012**: Out-of-range values MUST be clamped.

### 13.3 GitHub Token

**REQ-ENV-020**: Without token: 60 requests/hour rate limit.

**REQ-ENV-021**: With token: 5000 requests/hour rate limit.

**REQ-ENV-022**: Token format: `ghp_*` (classic) or `github_pat_*` (fine-grained).

---

## 14. File Permissions

### 14.1 Default Modes

**REQ-PERM-001**: Installation helpers MUST set appropriate modes:

| Helper | Default Mode |
|--------|--------------|
| `install_bin()` | 0755 |
| `install_lib()` | 0644 |
| `install_man()` | 0644 |
| `install_to_dir()` | preserve original |

### 14.2 Directory Creation

**REQ-PERM-010**: `mkdir()` MUST create directories with 0755.

**REQ-PERM-011**: Parent directories MUST be created as needed.

### 14.3 Explicit Permissions

**REQ-PERM-020**: `chmod()` MUST accept Unix octal mode.

**REQ-PERM-021**: Mode MUST be interpreted as integer (e.g., 0o755 = 493).

---

## 15. Network Operations

### 15.1 HTTP Client Requirements

**REQ-NET-001**: HTTP client MUST support:
- HTTP/1.1 and HTTP/2
- HTTPS with TLS 1.2+
- Redirect following (max 10 redirects)
- Chunked transfer encoding
- Content-Length detection

### 15.2 Timeout Behavior

**REQ-NET-010**: Connection timeout: configurable (default 30s).

**REQ-NET-011**: Read timeout: configurable (default 30s).

**REQ-NET-012**: Total timeout: connection + transfer time.

### 15.3 Retry Behavior

**REQ-NET-020**: Retries MAY be implemented for transient failures.

**REQ-NET-021**: If retrying, exponential backoff SHOULD be used.

**REQ-NET-022**: Maximum retries: 3.

### 15.4 Proxy Support

**REQ-NET-030**: HTTP_PROXY environment variable SHOULD be respected.

**REQ-NET-031**: HTTPS_PROXY environment variable SHOULD be respected.

**REQ-NET-032**: NO_PROXY environment variable SHOULD be respected.

---

## 16. Archive Handling

### 16.1 Supported Formats

**REQ-ARCH-001**: Required archive formats:

| Format | Extension | Required Tool |
|--------|-----------|---------------|
| gzip-compressed tar | .tar.gz, .tgz | tar |
| xz-compressed tar | .tar.xz | tar |
| bzip2-compressed tar | .tar.bz2 | tar |
| ZIP | .zip | unzip |

### 16.2 Extraction Behavior

**REQ-ARCH-010**: Archives MUST be extracted to current_dir.

**REQ-ARCH-011**: Existing files MUST be overwritten.

**REQ-ARCH-012**: Directory structure MUST be preserved.

### 16.3 Error Handling

**REQ-ARCH-020**: Corrupt archives MUST fail with clear error.

**REQ-ARCH-021**: Missing extraction tool MUST fail with helpful message:
```
error: Cannot extract .tar.xz: 'xz' not found
hint: Install xz-utils package
```

---

## 17. Hash Verification

### 17.1 Supported Algorithms

**REQ-HASH-001**: Required hash algorithms:

| Algorithm | Function | Output Size |
|-----------|----------|-------------|
| SHA-256 | `verify_sha256()` | 64 hex chars |
| SHA-512 | `verify_sha512()` | 128 hex chars |
| BLAKE3 | `verify_blake3()` | 64 hex chars |

### 17.2 Hash Comparison

**REQ-HASH-010**: Hash comparison MUST be case-insensitive.

**REQ-HASH-011**: Both uppercase and lowercase hex MUST be accepted.

**REQ-HASH-012**: Whitespace in expected hash SHOULD be stripped.

### 17.3 Progress Feedback

**REQ-HASH-020**: For files >100MB, progress SHOULD be shown.

**REQ-HASH-021**: Progress update interval: every 1MB or less.

### 17.4 Error Reporting

**REQ-HASH-030**: Hash mismatch MUST show:
```
error: SHA256 integrity check failed
  file:     /path/to/file
  expected: abc123...
  actual:   def456...
```

---

## 18. Git Operations

### 18.1 Clone Behavior

**REQ-GIT-001**: Clones MUST go to BUILD_DIR/{repo-name}.

**REQ-GIT-002**: Repository name MUST be extracted from URL:
- `https://github.com/owner/repo.git` → `repo`
- `https://github.com/owner/repo` → `repo`
- `git@github.com:owner/repo.git` → `repo`

### 18.2 Caching

**REQ-GIT-010**: If directory exists and is valid git repo, skip clone.

**REQ-GIT-011**: Validity check: `git rev-parse HEAD` succeeds.

**REQ-GIT-012**: If corrupted, delete and re-clone.

### 18.3 Shallow Clones

**REQ-GIT-020**: `git_clone_depth()` MUST use `--depth` flag.

**REQ-GIT-021**: Depth range: 1 to 1,000,000.

**REQ-GIT-022**: Depth=1 means only latest commit.

### 18.4 URL Validation

**REQ-GIT-030**: Allowed URL schemes:
- `https://`
- `http://`
- `git@`
- `ssh://`

**REQ-GIT-031**: Rejected URL schemes:
- `file://`
- Any other scheme

---

## 19. Atomic Installation

### 19.1 Staging Directory

**REQ-ATOMIC-001**: A staging directory MUST be used during install.

**REQ-ATOMIC-002**: Staging directory MUST be on same filesystem as PREFIX.

**REQ-ATOMIC-003**: During install phase, `PREFIX` points to staging.

### 19.2 Commit Process

**REQ-ATOMIC-010**: Commit MUST move files atomically where possible.

**REQ-ATOMIC-011**: On same filesystem, use rename() for atomicity.

**REQ-ATOMIC-012**: Cross-filesystem moves MUST copy-then-delete.

### 19.3 Failure Recovery

**REQ-ATOMIC-020**: If any phase fails before commit:
- Staging directory MUST be cleaned up
- PREFIX MUST be unchanged
- Recipe state MUST be unchanged

**REQ-ATOMIC-021**: If commit fails partway:
- Already-committed files remain
- Recipe state shows partial install
- User must manually recover

### 19.4 Cleanup

**REQ-ATOMIC-030**: Staging directory MUST be cleaned up on success.

**REQ-ATOMIC-031**: BUILD_DIR MAY be preserved for caching.

**REQ-ATOMIC-032**: BUILD_DIR cleanup is user's responsibility.

---

## 20. Conformance Testing

### 20.1 Test Categories

Implementations MUST pass the following test categories:

#### 20.1.1 CLI Tests

- [ ] All commands accept --help
- [ ] Global options work before command
- [ ] Unknown commands produce error
- [ ] Package not found produces helpful error

#### 20.1.2 Lifecycle Tests

- [ ] acquire() called before build()
- [ ] build() called before install()
- [ ] Skipped if is_installed() returns true
- [ ] State updated after successful install
- [ ] State unchanged after failed install

#### 20.1.3 Helper Tests

- [ ] download() creates file in BUILD_DIR
- [ ] verify_sha256() fails on mismatch
- [ ] verify_sha256() succeeds on match
- [ ] extract() handles all formats
- [ ] install_bin() sets 0755 permissions
- [ ] install_lib() sets 0644 permissions

#### 20.1.4 Dependency Tests

- [ ] Simple dependency chain resolves
- [ ] Diamond dependency resolved correctly
- [ ] Cycle detected with error
- [ ] Version constraint validated
- [ ] --no-deps skips dependencies

#### 20.1.5 State Tests

- [ ] installed_files tracks all files
- [ ] installed_version set on install
- [ ] installed_at contains valid timestamp
- [ ] installed_as_dep set for dependencies

#### 20.1.6 Removal Tests

- [ ] All installed_files deleted
- [ ] Empty directories cleaned up
- [ ] Reverse dependency check works
- [ ] --force bypasses check

#### 20.1.7 Atomicity Tests

- [ ] Failed acquire leaves no state change
- [ ] Failed build leaves no state change
- [ ] Failed install leaves no state change
- [ ] Successful install updates all state

### 20.2 Test Recipe

The following minimal recipe MUST work on all conforming implementations:

```
let name = "conformance-test";
let version = "1.0.0";
let installed = false;

fn acquire() {
    // Create a test file
    run("echo 'test' > " + BUILD_DIR + "/test.txt");
}

fn build() {
    // Optional build step
}

fn install() {
    install_to_dir(BUILD_DIR + "/test.txt", "share/conformance");
}

fn is_installed() {
    file_exists(PREFIX + "/share/conformance/test.txt")
}
```

### 20.3 Version Claim

**REQ-CONFORM-001**: Implementations claiming conformance MUST state
which version of this specification they implement.

**REQ-CONFORM-002**: Implementations MUST document any deviations.

---

## Appendix A: Variable Type Reference

| Type | Description | Example |
|------|-------------|---------|
| String | UTF-8 text | `"hello"` |
| Boolean | True or false | `true`, `false` |
| Integer | 64-bit signed | `42`, `-1` |
| Array | Ordered list | `["a", "b"]` |
| None/Unit | Absence of value | `()` |

---

## Appendix B: Glob Pattern Syntax

| Pattern | Meaning |
|---------|---------|
| `*` | Match any characters except `/` |
| `**` | Match any characters including `/` |
| `?` | Match single character |
| `[abc]` | Match one of a, b, or c |
| `[!abc]` | Match anything except a, b, or c |
| `{a,b}` | Match a or b |

---

## Appendix C: Semantic Versioning Reference

Format: `MAJOR.MINOR.PATCH`

- MAJOR: Incompatible API changes
- MINOR: Backwards-compatible functionality
- PATCH: Backwards-compatible bug fixes

Examples:
- `1.0.0` - Initial release
- `1.1.0` - New feature, compatible with 1.0.0
- `1.1.1` - Bug fix to 1.1.0
- `2.0.0` - Breaking changes from 1.x

---

## Appendix D: Exit Code Reference

| Code | Name | Description |
|------|------|-------------|
| 0 | SUCCESS | Operation completed successfully |
| 1 | ERROR | General error |
| 2 | USAGE | Invalid command-line usage |
| 3 | NOT_FOUND | Package or recipe not found |
| 4 | DEPENDENCY | Dependency resolution failed |
| 5 | NETWORK | Network operation failed |
| 6 | PERMISSION | Permission denied |
| 7 | CONFLICT | File conflict or version conflict |
| 8 | INTEGRITY | Hash verification failed |

---

## Appendix E: Recipe File Grammar

```ebnf
recipe      = { statement } ;
statement   = variable | function ;
variable    = "let" identifier "=" value ";" ;
function    = "fn" identifier "()" block ;
block       = "{" { statement | expression } "}" ;
value       = string | number | boolean | array | unit ;
string      = '"' { character } '"' ;
number      = digit { digit } ;
boolean     = "true" | "false" ;
array       = "[" [ value { "," value } ] "]" ;
unit        = "()" ;
identifier  = letter { letter | digit | "_" } ;
```

---

## Appendix F: Helper Function Quick Reference

### Acquire Phase
```
download(url: String)
copy(pattern: String)
verify_sha256(hash: String)
verify_sha512(hash: String)
verify_blake3(hash: String)
```

### Build Phase
```
extract(format: String)
cd(directory: String)
run(command: String)
shell(command: String)
```

### Install Phase
```
install_bin(pattern: String)
install_lib(pattern: String)
install_man(pattern: String)
install_to_dir(pattern: String, subdir: String)
install_to_dir(pattern: String, subdir: String, mode: Integer)
rpm_install()
```

### Filesystem
```
exists(path: String) -> Boolean
file_exists(path: String) -> Boolean
dir_exists(path: String) -> Boolean
mkdir(path: String)
rm(pattern: String)
mv(source: String, dest: String)
ln(target: String, link: String)
chmod(path: String, mode: Integer)
```

### I/O
```
read_file(path: String) -> String
glob_list(pattern: String) -> Array<String>
```

### Environment
```
env(name: String) -> String
set_env(name: String, value: String)
```

### Commands
```
run_output(command: String) -> String
run_status(command: String) -> Integer
exec(command: String)
exec_output(command: String) -> String
```

### HTTP
```
http_get(url: String) -> String
github_latest_release(repo: String) -> String
github_latest_tag(repo: String) -> String
parse_version(string: String) -> String
github_download_release(repo: String, pattern: String, dest: String) -> String
extract_from_tarball(url: String, pattern: String, dest: String)
```

### Git
```
git_clone(url: String) -> String
git_clone_depth(url: String, depth: Integer) -> String
```

### Disk
```
check_disk_space(path: String, bytes: Integer)
```

---

## Appendix G: Error Message Templates

### Package Not Found
```
error: Package 'NAME' not found

Searched in: PATH
Did you mean: SUGGESTION1, SUGGESTION2?

hint: Run 'recipe search QUERY' to find packages
```

### Dependency Cycle
```
error: Circular dependency detected

  PKG-A depends on PKG-B
  PKG-B depends on PKG-C
  PKG-C depends on PKG-A

hint: Review dependency declarations
```

### Hash Mismatch
```
error: ALGORITHM integrity check failed

  file:     FILE_PATH
  expected: EXPECTED_HASH
  actual:   ACTUAL_HASH

hint: The file may be corrupted or tampered with
```

### Version Constraint
```
error: Version constraint not satisfied

  PKG-A requires: PKG-B >= 2.0
  available:      PKG-B 1.5.0

hint: Upgrade PKG-B or relax the constraint
```

### Reverse Dependency
```
error: Cannot remove 'PKG-A': other packages depend on it

  PKG-B requires PKG-A
  PKG-C requires PKG-A

hint: Use --force to remove anyway
```

---

## Appendix H: Changelog

### Version 1.0.0 (2026-01-25)
- Initial specification release
- Based on Rust/Rhai implementation analysis
- Implementation-agnostic requirements

---

## Document Information

**Specification Version:** 1.0.0
**Document Status:** Draft
**Maintainer:** LevitateOS Project
**License:** MIT
