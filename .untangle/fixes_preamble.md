Generated from `.untangle/fixes.py` by `.untangle/apply_fixes.py`. Each fix appears in execution order, with every problem it removes listed under it by ID. `python3 -I .untangle/check_fixes.py UNTANGLE.md` confirms that every ID is in exactly one place.

**Review status:** the plan was shown to the user on 2026-11-06. The decisions they still need to make are listed in "Open questions" below. Until they answer, the plan proceeds with the defaults stated there.

### Requirements behind the special cases

The 168 special cases reduce to these requirements. The design never stated them, so each fix below is built to state them once:

1. **Every teardown step reports its own failure without masking the primary one.** This rules out swallowed reap, sync, drain and diagnostic-write errors, and also secondary failures that replace the real outcome. (A50, A51, A76, D62, H23, H24, I13, J29, J32, J41, J42 → X6, X7, X12)
2. **Cancellation is a typed outcome that every blocking wait observes, and library callers can trigger it.** This rules out reading a global at labelling time, sleep-polling and uncancellable copies. (A53, A55, A89, E69, B39, J35, J38, J61 → X6)
3. **Each failure has a machine-testable category and reason**, so classification never uses substrings and autofix eligibility never uses string matching. (A20, A21, A35, C48, C51, H28, H29 → X3)
4. **Operations differ in what they need and what they write.** Checks don't fabricate or commit state or resolve DNS; force flags belong only to install; cleanup carries its reason. (A56, H7, B28, C49, J39 → X3, X6)
5. **A recipe's role (main or dependency) is explicit and has its own policy:** force, outputs, work dirs, env and failure eligibility. (C24, C43, C51, I7, I8, I11 → X10)
6. **Phases follow one table of rules** for optionality, missing hooks, forced runs and post-verification. (C44–C47, I25, I26 → X10)
7. **Inputs have declared kinds** (file, tree, rootfs, restore target). Special behavior never keys off the names `rootfs` and `target`. (B45, B46, J34 → X9, X5)
8. **Where outputs and state go comes from the invocation**, not from the cwd, a shared default destination or a leaked tempdir. (A66, H20, B73, F36 → X6, X7)
9. **Every deadline and bound has one definition** that both sides of the boundary read. (A10, A46, A54, B17, B48, A73 → X3, X6, X7)
10. **Producer and consumer agree on artifact formats by sharing code:** the image, the archive and the export tar. (B84–B88, A67, J58 → X8, X5)
11. **Helpers behave the same in the same situation:** missing parents, zero matches, bad patterns, nonzero exits, reuse of existing results and staging before replacing. (E15, E16, E28, E36, J3, J4, J6, J9–J11, J15 → X8, X11, X12)
12. **A knob must reach the process that reads it.** Otherwise it doesn't exist. (D14, D32, E43, J21, J22 → X11)
13. **Crash leftovers are recognized by one rule per store**, and pruning never fails a new invocation. (A65, J43, J44, J46 → X7)

### Fix check (together / apart / direction)

- **Together:** each fix gathers one concept that is spread out today. Examples: the domain layout (X2), the cross-boundary contract (X3), the Ginux interface (X5), resolution (X9), the phase rules (X10) and the helper metadata (X11). The far co-change pairs from the Map (`lib.rs`↔`bin/recipe.rs`/`backend.rs`/`worker/run.rs`) collapse into X4's single `Invocation`.
- **Apart:** X4 splits `ExecutionOptions` by reason to change, and X13 splits `logging.rs` into a sink and a log store. X5 takes Ginux wire formats out of the orchestration in X6, and X9 takes host-side metadata out of the worker executor.
- **Direction after the fixes:** the leaves `domain`, `fsx`/`archive`/`image` and `sink` depend on nothing in the crate. `protocol` depends on `domain`. The coordinator side runs `ginux` and the stores → the run pipeline → `RecipeEngine`. The worker side runs `recipes` → the lifecycle → the helpers, all over the leaves. Today's inversions go away: `logging`/`cmd` → `worker::protocol`, `inputs::discover` → `core::executor`, in-domain code → `inputs`, and the `invocation`↔`runner` cycle. No fix adds an import toward a more volatile module.

### Order

X1 removes about 80 items with little risk and makes rustc report the remaining dead code. X2, X12 and X8 are leaves that the others build on. X3 comes next, then the coordinator side (X7 → X5 → X4 → X6), then the worker side (X9 → X10 → X13 → X11), and then X14. X16 and X15 come last, because they assert and document the final shapes. The Comments addition runs alongside each fix in the files it touches.

### Open questions for the user (defaults in bold)

1. **Library API.** No crate in the superproject links `levitate_recipe` as a library: they all run the `recipe` binary. X1 and X4 shrink the API and change `RecipeEngine` to return JSON instead of `rhai::Map`. **Default: allowed.**
2. **`ginux-domain` dependency.** X5 can validate the domain config with the upstream `tools/ginux` `ginux-domain` crate (a path dependency) instead of copying its limits. That couples the recipe build to the ginux checkout. **Default: keep hand-copied limits, centralized in X5, with a comment naming the upstream source.**
3. **Removing helpers.** X11 removes the `llm_*` helpers from the worker, because they cannot work there. The unexercised public helpers (D17) are left as is. **Default: remove `llm_*` from the worker; keep the rest.**
4. **Deleting docs.** X15 deletes the obsolete design notes and `APOLOGY.md`. **Default: delete; they stay in git history.**
5. **CLI flags.** X4 removes `--target-os`/`--target-arch` (each accepts only one value) and makes `--force-*`/`--llm-profile` subcommand-specific. distro-builder and xtask don't pass these flags. **Default: allowed.**
6. **Branch.** The recipe repo is checked out on `ginux-recipe/r06-launch`. Refactor commits land there, one per fix. **Default: as checked out.**
