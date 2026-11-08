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

fn output_name(recipe: &str) -> &'static str {
    if recipe.starts_with("alpine") {
        "apks"
    } else {
        "rpms"
    }
}
fn facts_name(recipe: &str) -> &'static str {
    if recipe.starts_with("alpine") {
        "acorn-apk-lock.rhai"
    } else {
        "levitate-rpm-lock.rhai"
    }
}

/// Read only the trusted lock's pure facts. Tiny fixture bytes replace their
/// hashes, preserving the actual producer lifecycle and full locked file set.
/// These bytes are integrity fixtures, never usable RPM/APK or ISO products.
fn lock_rows(recipes: &Path, recipe: &str) -> Vec<Vec<String>> {
    let engine = rhai::Engine::new();
    let ast = engine
        .compile_file(recipes.join("sources").join(facts_name(recipe)))
        .unwrap();
    let mut rows: rhai::Array = engine
        .call_fn(&mut rhai::Scope::new(), &ast, "packages", ())
        .unwrap();
    if recipe.starts_with("alpine") {
        let tools: rhai::Array = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "apk_tools", ())
            .unwrap();
        rows.push(tools.into());
    }
    rows.into_iter()
        .map(|row| {
            row.cast::<rhai::Array>()
                .into_iter()
                .map(|value| value.into_string().unwrap())
                .collect()
        })
        .collect()
}

fn prepare_assets(recipes: &Path, recipe: &str) -> (PathBuf, Vec<PathBuf>) {
    let source = production_recipes();
    std::fs::create_dir_all(recipes.join("sources")).unwrap();
    for file in [
        "locked-preseed.rhai",
        "rootfs-producer.rhai",
        "installed-system.rhai",
    ] {
        std::fs::copy(source.join(file), recipes.join(file)).unwrap();
    }
    std::fs::copy(
        source.join("sources").join(facts_name(recipe)),
        recipes.join("sources").join(facts_name(recipe)),
    )
    .unwrap();
    let recipe_path = recipes.join(format!("{recipe}.rhai"));
    let text = std::fs::read_to_string(source.join(format!("{recipe}.rhai"))).unwrap();
    let needs =
        levitate_recipe::needs::declaration(&source.join(format!("{recipe}.rhai")), Some(&source))
            .unwrap()
            .needs;
    let text = text.replace(&format!("network: {:?}", needs.network), "network: []");
    std::fs::write(recipe_path, text).unwrap();
    let target = recipes.parent().unwrap().join("verified assets");
    let directory = target.join(output_name(recipe));
    std::fs::create_dir_all(&directory).unwrap();
    let facts_path = recipes.join("sources").join(facts_name(recipe));
    let mut facts = std::fs::read_to_string(&facts_path).unwrap();
    let mut files = Vec::new();
    for row in lock_rows(recipes, recipe) {
        let name = row[2].rsplit('/').next().unwrap();
        let bytes = format!("verified fixture for {name}");
        let file = directory.join(name);
        std::fs::write(&file, &bytes).unwrap();
        facts = facts.replace(&row[3], &hex::encode(Sha256::digest(bytes.as_bytes())));
        files.push(file);
    }
    std::fs::write(facts_path, facts).unwrap();
    (target, files)
}

