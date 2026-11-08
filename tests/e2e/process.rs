use super::*;

#[test]
fn cli_exec_streams_diagnostics_to_stderr_and_preserves_status_result() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "exec-status",
        r#"
const RECIPE_API = 2;
const NEEDS = #{ outputs: #{ work: "directory" } };
let ctx = #{ name: "exec-status", state: #{installed: false} };
fn is_installed(ctx) { if ctx.state.installed { return ready(ctx); } needs_work(ctx, "execute child") }
fn is_acquired(ctx) { ready(ctx) }
fn is_built(ctx) { ready(ctx) }
fn install(ctx) {
    ctx.status = exec("sh", ["-c", "printf 'child stdout\\n'; printf 'child stderr\\n' >&2; exit 23"]);
    if ctx.status != 23 { throw "unexpected child status"; }
    write_file(join_path(RUNTIME.paths.build, "handled-status"), "child failure was handled explicitly");
    ctx.outputs.work = RUNTIME.paths.build;
    ctx.state.installed = true; ctx
}


"#,
    );

    let output = run_recipe(&["install", "exec-status", "--no-persist-state"], &recipes);
    assert_success(&output);
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(ctx["status"], 23);
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("child stdout"));
    assert!(diagnostics.contains("child stderr"));
    assert_eq!(
        std::fs::read_to_string(
            Path::new(ctx["outputs"]["work"].as_str().unwrap()).join("handled-status")
        )
        .unwrap(),
        "child failure was handled explicitly"
    );
}

#[test]
fn cli_large_asset_hash_progress_does_not_corrupt_json() {
    use sha2::Digest;

    let (_dir, recipes) = create_test_env();
    let chunks = 101;
    std::fs::File::create(recipes.join("large.bin"))
        .unwrap()
        .set_len(chunks * 1024 * 1024)
        .unwrap();
    let zeros = vec![0u8; 1024 * 1024];
    let mut sha256 = sha2::Sha256::new();
    let mut blake3 = blake3::Hasher::new();
    for _ in 0..chunks {
        sha256.update(&zeros);
        blake3.update(&zeros);
    }
    for (helper, digest) in [
        ("verify_sha256", format!("{:x}", sha256.finalize())),
        ("verify_blake3", blake3.finalize().to_hex().to_string()),
    ] {
        write_recipe(
            &recipes,
            "verify-large",
            &format!(
                r#"
const RECIPE_API = 2;
const NEEDS = #{{ inputs: #{{ "large.bin": "file" }} }};
let ctx = #{{ name: "verify-large", digest: "{digest}" }};
fn is_installed(ctx) {{
    {helper}("/inputs/large.bin", ctx.digest);
    ctx.verified = true;
    ready(ctx)
}}
"#
            ),
        );
        let output = recipe_command(&recipes)
            .args(["isinstalled", "verify-large", "--recipes-path"])
            .arg(&recipes)
            .arg("--input")
            .arg(format!("large.bin={}", recipes.join("large.bin").display()))
            .output()
            .unwrap();
        assert_success(&output);
        let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(ctx["verified"], true);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let algorithm = if helper == "verify_sha256" {
            "sha256"
        } else {
            "blake3"
        };
        assert!(
            stderr.contains(&format!("verifying {algorithm}")),
            "{stderr}"
        );
        let diagnostics = stderr
            .lines()
            .find_map(|line| line.strip_prefix("recipe: diagnostics: "))
            .expect("invocation exposes its retained diagnostic directory");
        let raw =
            std::fs::read_to_string(Path::new(diagnostics).join("supervisor-stderr.log")).unwrap();
        assert!(
            raw.contains("checksum:"),
            "checksum progress missing from retained diagnostics"
        );
    }
}
