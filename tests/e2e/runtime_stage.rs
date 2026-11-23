//! `recipe runtime stage`: the runtime tree a package or installer installs
//! (the Ginux files, Recipe's guard, the image references, the guard's image
//! allowlist with those references, the sources, and with `--sudoers GROUP`
//! the sudoers rule, which names the guard and never the helper).
//! Preconditions: none. It needs no Ginux, no sudo and no privileges: it
//! writes into a staging directory (never `/`, so `visudo` is not run) and
//! checks modes and text. Ownership is the installing package's business and
//! is not checked.
use super::*;
use std::os::unix::fs::PermissionsExt;

const REFERENCE: &str =
    "local/recipe-compiler@sha256:0000000000000000000000000000000000000000000000000000000000000001";
const MINIMAL: &str =
    "local/recipe-minimal@sha256:0000000000000000000000000000000000000000000000000000000000000002";
const ALLOWLIST: &str = "usr/libexec/recipe/runtime-images";

/// The references the staged allowlist lists (comments and blank lines left out).
fn allowlisted(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join(ALLOWLIST))
        .unwrap()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

fn stage_command(recipes: &Path, root: &Path) -> Command {
    stage_command_with_guard(recipes, root, None)
}

/// `guard`: the file `--recipe-guard` names; None: the stand-in this fixture writes.
fn stage_command_with_guard(recipes: &Path, root: &Path, guard: Option<&Path>) -> Command {
    let dir = recipes.parent().unwrap();
    let files = dir.join("ginux-files");
    std::fs::create_dir_all(&files).unwrap();
    for name in ["cli", "helper", "image", "runtime", "allocator", "guard"] {
        std::fs::write(files.join(name), format!("stand-in {name}")).unwrap();
    }
    let catalog = dir.join("catalog");
    std::fs::create_dir_all(&catalog).unwrap();
    std::fs::write(catalog.join("index.json"), "{}").unwrap();
    let mut command = host_command(recipes);
    command
        .args(["runtime", "stage", "--root"])
        .arg(root)
        .arg("--catalog")
        .arg(&catalog)
        .args(["--reference", REFERENCE]);
    for (option, name) in [
        ("--ginux-cli", "cli"),
        ("--ginux-helper", "helper"),
        ("--ginux-image", "image"),
        ("--ginux-runtime", "runtime"),
        ("--ginux-allocator", "allocator"),
        ("--recipe-guard", "guard"),
    ] {
        let file = match (name, guard) {
            ("guard", Some(guard)) => guard.to_path_buf(),
            _ => files.join(name),
        };
        command.arg(option).arg(file);
    }
    command
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[test]
fn runtime_stage_writes_the_ginux_files_the_guard_and_the_sudoers_rule_for_a_group() {
    let (dir, recipes) = create_test_env();
    let root = dir.path().join("staged");
    let output = stage_command(&recipes, &root)
        .args(["--sudoers", "wheel"])
        .output()
        .unwrap();
    assert_success(&output);
    // The CLI lives with Recipe; the helper and the tools it runs as root are
    // at the paths compiled into the CLI and the helper.
    assert_eq!(
        std::fs::read_to_string(root.join("usr/libexec/recipe/ginux")).unwrap(),
        "stand-in cli"
    );
    for (path, text) in [
        // Recipe's guard: the one program the sudoers rule names.
        ("usr/libexec/recipe/recipe-ginux-guard", "stand-in guard"),
        ("usr/libexec/ginux/ginux-helper", "stand-in helper"),
        ("usr/libexec/ginux/ginux-image", "stand-in image"),
        ("usr/libexec/ginux/ginux-runtime", "stand-in runtime"),
        (
            "usr/lib64/ginux/libginux_allocator.so",
            "stand-in allocator",
        ),
    ] {
        assert_eq!(std::fs::read_to_string(root.join(path)).unwrap(), text);
        assert_eq!(mode(&root.join(path)), 0o755, "{path}");
    }
    assert_eq!(
        std::fs::read_to_string(root.join("usr/share/recipe/runtime/root-reference")).unwrap(),
        format!("{REFERENCE}\n")
    );
    // The guard's allowlist: the shipped reference, 0644, at the fixed path.
    assert_eq!(allowlisted(&root), [REFERENCE]);
    assert_eq!(mode(&root.join(ALLOWLIST)), 0o644);
    // The rule: root-owned by the package, 0440, names the guard, pinned to
    // Recipe's roots; no temporary file is left beside it.
    let rule = root.join("etc/sudoers.d/recipe-ginux");
    assert_eq!(mode(&rule), 0o440);
    let beside: Vec<_> = std::fs::read_dir(root.join("etc/sudoers.d"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(beside, ["recipe-ginux"], "{beside:?}");
    let text = std::fs::read_to_string(&rule).unwrap();
    assert!(
        text.lines().any(|line| line
            == "%wheel ALL=(root) NOPASSWD: /usr/libexec/recipe/recipe-ginux-guard --protocol 1 --state-root /var/lib/recipe/ginux --run-root /run/recipe/ginux *"),
        "{text}"
    );
    // No active line of the rule names the helper: every one of its verbs is root.
    assert!(
        !text
            .lines()
            .filter(|line| !line.starts_with('#'))
            .any(|line| line.contains("ginux-helper")),
        "{text}"
    );
    assert!(
        text.contains("recipe-<uid>-"),
        "the rule says what the guard allows: {text}"
    );
    // The user-namespace launch and its AppArmor profile are gone.
    assert!(!root.join("etc/apparmor.d").exists());

    // Another group, another rule; no group, no rule.
    let again = stage_command(&recipes, &root)
        .args(["--sudoers", "sudo"])
        .output()
        .unwrap();
    assert_success(&again);
    assert!(
        std::fs::read_to_string(&rule)
            .unwrap()
            .contains("\n%sudo ALL=(root) NOPASSWD:")
    );
    let without = stage_command(&recipes, &root).output().unwrap();
    assert_success(&without);
    assert!(!rule.exists(), "a rule an earlier stage wrote must go");

    // With a minimal image both shipped references are allowed; without it
    // again only the full one.
    let minimal = stage_command(&recipes, &root)
        .args(["--minimal-reference", MINIMAL])
        .output()
        .unwrap();
    assert_success(&minimal);
    assert_eq!(allowlisted(&root), [REFERENCE, MINIMAL]);
    assert_eq!(mode(&root.join(ALLOWLIST)), 0o644);
    let full_only = stage_command(&recipes, &root).output().unwrap();
    assert_success(&full_only);
    assert_eq!(allowlisted(&root), [REFERENCE]);
}

#[test]
fn runtime_stage_never_writes_the_allowlist_or_the_rule_through_a_symlink() {
    let (dir, recipes) = create_test_env();
    let root = dir.path().join("staged");
    let target = dir.path().join("target");
    std::fs::write(&target, "untouched").unwrap();
    std::fs::create_dir_all(root.join("usr/libexec/recipe")).unwrap();
    std::os::unix::fs::symlink(&target, root.join(ALLOWLIST)).unwrap();
    let output = stage_command(&recipes, &root).output().unwrap();
    assert!(
        !output.status.success(),
        "staged through a symlinked allowlist"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("symbolic link"));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");

    std::fs::remove_file(root.join(ALLOWLIST)).unwrap();
    std::fs::create_dir_all(root.join("etc/sudoers.d")).unwrap();
    std::os::unix::fs::symlink(&target, root.join("etc/sudoers.d/recipe-ginux")).unwrap();
    let output = stage_command(&recipes, &root)
        .args(["--sudoers", "wheel"])
        .output()
        .unwrap();
    assert!(!output.status.success(), "staged through a symlinked rule");
    assert!(String::from_utf8_lossy(&output.stderr).contains("symbolic link"));
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
}

#[test]
fn runtime_stage_refuses_a_group_that_is_not_a_group_name_and_the_retired_apparmor_option() {
    let (dir, recipes) = create_test_env();
    let root = dir.path().join("staged");
    for group in ["", "Wheel", "wheel ALL=(ALL) ALL", "a;b", "-x"] {
        let output = stage_command(&recipes, &root)
            .args(["--sudoers", group])
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted group {group:?}");
        assert!(!root.join("etc/sudoers.d/recipe-ginux").exists());
    }
    let output = stage_command(&recipes, &root)
        .arg("--apparmor")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--apparmor'"));
    // The guard is not optional: without it the rule would name nothing.
    let output = stage_command_with_guard(&recipes, &root, Some(&dir.path().join("no-such-guard")))
        .args(["--sudoers", "wheel"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--recipe-guard"));
    assert!(!root.join("etc/sudoers.d/recipe-ginux").exists());
    let output = stage_command(&recipes, &root)
        .args(["--launcher", "/bin/true"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--launcher'"));
}
