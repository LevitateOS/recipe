use super::*;

#[test]
fn cli_exec_streams_diagnostics_to_stderr_and_preserves_status_result() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "exec-status",
        r#"
let ctx = #{ name: "exec-status" };
fn is_installed(ctx) { throw "run requested"; }
fn is_acquired(ctx) { ctx }
fn install(ctx) {
    ctx.status = exec("sh", ["-c", "printf 'child stdout\\n'; printf 'child stderr\\n' >&2; exit 23"]);
    if ctx.status != 23 { throw "unexpected child status"; }
    write_file(join_path(BUILD_DIR, "handled-status"), "child failure was handled explicitly");
    ctx
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
        std::fs::read_to_string(dir.path().join("build/handled-status")).unwrap(),
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
let ctx = #{{ name: "verify-large", digest: "{digest}" }};
fn is_installed(ctx) {{
    {helper}(join_path(RECIPE_DIR, "large.bin"), ctx.digest);
    ctx.verified = true;
    ctx
}}
"#
            ),
        );
        let output = run_recipe(&["isinstalled", "verify-large"], &recipes);
        assert_success(&output);
        let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(ctx["verified"], true);
        assert!(String::from_utf8_lossy(&output.stderr).contains("checksum:"));
    }
}
