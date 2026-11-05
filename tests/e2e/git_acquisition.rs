use super::*;

fn fixture_git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Recipe CLI fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert_success(&output);
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn upstream(root: &Path) -> (std::path::PathBuf, String, String) {
    let repo = root.join("upstream.git");
    std::fs::create_dir(&repo).unwrap();
    fixture_git(&repo, &["init", "--quiet"]);
    let first = commit_payload(&repo, "first source payload");
    let second = commit_payload(&repo, "second source payload");
    (repo, first, second)
}

fn commit_payload(repo: &Path, payload: &str) -> String {
    std::fs::write(repo.join("payload.txt"), payload).unwrap();
    fixture_git(repo, &["add", "payload.txt"]);
    fixture_git(repo, &["commit", "--quiet", "-m", payload]);
    fixture_git(repo, &["rev-parse", "HEAD"])
}

fn git_recipe(recipes: &Path, call: &str) {
    write_recipe(
        recipes,
        "git-payload",
        &format!(
            r#"
let ctx = #{{ name: "git-payload" }};
fn is_installed(ctx) {{ throw "run requested"; }}
fn is_acquired(ctx) {{ throw "acquisition requested"; }}
fn acquire(ctx) {{ ctx.source = {call}; ctx }}
fn install(ctx) {{
    mkdir(BUILD_DIR);
    copy_file(join_path(ctx.source, "payload.txt"), join_path(BUILD_DIR, "installed.txt"));
    ctx
}}
"#
        ),
    );
}

#[test]
fn cli_pinned_git_acquisition_selects_reuses_and_fetches_exact_commit() {
    let (dir, recipes) = create_test_env();
    let (source, first, second) = upstream(dir.path());
    let url = format!("file://{}", source.display());
    let checkout = recipes.join("exact checkout 'quoted'");
    let write_call = |commit: &str| {
        git_recipe(
            &recipes,
            &format!("git_checkout({url:?}, {checkout:?}, {commit:?})"),
        )
    };
    write_call(&first);
    let output = recipe_command(&recipes)
        .args(["install", "git-payload", "--no-persist-ctx"])
        .arg("--recipes-path")
        .arg(&recipes)
        .arg("--build-dir")
        .arg(dir.path().join("build"))
        .env("GIT_DIR", source.join(".git"))
        .env("GIT_WORK_TREE", &source)
        .env("GIT_INDEX_FILE", dir.path().join("caller-index"))
        .output()
        .unwrap();
    assert_success(&output);
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
    assert_eq!(fixture_git(&checkout, &["rev-parse", "HEAD"]), first);
    assert_eq!(fixture_git(&source, &["rev-parse", "HEAD"]), second);
    assert!(!dir.path().join("caller-index").exists());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("build/installed.txt")).unwrap(),
        "first source payload"
    );
    assert!(!checkout.join("upstream").exists());

    // A valid cached commit is reusable without contacting its origin.
    let offline = dir.path().join("temporarily offline.git");
    std::fs::rename(&source, &offline).unwrap();
    let output = run_recipe(&["install", "git-payload", "--no-persist-ctx"], &recipes);
    assert_success(&output);
    std::fs::rename(&offline, &source).unwrap();

    // The new commit did not exist when the checkout was cloned.
    let third = commit_payload(&source, "third fetched source payload");
    write_call(&third);
    let output = run_recipe(&["install", "git-payload", "--no-persist-ctx"], &recipes);
    assert_success(&output);
    assert_eq!(fixture_git(&checkout, &["rev-parse", "HEAD"]), third);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("build/installed.txt")).unwrap(),
        "third fetched source payload"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("fetch"));
}

