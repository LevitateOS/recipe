use super::*;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// `store`: the builder's content-addressed store as the recipe's optional
/// `store` input (files named by their lowercase SHA-256).
fn preseed_command(recipes: &Path, recipe: &str, action: &str, store: Option<&Path>) -> Command {
    let mut command = recipe_command(recipes);
    command
        .args([action, recipe, "--no-persist-state"])
        .arg("--recipes-path")
        .arg(recipes)
        .arg("--build-dir")
        .arg(recipes.parent().unwrap().join("work"));
    if let Some(store) = store {
        command
            .arg("--input")
            .arg(format!("store={}", store.display()));
    }
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

/// The locked file set of `recipe` as tiny fixture bytes in a store folder,
/// and the facts module rewritten to their digests. Returns the store, and
/// each file's published name, store file and bytes.
fn prepare_assets(recipes: &Path, recipe: &str) -> (PathBuf, Vec<(String, PathBuf, Vec<u8>)>) {
    let source = production_recipes();
    std::fs::create_dir_all(recipes.join("sources")).unwrap();
    std::fs::copy(
        source.join("locked-preseed.rhai"),
        recipes.join("locked-preseed.rhai"),
    )
    .unwrap();
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
    let store = recipes.parent().unwrap().join("verified store");
    std::fs::create_dir_all(&store).unwrap();
    let facts_path = recipes.join("sources").join(facts_name(recipe));
    let mut facts = std::fs::read_to_string(&facts_path).unwrap();
    let mut files = Vec::new();
    for row in lock_rows(recipes, recipe) {
        let name = row[2].rsplit('/').next().unwrap().to_owned();
        let bytes = format!("verified fixture for {name}").into_bytes();
        let digest = hex::encode(Sha256::digest(&bytes));
        let file = store.join(&digest);
        std::fs::write(&file, &bytes).unwrap();
        facts = facts.replace(&row[3], &digest);
        files.push((name, file, bytes));
    }
    std::fs::write(facts_path, facts).unwrap();
    (store, files)
}

fn sample<T: Clone>(files: &[T]) -> Vec<T> {
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
        let (store, files) = prepare_assets(&recipes, recipe);
        let (output, result) = super::kernel_recipes::invoke_kernel(
            preseed_command(&recipes, recipe, "install", Some(&store)),
            dir.path(),
        );
        assert_success(&output);
        let result = result.unwrap();
        let published = PathBuf::from(result["outputs"][output_name(recipe)].as_str().unwrap());
        for (name, file, bytes) in &files {
            assert_eq!(&std::fs::read(published.join(name)).unwrap(), bytes);
            assert_ne!(published.join(name), *file);
        }
        for (_, file, original) in sample(&files) {
            let mut changed = original.clone();
            changed[0] ^= 1;
            std::fs::write(&file, changed).unwrap();
            let (output, _) = super::kernel_recipes::invoke_kernel(
                preseed_command(&recipes, recipe, "install", Some(&store)),
                dir.path(),
            );
            assert!(!output.status.success(), "{recipe} accepted changed asset");
            assert!(String::from_utf8_lossy(&output.stderr).contains("integrity check failed"));
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
        let (store, _) = prepare_assets(&recipes, recipe);
        let (output, _) = super::kernel_recipes::invoke_kernel(
            preseed_command(&recipes, recipe, "install", Some(&store)),
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
        let (store, files) = prepare_assets(&recipes, recipe);
        let (output, initial) = super::kernel_recipes::invoke_kernel(
            preseed_command(&recipes, recipe, "install", Some(&store)),
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
            .map(|(index, (name, _, bytes))| {
                (
                    name.clone(),
                    if Some(index) == corrupt {
                        b"corrupt APK".to_vec()
                    } else {
                        bytes.clone()
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
        // Without the store, every file comes over the network.
        let (output, refreshed) = super::kernel_recipes::invoke_kernel(
            preseed_command(&recipes, recipe, "install", None),
            dir.path(),
        );
        server.join().unwrap();
        if fault.is_some() {
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("integrity check failed"));
            assert!(refreshed.is_none());
        } else {
            assert_success(&output);
            let published = PathBuf::from(refreshed.unwrap()["outputs"]["apks"].as_str().unwrap());
            for (name, _, bytes) in &files {
                assert_eq!(&std::fs::read(published.join(name)).unwrap(), bytes);
            }
        }
        for (name, file, bytes) in &files {
            assert_eq!(&std::fs::read(file).unwrap(), bytes);
            if fault.is_some() {
                assert_eq!(&std::fs::read(previous.join(name)).unwrap(), bytes);
            }
        }
        assert_eq!(std::fs::read_to_string(recipe_path).unwrap(), source);
    }
}
