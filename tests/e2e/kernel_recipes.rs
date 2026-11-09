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

#[test]
fn cli_kernel_readiness_preserves_release_module_and_configuration_rules() {
    for (target, release) in [("acorn", "7.3.3-acorn"), ("levitate", "6.12.116-levitate")] {
        for recipe in ["linux", "linux-prebuilt"] {
            let (dir, recipes) = create_test_env();
            copy_kernel_recipes(&recipes);
            if recipe == "linux" {
                // This case checks the real kernel recipe against disposable
                // imported artifacts, without building a kernel/toolchain.
                // Give its build dependency a real managed publication and
                // verify manual checks cannot create that prerequisite. The
                // production linux-deps acquisition is outside this fixture.
                write_recipe(
                    &recipes,
                    "linux-deps",
                    r#"const RECIPE_API = 2;
const NEEDS = #{ outputs: #{ tools: "directory" } };
let ctx = #{ name: "linux-deps" };
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) { ready(ctx) }
fn is_installed(ctx) {
    let marker = "/out/tools/readiness-fixture";
    if !is_file(marker) { return needs_work(ctx, "prepare the readiness fixture explicitly"); }
    if read_file(marker) != "managed prerequisite" { throw "changed prerequisite"; }
    ctx.outputs.tools = "/out/tools"; ready(ctx)
}
fn install(ctx) {
    print("KERNEL_PREREQUISITE_INSTALL");
    mkdir("/out/tools"); write_file("/out/tools/readiness-fixture", "managed prerequisite");
    ctx.outputs.tools = "/out/tools"; ctx
}
"#,
                );
                let (missing, _) = invoke_kernel(
                    kernel_command_for_target(&recipes, "isbuilt", recipe, target),
                    dir.path(),
                );
                assert!(!missing.status.success());
                let stderr = String::from_utf8_lossy(&missing.stderr);
                assert!(
                    stderr.contains("requires matching cached products"),
                    "{stderr}"
                );
                assert!(!stderr.contains("KERNEL_PREREQUISITE_INSTALL"), "{stderr}");
            }
            let artifacts = dir.path().join("existing kernel");
            let build = artifacts.join("kernel-build");
            let release_file = build.join("include/config/kernel.release");
            let modules = artifacts.join("staging/usr/lib/modules").join(release);
            std::fs::create_dir_all(release_file.parent().unwrap()).unwrap();
            std::fs::write(&release_file, release).unwrap();
            std::fs::create_dir_all(build.join("arch/x86/boot")).unwrap();
            std::fs::write(
                build.join("arch/x86/boot/bzImage"),
                "disposable build artifact",
            )
            .unwrap();
            std::fs::create_dir_all(artifacts.join("staging/boot")).unwrap();
            std::fs::write(artifacts.join("staging/boot/vmlinuz"), "disposable image").unwrap();
            std::fs::create_dir_all(&modules).unwrap();
            std::fs::write(modules.join("fixture.ko"), "disposable module").unwrap();
            let kconfig = dir.path().join("config");
            let config_bytes = "CONFIG_LOCALVERSION=\"fixture\"\n";
            std::fs::write(&kconfig, config_bytes).unwrap();
            std::fs::write(
                build.join(".config.kconfig-hash"),
                format!(
                    "{}  /inputs/kconfig",
                    hex::encode(Sha256::digest(config_bytes))
                ),
            )
            .unwrap();
            let run = |action: &str| {
                let mut cmd = kernel_command_for_target(&recipes, action, recipe, target);
                cmd.args(["--input", &format!("target={}", artifacts.display())]);
                invoke_kernel(cmd, dir.path())
            };
            if recipe == "linux" {
                // Install the graph explicitly. The imported complete kernel
                // makes the real parent ready, so no kernel build is run; the
                // child publication is retained at its normal graph destination.
                let (prepared, _) = run("install");
                assert_success(&prepared);
                assert!(
                    String::from_utf8_lossy(&prepared.stderr)
                        .contains("KERNEL_PREREQUISITE_INSTALL")
                );
            }
            for action in ["isbuilt", "isinstalled"] {
                let (output, ctx) = run(action);
                assert_success(&output);
                assert!(
                    !String::from_utf8_lossy(&output.stderr)
                        .contains("KERNEL_PREREQUISITE_INSTALL"),
                    "manual readiness must not install its dependency"
                );
                let ctx = ctx.unwrap();
                assert_eq!(ctx["kernel_version"], release);
                assert_eq!(ctx["build_dir"], "/out/kernel-build");
                if action == "isinstalled" {
                    let published = Path::new(ctx["outputs"]["kernel"].as_str().unwrap());
                    assert_eq!(
                        std::fs::read_to_string(published).unwrap(),
                        "disposable image"
                    );
                    assert_ne!(published, artifacts.join("staging/boot/vmlinuz"));
                }
            }
            std::fs::write(&release_file, "6.18-wrong").unwrap();
            let (output, _) = run("isinstalled");
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("Wrong kernel"));
            std::fs::write(&release_file, release).unwrap();
            std::fs::rename(&modules, modules.with_extension("stale")).unwrap();
            let (output, _) = run("isinstalled");
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("modules"));
            if recipe == "linux" {
                std::fs::write(&kconfig, "CONFIG_CHANGED=y\n").unwrap();
                let (output, _) = run("isbuilt");
                assert!(!output.status.success());
                assert!(String::from_utf8_lossy(&output.stderr).contains("configuration changed"));
            }
            assert_eq!(
                std::fs::read_to_string(artifacts.join("staging/boot/vmlinuz")).unwrap(),
                "disposable image"
            );
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
        let mut command = kernel_command(&recipes, "remove", recipe);
        command.args(["--input", &format!("target={}", artifacts.display())]);
        let (output, ctx) = invoke_kernel(command, dir.path());
        assert_success(&output);
        assert!(ctx.unwrap()["outputs"].as_object().unwrap().is_empty());
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

#[test]
fn cli_kernel_cleanup_rejects_paths_outside_owned_work() {
    let (dir, recipes) = create_test_env();
    copy_kernel_recipes(&recipes);
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("preserve"), "preserve").unwrap();
    use std::io::Write;
    let mut child = std::fs::OpenOptions::new()
        .append(true)
        .open(recipes.join("linux.rhai"))
        .unwrap();
    writeln!(
        child,
        "ctx.source_dir_name = \"../outside\"; ctx.tarball_filename = \"../outside.tar.xz\";"
    )
    .unwrap();
    let (output, _) = invoke_kernel(kernel_command(&recipes, "cleanup", "linux"), dir.path());
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsafe kernel cleanup"));
    assert_eq!(
        std::fs::read_to_string(outside.join("preserve")).unwrap(),
        "preserve"
    );
}
