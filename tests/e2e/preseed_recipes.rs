use super::*;
use sha2::{Digest, Sha256};

fn preseed_command(recipes: &Path, recipe: &str, action: &str) -> Command {
    let mut command = recipe_command(recipes);
    command
        .args([action, recipe, "--no-persist-ctx"])
        .arg("--recipes-path")
        .arg(recipes)
        .arg("--build-dir")
        .arg(recipes.parent().unwrap().join("build"));
    for name in [
        "ALPINE_FORCE_REFRESH",
        "FEDORA_FORCE_REFRESH",
        "ROOTFS_SOURCE_FORCE_REFRESH",
    ] {
        command.env_remove(name);
    }
    command
}

fn prepare_assets(recipes: &Path, recipe: &str) -> serde_json::Value {
    let path = recipes.join(format!("{recipe}.rhai"));
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../distro-builder/recipes");
    std::fs::create_dir_all(recipes.join("sources")).unwrap();
    for distro in ["alpine", "fedora"] {
        std::fs::copy(
            source_root.join(format!("sources/{distro}.rhai")),
            recipes.join(format!("sources/{distro}.rhai")),
        )
        .unwrap();
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../distro-builder/recipes")
        .join(format!("{recipe}.rhai"));
    std::fs::copy(source, &path).unwrap();
    let output = preseed_command(recipes, recipe, "cleanup")
        .output()
        .unwrap();
    assert_success(&output);
    let mut ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let alpine = recipe.starts_with("alpine");
    let fields = if alpine {
        vec![
            ("iso_path", "iso_sha256"),
            ("apk_tools_path", "apk_tools_sha256"),
        ]
    } else {
        vec![("iso_path", "sha256")]
    };
    let facts_path = recipes.join(if alpine {
        "sources/alpine.rhai"
    } else {
        "sources/fedora.rhai"
    });
    let mut facts = std::fs::read_to_string(&facts_path).unwrap();
    // Small synthetic inputs replace only package checksum facts. All lifecycle
    // and verification code comes from the actual repository recipe.
    for (path_field, hash_field) in fields {
        let asset = Path::new(ctx[path_field].as_str().unwrap());
        std::fs::create_dir_all(asset.parent().unwrap()).unwrap();
        let bytes = format!("verified fixture for {path_field}");
        std::fs::write(asset, &bytes).unwrap();
        let digest = format!("{:x}", Sha256::digest(bytes.as_bytes()));
        facts = facts.replace(ctx[hash_field].as_str().unwrap(), &digest);
        ctx[hash_field] = serde_json::Value::String(digest);
    }
    std::fs::write(facts_path, facts).unwrap();
    let iso_size = std::fs::metadata(ctx["iso_path"].as_str().unwrap())
        .unwrap()
        .len();
    let (marker_name, marker) = if alpine {
        let apk_size = std::fs::metadata(ctx["apk_tools_path"].as_str().unwrap())
            .unwrap()
            .len();
        (
            ".alpine-source-preseed.marker",
            format!(
                "iso_sha256={}\niso_fingerprint={iso_size}\napk_tools_sha256={}\napk_tools_fingerprint={apk_size}\n",
                ctx["iso_sha256"].as_str().unwrap(),
                ctx["apk_tools_sha256"].as_str().unwrap()
            ),
        )
    } else {
        (
            ".fedora-iso-trust.marker",
            format!(
                "sha256={}\nfingerprint={iso_size}\n",
                ctx["sha256"].as_str().unwrap()
            ),
        )
    };
    std::fs::write(
        Path::new(ctx["trust_dir"].as_str().unwrap()).join(marker_name),
        marker,
    )
    .unwrap();
    ctx
}

#[test]
fn cli_preseed_checks_reject_same_size_corruption_of_verified_assets() {
    for recipe in ["alpine-preseed-source-assets", "fedora-preseed-iso"] {
        let (_dir, recipes) = create_test_env();
        let ctx = prepare_assets(&recipes, recipe);
        for action in ["isacquired", "isbuilt", "isinstalled"] {
            let output = preseed_command(&recipes, recipe, action).output().unwrap();
            assert_success(&output);
        }
        let output = preseed_command(&recipes, recipe, "install")
            .output()
            .unwrap();
        assert_success(&output);
        assert!(String::from_utf8_lossy(&output.stderr).contains("already installed"));

        let fields = if recipe.starts_with("alpine") {
            vec!["iso_path", "apk_tools_path"]
        } else {
            vec!["iso_path"]
        };
        for field in fields {
            let asset = Path::new(ctx[field].as_str().unwrap());
            let original = std::fs::read(asset).unwrap();
            let mut bytes = original.clone();
            bytes[0] ^= 1;
            std::fs::write(asset, bytes).unwrap();
            for action in ["isacquired", "isbuilt", "isinstalled"] {
                let output = preseed_command(&recipes, recipe, action).output().unwrap();
                assert!(
                    !output.status.success(),
                    "{recipe} {action} accepted changed {field} content"
                );
                assert!(String::from_utf8_lossy(&output.stderr).contains("integrity check failed"));
            }
            std::fs::write(asset, original).unwrap();
        }
    }
}

#[test]
fn cli_preseed_installed_checks_honor_refresh_before_cache_hit() {
    for (recipe, refresh) in [
        ("alpine-preseed-source-assets", "ALPINE_FORCE_REFRESH"),
        ("fedora-preseed-iso", "FEDORA_FORCE_REFRESH"),
        ("fedora-preseed-iso", "ROOTFS_SOURCE_FORCE_REFRESH"),
    ] {
        let (_dir, recipes) = create_test_env();
        let ctx = prepare_assets(&recipes, recipe);
        let iso = ctx["iso_path"].as_str().unwrap();
        let before = std::fs::read(iso).unwrap();
        let output = preseed_command(&recipes, recipe, "isinstalled")
            .env(refresh, "1")
            .output()
            .unwrap();
        assert!(!output.status.success(), "{recipe} ignored {refresh}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("forced refresh"));
        assert_eq!(std::fs::read(iso).unwrap(), before);
    }
}

#[test]
fn cli_shared_preseed_markers_handle_quoted_asset_paths() {
    for recipe in ["alpine-preseed-source-assets", "fedora-preseed-iso"] {
        let dir = tempfile::Builder::new()
            .prefix("recipe 'quoted' $(touch INJECTED) ")
            .tempdir()
            .unwrap();
        let recipes = dir.path().join("recipes");
        std::fs::create_dir(&recipes).unwrap();
        prepare_assets(&recipes, recipe);
        let output = preseed_command(&recipes, recipe, "isinstalled")
            .output()
            .unwrap();
        assert_success(&output);
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
        assert!(!dir.path().join("INJECTED").exists());
    }
}

#[test]
fn cli_rootfs_consumers_reject_changed_cache_and_staged_assets() {
    for (producer, consumer, fields, marker) in [
        (
            "fedora-preseed-iso",
            "fedora-dvd-source-rootfs",
            vec![("iso_path", "preseed_iso_path", "sha256")],
            ".fedora-iso-trust.marker",
        ),
        (
            "alpine-preseed-source-assets",
            "alpine-live-source-rootfs",
            vec![
                ("iso_path", "iso_preseed_path", "iso_sha256"),
                (
                    "apk_tools_path",
                    "apk_tools_preseed_path",
                    "apk_tools_sha256",
                ),
            ],
            ".alpine-source-preseed.marker",
        ),
    ] {
        let (dir, recipes) = create_test_env();
        let producer_ctx = prepare_assets(&recipes, producer);
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../distro-builder/recipes")
            .join(format!("{consumer}.rhai"));
        let recipe_path = recipes.join(format!("{consumer}.rhai"));
        std::fs::copy(source, &recipe_path).unwrap();
        let work = dir.path().join("work/source");
        let command = |action: &str| {
            let mut cmd = recipe_command(&recipes);
            cmd.args([action, consumer, "--no-persist-ctx"])
                .arg("--recipes-path")
                .arg(&recipes)
                .arg("--build-dir")
                .arg(&work)
                .env("FORCE_TRUST_CREATE", "0");
            cmd
        };
        let output = command("cleanup").output().unwrap();
        assert_success(&output);
        let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let mut assets = Vec::new();
        // Both recipes import the same fixture checksum facts.
        for (producer_field, cache_field, _hash_field) in fields {
            let cache_path = std::path::PathBuf::from(ctx[cache_field].as_str().unwrap());
            std::fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
            std::fs::copy(producer_ctx[producer_field].as_str().unwrap(), &cache_path).unwrap();
            assets.push(cache_path);
        }
        std::fs::copy(
            Path::new(producer_ctx["trust_dir"].as_str().unwrap()).join(marker),
            Path::new(ctx["trust_dir"].as_str().unwrap()).join(marker),
        )
        .unwrap();
        if consumer.starts_with("alpine") {
            // Acquired-state fixture includes a previously extracted tool;
            // this scenario verifies asset integrity and does not execute it.
            std::fs::create_dir_all(work.join("apk-tools/sbin")).unwrap();
            std::fs::write(
                work.join("apk-tools/sbin/apk.static"),
                "extracted tool fixture",
            )
            .unwrap();
        }
        let output = command("isacquired").output().unwrap();
        assert_success(&output);
        let acquired: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assets.push(std::path::PathBuf::from(
            acquired["iso_path"].as_str().unwrap(),
        ));
        if consumer.starts_with("alpine") {
            let apk_name = Path::new(ctx["apk_tools_preseed_path"].as_str().unwrap())
                .file_name()
                .unwrap();
            assets.push(work.join("apk-tools").join(apk_name));
        }
        for asset in assets {
            let original = std::fs::read(&asset).unwrap();
            let mut changed = original.clone();
            changed[0] ^= 1;
            std::fs::write(&asset, changed).unwrap();
            let output = command("isacquired").output().unwrap();
            assert!(
                !output.status.success(),
                "{consumer} accepted changed asset {}",
                asset.display()
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains("integrity check failed"));
            std::fs::write(asset, original).unwrap();
        }
    }
}
