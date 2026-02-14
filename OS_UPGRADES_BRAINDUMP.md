# OS Upgrades Braindump (Recipe + A/B Images)

This is a braindump of how "OS upgrades" should work in this repo when the system is built from recipes and shipped as disk images (with A/B slots by default).

The key idea: an "OS upgrade" is mostly "refresh and rebuild the set of preinstalled packages that end up on the installed disk", not a traditional in-place package-manager mutation of the running system.

## Motivation

- Our distros are assembled via the `recipe` package manager (Rhai recipes).
- Going forward, A/B immutable should be the default update model:
  - Build the next system into the inactive slot (`B`)
  - Test/trial-boot `B`
  - Commit or rollback
- Historically, recipes focused on `acquire/build/install` but we've underinvested in the *update/upgrade lifecycle* hooks.

## Terms (Proposed)

- **Refresh** (recipe maintenance): Update the *recipe* itself so it continues to build/install correctly.
  - Example: upstream moved URLs, changed checksums, renamed build deps, requires new flags, etc.
  - This is where an LLM helper may consult upstream docs/release notes to propose recipe edits.
- **Update** (discover): Determine whether a newer version exists for a recipe (`check_update()`).
- **Upgrade** (apply): Deterministically build/install the newer version into a target root/prefix.
  - For immutable A/B OS updates: install into the inactive slot root filesystem (not into the running `/`).

## What An "OS Upgrade" Actually Is

An OS upgrade is "upgrade the package set baked into the disk image / slot".

- Many "packages" are just files we preinstall into the disk image.
- Some updates are not even "system files" in the classic sense (they may be tooling shipped on the appliance).
- Therefore, upgrades should be modeled as **reinstalling** new versions into a target filesystem tree, not mutating the live host root.

## Lifecycle Hooks We Need To Treat As First-Class

We already have (or intend) `check_update()` and `upgrade` commands (see `REQUIREMENTS.md`), but the missing piece is the practical "recipe refresh" workflow.

Suggested hooks/helpers to formalize (names TBD):

- `check_update()`:
  - Returns "new version available" (and optionally metadata).
- `refresh_recipe()` (or similar):
  - Optional. Produces a proposed patch to the recipe when upstream changes break the current recipe.
  - May invoke an LLM to look online for:
    - new download URLs / tags
    - new checksums
    - dependency changes
    - build instructions
  - Must be auditable and gated (see security notes below).
- `upgrade()`:
  - Deterministic application: runs `acquire/build/install` for the new version.
  - Should be implemented as a "reinstall into a clean target root" whenever possible.

## Deterministic Upgrade Model (Preferred)

Upgrade should look like a clean install into a target prefix/root, not a set of imperative edits on the running system.

Conceptually:

1. Resolve the new version (from `check_update()` or a user override).
2. Build into an isolated build directory (as usual).
3. Install into a target rootfs/prefix directory:
   - Examples:
     - `DESTDIR=/var/lib/recipe/stage/<pkg>/<version>/rootfs`
     - or "inactive slot root mount" (for A/B updates)
4. Produce a manifest (files, hashes) and update recipe state.
5. Only after validation, make it the active system (slot switch / boot entry change).

This aligns with A/B: "upgrade" is "compose the next system image/slot".

## A/B Flow (OS-Level)

When A/B is enabled (default in this direction):

1. Compose slot `B` from current policy (recipe lock + package set).
2. Run validations against slot `B` filesystem tree.
3. Boot slot `B` once (trial boot) and run minimal health checks.
4. Commit slot `B` or rollback to `A`.

This is why we added an explicit checkpoint for "successful `B` boot".

## Security / Product Constraints

- Mutable (in-place) system mutation is inherently dangerous when recipes can be authored/modified by an LLM.
- Therefore:
  - Immutable A/B is the supported path.
  - Mutable mode (if it exists at all) is opt-in and explicitly unsafe.

For the recipe refresh/update hook that consults online sources:

- Treat fetched web content as untrusted input.
- Record provenance:
  - what URLs/docs were consulted
  - what changes were proposed
  - what checksums changed and why
- Require explicit approval to apply recipe source edits (especially for system recipes).
- Prefer lock files and content hashes to keep upgrades reproducible.

## Concrete Next Steps (Documentation-Level)

- Decide and document the exact meaning of:
  - "update" vs "refresh" vs "upgrade"
- Add a standard pattern for recipes to implement `check_update()` in a consistent way.
- Add a standard "refresh recipe" workflow:
  - where the LLM helper runs
  - what artifacts it can change (version fields, URLs, checksums, deps)
  - what guardrails exist (review required, signatures, lock constraints)

