use super::*;
use sha2::{Digest, Sha256};

pub(super) fn copy_kernel_recipes(recipes: &Path) {
    let source = production_recipes();
    for name in ["linux.rhai", "linux-prebuilt.rhai", "linux-deps.rhai"] {
        std::fs::copy(source.join(name), recipes.join(name)).unwrap();
    }
    std::fs::create_dir_all(recipes.join("kernel")).unwrap();
    std::fs::copy(
        source.join("kernel/metadata.rhai"),
        recipes.join("kernel/metadata.rhai"),
    )
    .unwrap();
}

/// The kernel.release a variant's pin in `kernel/metadata.rhai` builds
/// (`<version>-<target>`), read from the copied recipe so the tests follow a
/// kernel bump.
pub(super) fn kernel_release(recipes: &Path, target: &str) -> String {
    let text = std::fs::read_to_string(recipes.join("kernel/metadata.rhai")).unwrap();
    let pin = text
        .find(&format!("{target}: #{{"))
        .unwrap_or_else(|| panic!("no pin for {target}"));
    let marker = "version: \"";
    let start = pin + text[pin..].find(marker).expect("pin version") + marker.len();
    let version = &text[start..start + text[start..].find('"').unwrap()];
    format!("{version}-{target}")
}

// The kernel recipe's build dependency, as a disposable fixture: these cases
// inspect the production kernel recipes against imported artifact trees
// without acquiring a compiler or building the kernel toolchain.
fn write_kernel_dependency_fixture(recipes: &Path) {
    write_recipe(
        recipes,
        "linux-deps",
        r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ tools: "directory" } };
fn build(src, out) {
    print("KERNEL_PREREQUISITE_INSTALL");
    write_file(join_path(out, "readiness-fixture"), "managed prerequisite");
}
"#,
    );
}

pub(super) fn kernel_command(recipes: &Path, action: &str, recipe: &str) -> Command {
    let mut command = kernel_command_for_target(recipes, action, recipe, "acorn");
    command.arg("--no-persist-state");
    command
}

fn kernel_command_for_target(recipes: &Path, action: &str, recipe: &str, target: &str) -> Command {
    let kconfig = recipes.parent().unwrap().join("config");
    if !kconfig.exists() {
        std::fs::write(&kconfig, "CONFIG_LOCALVERSION=\"fixture\"\n").unwrap();
    }
    let mut command = recipe_command(recipes);
    command
        .args([action, recipe])
        .args(["--input", &format!("kconfig={}", kconfig.display())])
        .args(["--define", &format!("KERNEL_TARGET={target}")])
        .arg("--recipes-path")
        .arg(recipes)
        .arg("--build-dir")
        .arg(recipes.parent().unwrap().join("work"));
    command
}

pub(super) fn invoke_kernel(
    mut command: Command,
    root: &Path,
) -> (std::process::Output, Option<serde_json::Value>) {
    let result = tempfile::NamedTempFile::new_in(root).unwrap();
    let output = command
        .arg("--json-output")
        .arg(result.path())
        .output()
        .unwrap();
    let ctx = if output.status.success() {
        Some(serde_json::from_slice(&std::fs::read(result.path()).unwrap()).unwrap())
    } else {
        None
    };
    (output, ctx)
}

/// A complete imported kernel bundle for `release`; `fault` leaves out what
/// a bundle must hold (`release` writes another release, `modules` has none).
fn write_artifacts(root: &Path, release: &str, fault: &str) -> std::path::PathBuf {
    let artifacts = root.join("existing kernel");
    let build = artifacts.join("kernel-build");
    let release_file = build.join("include/config/kernel.release");
    std::fs::create_dir_all(release_file.parent().unwrap()).unwrap();
    std::fs::write(
        &release_file,
        if fault == "release" {
            "6.18-wrong"
        } else {
            release
        },
    )
    .unwrap();
    std::fs::create_dir_all(build.join("arch/x86/boot")).unwrap();
    std::fs::write(
        build.join("arch/x86/boot/bzImage"),
        "disposable build artifact",
    )
    .unwrap();
    std::fs::create_dir_all(artifacts.join("staging/boot")).unwrap();
    std::fs::write(artifacts.join("staging/boot/vmlinuz"), "disposable image").unwrap();
    if fault != "modules" {
        let modules = artifacts.join("staging/usr/lib/modules").join(release);
        std::fs::create_dir_all(&modules).unwrap();
        std::fs::write(modules.join("fixture.ko"), "disposable module").unwrap();
    }
    let config_bytes = "CONFIG_LOCALVERSION=\"fixture\"\n";
    std::fs::write(
        build.join(".config.kconfig-hash"),
        format!(
            "{}  /inputs/kconfig",
            hex::encode(Sha256::digest(config_bytes))
        ),
    )
    .unwrap();
    artifacts
}