#[test]
fn cli_git_acquisition_preserves_mismatched_dirty_and_invalid_destinations() {
    for failure in ["origin", "dirty", "invalid", "invalid-under-repo"] {
        let (dir, recipes) = create_test_env();
        let (source, first, second) = upstream(dir.path());
        let url = format!("file://{}", source.display());
        let checkout = if failure == "invalid-under-repo" {
            source.join("nested-checkout")
        } else {
            recipes.join("exact-checkout")
        };
        git_recipe(
            &recipes,
            &format!("git_checkout({url:?}, {checkout:?}, {first:?})"),
        );
        let output = run_recipe(&["install", "git-payload", "--no-persist-ctx"], &recipes);
        assert_success(&output);
        match failure {
            "origin" => {
                fixture_git(
                    &checkout,
                    &["config", "remote.origin.url", "file:///wrong-origin.git"],
                );
            }
            "dirty" => {
                std::fs::write(checkout.join("payload.txt"), "local edits to preserve").unwrap();
            }
            "invalid" | "invalid-under-repo" => {
                std::fs::remove_file(checkout.join(".git/HEAD")).unwrap();
            }
            _ => unreachable!(),
        }
        let before = std::fs::read(checkout.join("payload.txt")).unwrap();
        std::fs::write(
            dir.path().join("build/installed.txt"),
            "previous installation",
        )
        .unwrap();
        git_recipe(
            &recipes,
            &format!("git_checkout({url:?}, {checkout:?}, {second:?})"),
        );
        let output = run_recipe(&["install", "git-payload", "--no-persist-ctx"], &recipes);
        assert!(!output.status.success(), "accepted {failure} checkout");
        assert_eq!(std::fs::read(checkout.join("payload.txt")).unwrap(), before);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("build/installed.txt")).unwrap(),
            "previous installation"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(match failure {
                "origin" => "origin mismatch",
                "dirty" => "local changes",
                _ => "invalid Git checkout",
            }),
            "{stderr}"
        );
    }
}

#[test]
fn cli_legacy_git_clones_keep_parent_destination_and_check_origin() {
    for shallow in [false, true] {
        let (dir, recipes) = create_test_env();
        let (source, _, second) = upstream(dir.path());
        let url = format!("file://{}", source.display());
        let parent = recipes.join("source cache");
        let checkout = parent.join("upstream");
        git_recipe(
            &recipes,
            &if shallow {
                format!("git_clone_depth({url:?}, {parent:?}, 1)")
            } else {
                format!("git_clone({url:?}, {parent:?})")
            },
        );
        let output = run_recipe(&["install", "git-payload", "--no-persist-ctx"], &recipes);
        assert_success(&output);
        assert_eq!(fixture_git(&checkout, &["rev-parse", "HEAD"]), second);
        assert_eq!(checkout.join(".git/shallow").exists(), shallow);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("build/installed.txt")).unwrap(),
            "second source payload"
        );
        std::fs::write(checkout.join("payload.txt"), "local legacy payload").unwrap();
        let output = run_recipe(&["install", "git-payload", "--no-persist-ctx"], &recipes);
        assert_success(&output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("build/installed.txt")).unwrap(),
            "local legacy payload"
        );
        fixture_git(
            &checkout,
            &["config", "remote.origin.url", "file:///other-origin.git"],
        );
        let output = run_recipe(&["install", "git-payload", "--no-persist-ctx"], &recipes);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("origin mismatch"));
        assert_eq!(fixture_git(&checkout, &["rev-parse", "HEAD"]), second);
        std::fs::remove_file(checkout.join(".git/HEAD")).unwrap();
        let output = run_recipe(&["install", "git-payload", "--no-persist-ctx"], &recipes);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid Git checkout"));
        assert_eq!(
            std::fs::read_to_string(checkout.join("payload.txt")).unwrap(),
            "local legacy payload"
        );
    }
}

#[test]
fn cli_failed_git_acquisition_leaves_no_published_checkout() {
    let (dir, recipes) = create_test_env();
    let (source, _, _) = upstream(dir.path());
    let url = format!("file://{}", source.display());
    let checkout = recipes.join("checkout");
    git_recipe(
        &recipes,
        &format!("git_checkout({url:?}, {checkout:?}, {:?})", "0".repeat(40)),
    );
    let output = run_recipe(&["install", "git-payload", "--no-persist-ctx"], &recipes);
    assert!(!output.status.success());
    assert!(!checkout.exists());
    assert!(!dir.path().join("build/installed.txt").exists());
    assert!(!std::fs::read_dir(&recipes).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".recipe-git.")
    }));
}
