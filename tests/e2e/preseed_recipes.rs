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
    for facts in ["alpine.rhai", "levitate-rpm-lock.rhai"] {
        std::fs::copy(
            source.join(format!("sources/{facts}")),
            recipes.join(format!("sources/{facts}")),
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
    let lock = std::fs::read_to_string(recipes.join("sources/levitate-rpm-lock.rhai")).unwrap();
    let entries: Vec<(String, String)> = if alpine {
        // Read the trusted producer's pure facts rather than maintaining a
        // second package list that can silently omit new required assets.
        let engine = rhai::Engine::new();
        let ast = engine
            .compile_file(recipes.join("sources/alpine.rhai"))
            .unwrap();
        let facts: rhai::Map = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "facts", ())
            .unwrap();
        let mut entries = vec![
            (
                facts["iso_name"].clone().into_string().unwrap(),
                facts["iso_sha256"].clone().into_string().unwrap(),
            ),
            (
                facts["apk_tools_name"].clone().into_string().unwrap(),
                facts["apk_tools_sha256"].clone().into_string().unwrap(),
            ),
        ];
        for apk in facts["extra_apks"].clone_cast::<rhai::Array>() {
            let apk = apk.cast::<rhai::Array>();
            entries.push((
                format!("extra-apks/{}", apk[0].clone().into_string().unwrap()),
                apk[1].clone().into_string().unwrap(),
            ));
        }
        entries
    } else {
        // Each locked RPM: [name, NEVRA, URL, SHA256].
        lock.lines()
            .filter_map(|line| {
                let row: Vec<&str> = line.trim().strip_prefix("[\"")?.split("\", \"").collect();
                Some((
                    row[2].rsplit('/').next()?.to_owned(),
                    row[3].trim_end_matches("\"],").to_owned(),
                ))
            })
            .collect()
    };
    let facts_path = recipes.join(if alpine {
        "sources/alpine.rhai"
    } else {
        "sources/levitate-rpm-lock.rhai"
    });
    // The RPM set lives in the `rpms` directory of the preseed output.
    let asset_dir = if alpine {
        target.clone()
    } else {
        target.join("rpms")
    };
    std::fs::create_dir_all(&asset_dir).unwrap();
    let mut facts = std::fs::read_to_string(&facts_path).unwrap();
    let mut files = Vec::new();
    for (name, hash) in entries {
        let file = asset_dir.join(&name);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let bytes = format!("verified fixture for {name}");
        std::fs::write(&file, &bytes).unwrap();
        facts = facts.replace(&hash, &hex::encode(Sha256::digest(bytes.as_bytes())));
        files.push(file);
    }
    std::fs::write(facts_path, facts).unwrap();
    (target, files)
}