#[test]
fn cli_kernel_install_keeps_a_matching_bundle_and_rejects_others_offline() {
    for target in ["acorn", "levitate"] {
        for recipe in ["linux", "linux-prebuilt"] {
            for fault in ["none", "release", "modules"] {
                let (dir, recipes) = create_test_env();
                copy_kernel_recipes(&recipes);
                if recipe == "linux" {
                    write_kernel_dependency_fixture(&recipes);
                }
                let release = kernel_release(&recipes, target);
                let artifacts = write_artifacts(dir.path(), &release, fault);
                // The fixture's kconfig is the one whose hash the bundle records.
                std::fs::write(
                    dir.path().join("config"),
                    "CONFIG_LOCALVERSION=\"fixture\"\n",
                )
                .unwrap();
                let mut command = kernel_command_for_target(&recipes, "install", recipe, target);
                command
                    .args(["--no-persist-state", "--no-network"])
                    .args(["--input", &format!("target={}", artifacts.display())]);
                let (output, result) = invoke_kernel(command, dir.path());
                let stderr = String::from_utf8_lossy(&output.stderr);
                if fault == "none" {
                    // The imported bundle already is the pinned kernel: nothing
                    // is built or downloaded, and it is published.
                    assert_success(&output);
                    assert_eq!(
                        stderr.contains("KERNEL_PREREQUISITE_INSTALL"),
                        recipe == "linux",
                        "{stderr}"
                    );
                    let result = result.unwrap();
                    assert_eq!(result["state"], "installed");
                    let published = Path::new(result["outputs"]["bundle"].as_str().unwrap());
                    assert_eq!(
                        std::fs::read_to_string(published.join("staging/boot/vmlinuz")).unwrap(),
                        "disposable image"
                    );
                    assert_ne!(published, artifacts);
                } else {
                    // Another release or missing modules means the kernel is
                    // fetched, which the run may not do.
                    assert!(
                        !output.status.success(),
                        "{recipe} accepted a bundle with {fault}: {stderr}"
                    );
                    assert!(result.is_none());
                }
                assert_eq!(
                    std::fs::read_to_string(artifacts.join("staging/boot/vmlinuz")).unwrap(),
                    "disposable image"
                );
            }
        }
    }
}

#[test]
fn cli_kernel_removal_preserves_imported_artifact_tree() {
    for recipe in ["linux", "linux-prebuilt"] {
        let (dir, recipes) = create_test_env();
        copy_kernel_recipes(&recipes);
        let original = std::fs::read(recipes.join(format!("{recipe}.rhai"))).unwrap();
        let artifacts = dir.path().join("imported target");
        let image = artifacts.join("staging/boot/vmlinuz");
        std::fs::create_dir_all(image.parent().unwrap()).unwrap();
        std::fs::write(&image, "immutable imported image").unwrap();
        let mut command = kernel_command_for_target(&recipes, "remove", recipe, "acorn");
        command.args(["--input", &format!("target={}", artifacts.display())]);
        let (output, result) = invoke_kernel(command, dir.path());
        assert_success(&output);
        assert!(!String::from_utf8_lossy(&output.stderr).contains("KERNEL_PREREQUISITE_INSTALL"));
        assert!(result.unwrap()["outputs"].as_object().unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(&image).unwrap(),
            "immutable imported image"
        );
        assert_eq!(
            std::fs::read(recipes.join(format!("{recipe}.rhai"))).unwrap(),
            original
        );
    }
}
