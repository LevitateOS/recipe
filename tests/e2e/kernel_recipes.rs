use super::*;

fn copy_kernel_recipes(recipes: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../distro-builder/recipes");
    for name in ["linux.rhai", "linux-base.rhai", "linux-prebuilt.rhai"] {
        std::fs::copy(source.join(name), recipes.join(name)).unwrap();
    }
}

fn kernel_command(recipes: &Path, action: &str, recipe: &str) -> Command {
    let mut command = recipe_command(recipes);
    command
        .args([action, recipe, "--no-persist-ctx"])
        .arg("--recipes-path")
        .arg(recipes)
        .arg("--build-dir")
        .arg(recipes.parent().unwrap().join("acorn/downloads"));
    command
}

#[test]
fn cli_kernel_cleanup_initializes_paths_and_preserves_shared_downloads() {
    let (dir, recipes) = create_test_env();
    copy_kernel_recipes(&recipes);
    let downloads = dir.path().join("acorn/downloads");
    std::fs::create_dir_all(&downloads).unwrap();
    let unrelated = downloads.join("unrelated-download.txt");
    std::fs::write(&unrelated, "preserve").unwrap();
    let original_base = std::fs::read(recipes.join("linux-base.rhai")).unwrap();

    let output = kernel_command(&recipes, "cleanup", "linux")
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "preserve");
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let source = downloads.join(ctx["source_dir_name"].as_str().unwrap());
    let tarball = downloads.join(ctx["tarball_filename"].as_str().unwrap());
    assert_ne!(source, downloads);
    assert_ne!(tarball, downloads);
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("Makefile"), "disposable kernel source").unwrap();
    std::fs::write(&tarball, "disposable archive").unwrap();

    let output = kernel_command(&recipes, "cleanup", "linux")
        .output()
        .unwrap();
    assert_success(&output);
    assert!(!source.exists());
    assert!(!tarball.exists());
    assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "preserve");
    assert_eq!(
        std::fs::read(recipes.join("linux-base.rhai")).unwrap(),
        original_base
    );
}

#[test]
fn cli_kernel_cleanup_rejects_state_paths_outside_downloads() {
    let (dir, recipes) = create_test_env();
    copy_kernel_recipes(&recipes);
    let outside = dir.path().join("acorn/outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("preserve.txt"), "preserve").unwrap();
    let mut child = std::fs::OpenOptions::new()
        .append(true)
        .open(recipes.join("linux.rhai"))
        .unwrap();
    use std::io::Write;
    writeln!(
        child,
        "\nctx.source_dir_name = \"../outside\";\nctx.tarball_filename = \"../outside.tar.xz\";"
    )
    .unwrap();

    let output = kernel_command(&recipes, "cleanup", "linux")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsafe kernel cleanup"));
    assert_eq!(
        std::fs::read_to_string(outside.join("preserve.txt")).unwrap(),
        "preserve"
    );
}

#[test]
fn cli_kernel_removal_targets_current_artifacts_without_creating_directories() {
    for recipe in ["linux", "linux-prebuilt"] {
        let (dir, recipes) = create_test_env();
        copy_kernel_recipes(&recipes);
        let artifact_root = dir.path().join("artifacts/kernel");
        let children = ["kernel-build", "kernel-build.tmp", "staging", "staging.tmp"];
        for name in children {
            let path = artifact_root.join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("payload"), "disposable kernel output").unwrap();
        }
        let unrelated = artifact_root.join("preserve.txt");
        std::fs::write(&unrelated, "preserve").unwrap();
        let root_define = format!("KERNEL_ARTIFACT_ROOT={}", artifact_root.display());

        let output = kernel_command(&recipes, "remove", recipe)
            .args(["--define", &root_define])
            .output()
            .unwrap();
        assert_success(&output);
        for name in children {
            assert!(!artifact_root.join(name).exists());
        }
        assert_eq!(std::fs::read_to_string(&unrelated).unwrap(), "preserve");

        std::fs::remove_file(&unrelated).unwrap();
        std::fs::remove_dir(&artifact_root).unwrap();
        let output = kernel_command(&recipes, "remove", recipe)
            .args(["--define", &root_define])
            .output()
            .unwrap();
        assert_success(&output);
        assert!(
            !artifact_root.exists(),
            "remove recreated the absent artifact root"
        );
    }
}

#[test]
fn cli_kernel_removal_rejects_shared_or_glob_roots() {
    for recipe in ["linux", "linux-prebuilt"] {
        let (dir, recipes) = create_test_env();
        copy_kernel_recipes(&recipes);
        let preserve = dir.path().join("preserve.txt");
        std::fs::write(&preserve, "preserve").unwrap();
        for invalid_root in [".", "artifacts/..", "artifacts/*"] {
            let define = format!("KERNEL_ARTIFACT_ROOT={invalid_root}");
            let output = kernel_command(&recipes, "remove", recipe)
                .args(["--define", &define])
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("unsafe kernel artifact root")
            );
            assert_eq!(std::fs::read_to_string(&preserve).unwrap(), "preserve");
        }
    }
}

#[test]
fn cli_kernel_checks_honor_force_define_over_legacy_environment() {
    for (recipe, action, forced_message, missing_message) in [
        (
            "linux",
            "isbuilt",
            "force rebuild requested",
            "kernel not built",
        ),
        (
            "linux-prebuilt",
            "isacquired",
            "forced prebuilt refresh requested",
            "Missing kernel.release",
        ),
    ] {
        let (dir, recipes) = create_test_env();
        copy_kernel_recipes(&recipes);
        let root_define = format!(
            "KERNEL_ARTIFACT_ROOT={}",
            dir.path().join("kernel").display()
        );
        for (force, legacy, expected) in [("1", "0", forced_message), ("0", "1", missing_message)] {
            let define = format!("KERNEL_FORCE_REBUILD={force}");
            let output = kernel_command(&recipes, action, recipe)
                .args([
                    "--define",
                    "KERNEL_KCONFIG_PATH=",
                    "--define",
                    &root_define,
                    "--define",
                    &define,
                ])
                .env("KERNEL_FORCE_REBUILD", legacy)
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains(expected),
                "{recipe}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!dir.path().join("kernel").exists());
        }
        let output = kernel_command(&recipes, action, recipe)
            .args(["--define", "KERNEL_KCONFIG_PATH=", "--define", &root_define])
            .env("KERNEL_FORCE_REBUILD", "1")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(forced_message));
    }
}

#[test]
fn cli_prebuilt_kernel_check_does_not_require_kconfig_define() {
    let (dir, recipes) = create_test_env();
    copy_kernel_recipes(&recipes);
    let define = format!(
        "KERNEL_ARTIFACT_ROOT={}",
        dir.path().join("kernel").display()
    );
    let output = kernel_command(&recipes, "isacquired", "linux-prebuilt")
        .args(["--define", &define])
        .env_remove("KERNEL_FORCE_REBUILD")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("Missing kernel.release"), "{error}");
    assert!(!error.contains("Variable not found"));
    assert!(!dir.path().join("kernel").exists());
}