fn sample(files: &[PathBuf]) -> Vec<PathBuf> {
    let mut indexes = vec![0, files.len() / 2, files.len() - 1];
    indexes.sort_unstable();
    indexes.dedup();
    indexes
        .into_iter()
        .map(|index| files[index].clone())
        .collect()
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
            let published = PathBuf::from(ctx["outputs"][output_name(recipe)].as_str().unwrap());
            for file in &files {
                assert_eq!(
                    std::fs::read(published.join(file.file_name().unwrap())).unwrap(),
                    std::fs::read(file).unwrap()
                );
                assert_ne!(published.join(file.file_name().unwrap()), *file);
            }
        }
        for file in sample(&files) {
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
    use super::downloads::serve_files;
    for fault in [None, Some(0), Some(1), Some(2)] {
        let (dir, recipes) = create_test_env();
        let recipe = "alpine-preseed-source-assets";
        let (target, files) = prepare_assets(&recipes, recipe);
        let before = files
            .iter()
            .map(|file| std::fs::read(file).unwrap())
            .collect::<Vec<_>>();
        let (output, initial) = super::kernel_recipes::invoke_kernel(
            preseed_command(&recipes, recipe, "isinstalled", &target),
            dir.path(),
        );
        assert_success(&output);
        let initial = initial.unwrap();
        let previous = PathBuf::from(initial["outputs"]["apks"].as_str().unwrap());
        let indexes = [0, files.len() / 2, files.len() - 1];
        let corrupt = fault.map(|index| indexes[index]);
        let payloads = files
            .iter()
            .enumerate()
            .take(corrupt.map_or(files.len(), |index| index + 1))
            .map(|(index, file)| {
                (
                    file.file_name().unwrap().to_str().unwrap().to_owned(),
                    if Some(index) == corrupt {
                        b"corrupt APK".to_vec()
                    } else {
                        before[index].clone()
                    },
                )
            })
            .collect();
        let (origin, server) = serve_files(payloads);
        let facts_path = recipes.join("sources").join(facts_name(recipe));
        let mut facts = std::fs::read_to_string(&facts_path).unwrap();
        for row in lock_rows(&recipes, recipe) {
            facts = facts.replace(
                &row[2],
                &format!("{origin}{}", row[2].rsplit('/').next().unwrap()),
            );
        }
        std::fs::write(facts_path, facts).unwrap();
        let endpoint = origin
            .strip_prefix("http://")
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        let recipe_path = recipes.join(format!("{recipe}.rhai"));
        let source = std::fs::read_to_string(&recipe_path)
            .unwrap()
            .replace("network: []", &format!("network: [{endpoint:?}]"));
        std::fs::write(&recipe_path, &source).unwrap();
        let (output, _) = super::kernel_recipes::invoke_kernel(
            {
                let mut command = preseed_command(&recipes, recipe, "install", &target);
                command.args(["--force-acquire", "--force-install"]);
                command
            },
            dir.path(),
        );
        server.join().unwrap();
        if fault.is_some() {
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("integrity check failed"));
        } else {
            assert_success(&output);
        }
        for (file, bytes) in files.iter().zip(&before) {
            assert_eq!(&std::fs::read(file).unwrap(), bytes);
            assert_eq!(
                &std::fs::read(previous.join(file.file_name().unwrap())).unwrap(),
                bytes
            );
        }
        assert_eq!(std::fs::read_to_string(recipe_path).unwrap(), source);
    }
}

#[test]
fn cli_rootfs_consumers_reject_changed_explicit_assets() {
    for (producer, consumer) in [
        ("fedora-rpm-preseed", "fedora-rpm-source-rootfs"),
        ("alpine-preseed-source-assets", "alpine-live-source-rootfs"),
    ] {
        let (dir, recipes) = create_test_env();
        let (target, files) = prepare_assets(&recipes, producer);
        let source = production_recipes();
        std::fs::copy(
            source.join(format!("{consumer}.rhai")),
            recipes.join(format!("{consumer}.rhai")),
        )
        .unwrap();
        let control = dir.path().join("control");
        std::fs::write(&control, "explicit check input").unwrap();
        for file in sample(&files) {
            let original = std::fs::read(&file).unwrap();
            let mut changed = original.clone();
            changed[0] ^= 1;
            std::fs::write(&file, changed).unwrap();
            let mut command = recipe_command(&recipes);
            command
                .args(["isacquired", consumer, "--no-persist-state"])
                .arg("--recipes-path")
                .arg(&recipes)
                .arg("--build-dir")
                .arg(dir.path().join("materializer"))
                .args([
                    "--input",
                    &format!(
                        "{}={}",
                        output_name(producer),
                        target.join(output_name(producer)).display()
                    ),
                ]);
            for name in [
                "capabilities",
                "system_setup",
                "os_release",
                "admin_rule_file",
                "admin_rule",
            ] {
                command.args(["--input", &format!("{name}={}", control.display())]);
            }
            let (output, _) = super::kernel_recipes::invoke_kernel(command, dir.path());
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("integrity check failed"),
                "{consumer}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            std::fs::write(file, original).unwrap();
        }
    }
}