#[test]
fn cli_preseed_checks_reject_same_size_corruption_of_verified_assets() {
    for recipe in ["alpine-preseed-source-assets", "fedora-rpm-preseed"] {
        let (dir, recipes) = create_test_env();
        let (target, files) = prepare_assets(&recipes, recipe);
        for action in ["isacquired", "isbuilt", "isinstalled", "install"] {
            let (output, ctx) = super::kernel_recipes::invoke_kernel(
                preseed_command(&recipes, recipe, action, &target),
                dir.path(),
            );
            assert_success(&output);
            let ctx = ctx.unwrap();
            let published = if recipe.starts_with("alpine") {
                PathBuf::from(ctx["outputs"]["iso"].as_str().unwrap())
            } else {
                Path::new(ctx["outputs"]["rpms"].as_str().unwrap())
                    .join(files[0].file_name().unwrap())
            };
            assert_eq!(
                std::fs::read(&published).unwrap(),
                std::fs::read(&files[0]).unwrap()
            );
            assert_ne!(published, files[0]);
            if recipe.starts_with("alpine") {
                let extras = Path::new(ctx["outputs"]["extra_apks"].as_str().unwrap());
                for file in files.iter().skip(2) {
                    assert_eq!(
                        std::fs::read(extras.join(file.file_name().unwrap())).unwrap(),
                        std::fs::read(file).unwrap()
                    );
                }
            }
        }
        // Preserve both main Alpine checks and sample its additional package
        // set; the first/last RPMs stand for the rest of the lock.
        let sampled = if recipe.starts_with("alpine") {
            vec![
                files[0].clone(),
                files[1].clone(),
                files.last().unwrap().clone(),
            ]
        } else {
            vec![
                files.first().unwrap().clone(),
                files.last().unwrap().clone(),
            ]
        };
        for file in sampled {
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
    for recipe in ["alpine-preseed-source-assets", "fedora-rpm-preseed"] {
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
    use super::downloads::{serve_download, serve_files};
    for failure in [None, Some("iso"), Some("apk"), Some("extra")] {
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
        let initial = ctx.unwrap();
        let prior_iso = PathBuf::from(initial["outputs"]["iso"].as_str().unwrap());
        let prior_extras = PathBuf::from(initial["outputs"]["extra_apks"].as_str().unwrap());
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
        let extra_response = if failure.is_none() || failure == Some("extra") {
            // Extras are served where the preseed asks for them: under their
            // pinned repository, below the mirror root (extra_apks_url).
            let engine = rhai::Engine::new();
            let ast = engine
                .compile_file(recipes.join("sources/alpine.rhai"))
                .unwrap();
            let facts: rhai::Map = engine
                .call_fn(&mut rhai::Scope::new(), &ast, "facts", ())
                .unwrap();
            let repositories: Vec<String> = facts["extra_apks"]
                .clone_cast::<rhai::Array>()
                .into_iter()
                .map(|apk| apk.cast::<rhai::Array>()[2].clone().into_string().unwrap())
                .collect();
            let last = files.len() - 1;
            let payloads = files
                .iter()
                .enumerate()
                .skip(2)
                .map(|(index, file)| {
                    let bytes = if failure == Some("extra") && index == last {
                        b"corrupt extra APK".to_vec()
                    } else {
                        before[index].clone()
                    };
                    (
                        format!(
                            "{}/x86_64/{}",
                            repositories[index - 2],
                            file.file_name().unwrap().to_str().unwrap()
                        ),
                        bytes,
                    )
                })
                .collect();
            Some(serve_files(payloads))
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
        if let Some((url, _)) = &extra_response {
            source = source.replace(
                "extra_apks_url: assets.extra_apks_url,",
                &format!("extra_apks_url: {url:?},"),
            );
        }
        std::fs::write(&path, &source).unwrap();
        let mut command = preseed_command(&recipes, recipe, "install", &target);
        command.args(["--force-acquire", "--force-install"]);
        for url in std::iter::once(&iso_url)
            .chain(apk_response.as_ref().map(|(url, _)| url))
            .chain(extra_response.as_ref().map(|(url, _)| url))
        {
            let authority = url.trim_start_matches("http://").split('/').next().unwrap();
            command.args(["--network-host", authority]);
        }
        let (output, _) = super::kernel_recipes::invoke_kernel(command, dir.path());
        iso_server.join().unwrap();
        if let Some((_, server)) = apk_response {
            server.join().unwrap();
        }
        if let Some((_, server)) = extra_response {
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
        for (index, file) in files.iter().enumerate().skip(2) {
            assert_eq!(
                std::fs::read(prior_extras.join(file.file_name().unwrap())).unwrap(),
                before[index]
            );
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }
}

#[test]
fn cli_rootfs_consumers_reject_changed_explicit_assets() {
    for (producer, consumer) in [
        ("fedora-rpm-preseed", "fedora-rpm-source-rootfs"),
        ("alpine-preseed-source-assets", "alpine-live-source-rootfs"),
    ] {
        let (dir, recipes) = create_test_env();
        let (_target, files) = prepare_assets(&recipes, producer);
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../distro/builder/recipes");
        std::fs::copy(
            source.join("installed-system.rhai"),
            recipes.join("installed-system.rhai"),
        )
        .unwrap();
        std::fs::copy(
            source.join(format!("{consumer}.rhai")),
            recipes.join(format!("{consumer}.rhai")),
        )
        .unwrap();
        let fedora = consumer.starts_with("fedora");
        let sampled = if fedora {
            vec![
                files.first().unwrap().clone(),
                files.last().unwrap().clone(),
            ]
        } else {
            files.clone()
        };
        for file in &sampled {
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
                .arg(dir.path().join("materializer"));
            if fedora {
                command.args([
                    "--input",
                    &format!("rpms={}", files[0].parent().unwrap().display()),
                ]);
            } else {
                command.args(["--input", &format!("iso={}", files[0].display())]);
            }
            if !fedora {
                command.args(["--input", &format!("apk_tools={}", files[1].display())]);
                command.args([
                    "--input",
                    &format!("extra_apks={}", files[2].parent().unwrap().display()),
                ]);
            }
            let (output, _) = super::kernel_recipes::invoke_kernel(command, dir.path());
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("integrity check failed"));
            std::fs::write(file, original).unwrap();
        }
    }
}
