use super::*;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

fn preseed_command(recipes: &Path, recipe: &str, action: &str, target: &Path) -> Command {
    let mut command = recipe_command(recipes);
    command
        .args([action, recipe, "--no-persist-state"])
        .args(["--input", &format!("target={}", target.display())])
        .arg("--recipes-path")
        .arg(recipes)
        .arg("--build-dir")
        .arg(recipes.parent().unwrap().join("work"));
    command
}

/// Small real files replace only the source facts for these integrity cases.
/// They are never presented as usable ISO/APK products or executed build tools.
fn prepare_assets(recipes: &Path, recipe: &str) -> (PathBuf, Vec<PathBuf>) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../distro/builder/recipes");
    std::fs::create_dir_all(recipes.join("sources")).unwrap();
    for distro in ["alpine", "fedora"] {
        std::fs::copy(
            source.join(format!("sources/{distro}.rhai")),
            recipes.join(format!("sources/{distro}.rhai")),
        )
        .unwrap();
    }
    std::fs::copy(
        source.join(format!("{recipe}.rhai")),
        recipes.join(format!("{recipe}.rhai")),
    )
    .unwrap();
    let target = recipes.parent().unwrap().join("verified assets");
    std::fs::create_dir_all(&target).unwrap();
    let alpine = recipe.starts_with("alpine");
    let entries = if alpine {
        vec![
            (
                "alpine-extended-3.23.2-x86_64.iso",
                "8d50854936dba58e7616ac3bfa07ad67dac0347305fec1cba6d288cf5df1577d",
            ),
            (
                "apk-tools-static-3.0.9-r0.apk",
                "ea77b2533566e70c9566c03748199b79348010c3199ba02d97e0634cf0fa9ef7",
            ),
        ]
    } else {
        vec![(
            "Fedora-Server-dvd-x86_64-43-1.6.iso",
            "aca06983bef83da9b43144c1a2ff4c8483e4745167c17f53725c16a16742e643",
        )]
    };
    let facts_path = recipes.join(if alpine {
        "sources/alpine.rhai"
    } else {
        "sources/fedora.rhai"
    });
    let mut facts = std::fs::read_to_string(&facts_path).unwrap();
    let mut files = Vec::new();
    for (name, hash) in entries {
        let file = target.join(name);
        let bytes = format!("verified fixture for {name}");
        std::fs::write(&file, &bytes).unwrap();
        facts = facts.replace(hash, &hex::encode(Sha256::digest(bytes.as_bytes())));
        files.push(file);
    }
    std::fs::write(facts_path, facts).unwrap();
    (target, files)
}

#[test]
fn cli_preseed_checks_reject_same_size_corruption_of_verified_assets() {
    for recipe in ["alpine-preseed-source-assets", "fedora-preseed-iso"] {
        let (dir, recipes) = create_test_env();
        let (target, files) = prepare_assets(&recipes, recipe);
        for action in ["isacquired", "isbuilt", "isinstalled", "install"] {
            let (output, ctx) = super::kernel_recipes::invoke_kernel(
                preseed_command(&recipes, recipe, action, &target),
                dir.path(),
            );
            assert_success(&output);
            let ctx = ctx.unwrap();
            let iso = Path::new(ctx["outputs"]["iso"].as_str().unwrap());
            assert_eq!(
                std::fs::read(iso).unwrap(),
                std::fs::read(&files[0]).unwrap()
            );
            assert_ne!(iso, files[0]);
        }
        for file in files {
            let original = std::fs::read(&file).unwrap();
            let mut changed = original.clone();
            changed[0] ^= 1;
            std::fs::write(&file, changed).unwrap();
            for action in ["isacquired", "isbuilt", "isinstalled"] {
                let (output, _) = super::kernel_recipes::invoke_kernel(
                    preseed_command(&recipes, recipe, action, &target),
                    dir.path(),
                );
                assert!(!output.status.success(), "{recipe} accepted changed asset");
                assert!(String::from_utf8_lossy(&output.stderr).contains("integrity check failed"));
            }
            std::fs::write(file, original).unwrap();
        }
    }
}

#[test]
fn cli_preseed_explicit_inputs_handle_quoted_host_paths() {
    for recipe in ["alpine-preseed-source-assets", "fedora-preseed-iso"] {
        let dir = tempfile::Builder::new()
            .prefix("recipe 'quoted' $(touch INJECTED) ")
            .tempdir()
            .unwrap();
        let recipes = dir.path().join("recipes");
        std::fs::create_dir(&recipes).unwrap();
        let (target, _) = prepare_assets(&recipes, recipe);
        let (output, _) = super::kernel_recipes::invoke_kernel(
            preseed_command(&recipes, recipe, "isinstalled", &target),
            dir.path(),
        );
        assert_success(&output);
        assert!(!dir.path().join("INJECTED").exists());
    }
}

#[test]
fn cli_alpine_refresh_preserves_previous_publication_and_inputs_on_failure() {
    use super::downloads::serve_download;
    for failure in [None, Some("iso"), Some("apk")] {
        let (dir, recipes) = create_test_env();
        let recipe = "alpine-preseed-source-assets";
        let (target, files) = prepare_assets(&recipes, recipe);
        let before: Vec<_> = files
            .iter()
            .map(|file| std::fs::read(file).unwrap())
            .collect();
        let (initial, ctx) = super::kernel_recipes::invoke_kernel(
            preseed_command(&recipes, recipe, "isinstalled", &target),
            dir.path(),
        );
        assert_success(&initial);
        let prior_iso = PathBuf::from(ctx.unwrap()["outputs"]["iso"].as_str().unwrap());
        let iso_bytes = if failure == Some("iso") {
            b"corrupt ISO".as_slice()
        } else {
            &before[0]
        };
        let (iso_url, iso_server) = serve_download(iso_bytes, iso_bytes.len());
        let apk_response = if failure != Some("iso") {
            let apk_bytes = if failure == Some("apk") {
                b"corrupt APK".as_slice()
            } else {
                &before[1]
            };
            Some(serve_download(apk_bytes, apk_bytes.len()))
        } else {
            None
        };
        let path = recipes.join(format!("{recipe}.rhai"));
        let mut source = std::fs::read_to_string(&path).unwrap().replace(
            "iso_url: assets.iso_url,",
            &format!("iso_url: {iso_url:?},"),
        );
        if let Some((url, _)) = &apk_response {
            source = source.replace(
                "apk_tools_url: assets.apk_tools_url,",
                &format!("apk_tools_url: {url:?},"),
            );
        }
        std::fs::write(&path, &source).unwrap();
        let mut command = preseed_command(&recipes, recipe, "install", &target);
        command.args(["--force-acquire", "--force-install"]);
        for url in std::iter::once(&iso_url).chain(apk_response.as_ref().map(|(url, _)| url)) {
            let authority = url.trim_start_matches("http://").split('/').next().unwrap();
            command.args(["--network-host", authority]);
        }
        let (output, _) = super::kernel_recipes::invoke_kernel(command, dir.path());
        iso_server.join().unwrap();
        if let Some((_, server)) = apk_response {
            server.join().unwrap();
        }
        if failure.is_some() {
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("integrity check failed"));
        } else {
            assert_success(&output);
        }
        for (file, bytes) in files.iter().zip(&before) {
            assert_eq!(&std::fs::read(file).unwrap(), bytes);
        }
        assert_eq!(std::fs::read(prior_iso).unwrap(), before[0]);
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }
}

#[test]
fn cli_rootfs_consumers_reject_changed_explicit_assets() {
    for (producer, consumer) in [
        ("fedora-preseed-iso", "fedora-dvd-source-rootfs"),
        ("alpine-preseed-source-assets", "alpine-live-source-rootfs"),
    ] {
        let (dir, recipes) = create_test_env();
        let (_target, files) = prepare_assets(&recipes, producer);
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../distro/builder/recipes");
        std::fs::copy(
            source.join(format!("{consumer}.rhai")),
            recipes.join(format!("{consumer}.rhai")),
        )
        .unwrap();
        if consumer == "fedora-dvd-source-rootfs" {
            std::fs::copy(
                source.join("dvd-source-deps.rhai"),
                recipes.join("dvd-source-deps.rhai"),
            )
            .unwrap();
        }
        for file in &files {
            let original = std::fs::read(file).unwrap();
            let mut changed = original.clone();
            changed[0] ^= 1;
            std::fs::write(file, changed).unwrap();
            let mut command = recipe_command(&recipes);
            command
                .args(["isacquired", consumer, "--no-persist-state"])
                .arg("--recipes-path")
                .arg(&recipes)
                .arg("--build-dir")
                .arg(dir.path().join("materializer"))
                .args(["--input", &format!("iso={}", files[0].display())]);
            if files.len() == 2 {
                command.args(["--input", &format!("apk_tools={}", files[1].display())]);
            }
            let (output, _) = super::kernel_recipes::invoke_kernel(command, dir.path());
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("integrity check failed"));
            std::fs::write(file, original).unwrap();
        }
    }
}
