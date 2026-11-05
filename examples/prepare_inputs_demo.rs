//! R05 real preparation run: stage a genuine recipe, show every artifact,
//! then exercise one over-limit and two unsupported-discovery failures plus
//! the `GinuxLauncher::run` ginux arm.
//!
//! Run with:
//!
//! ```sh
//! cargo run -p levitate-recipe --example prepare_inputs_demo
//! ```
//!
//! The demo writes fixture recipes only under the process temp dir; no home
//! directories are copied and no secrets are staged.

use std::path::PathBuf;

use levitate_recipe::GinuxLauncher;
use levitate_recipe::inputs::{ExtraInput, prepare_invocation};

fn main() -> anyhow::Result<()> {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let recipe = crate_dir.join("../../AcornOS/deps/recfstab.rhai");
    println!("== R05 real run: {} ==", recipe.display());

    // 1. Real preparation over the actual recfstab recipe.
    let prepared = prepare_invocation(&recipe, "install", None, &[], &[], &Default::default())?;
    println!("invocation_id: {}", prepared.invocation_id);
    println!("operation: {}", prepared.operation);
    println!("recipe_digest: sha256:{}", prepared.recipe_digest);
    println!("staging_dir: {}", prepared.staging_dir().display());
    println!("-- staged input set --");
    for input in &prepared.inputs {
        println!(
            "INPUT: path=/inputs/{} sha256={} size={} mode={:04o} <- {}",
            input.name,
            hex::encode(input.sha256),
            input.size,
            input.mode,
            input.referenced_as,
        );
    }
    println!("INPUTS: packaged={}", prepared.inputs.len());
    println!("-- allowlisted env --");
    for (name, value) in &prepared.env {
        println!("ENV: {name}={value}");
    }
    println!("-- domain config text --");
    print!("{}", prepared.config_text);
    println!(
        "-- config size: {} B (limit 4096) --",
        prepared.config_text.len()
    );

    // Leak check: domain artifacts must never name host paths.
    let staging = prepared.staging_dir().to_string_lossy().into_owned();
    let home = std::env::var("HOME").unwrap_or_default();
    let checkout = crate_dir
        .canonicalize()
        .unwrap_or(crate_dir.clone())
        .to_string_lossy()
        .into_owned();
    let haystacks = [&prepared.config_text];
    let mut leaked = false;
    for marker in [&staging, &home, &checkout] {
        if marker.len() < 2 {
            continue;
        }
        for hay in haystacks {
            if hay.contains(marker) {
                println!("LEAK: domain config contains host path {marker:?}");
                leaked = true;
            }
        }
        for (name, value) in &prepared.env {
            if value.contains(marker) {
                println!("LEAK: env {name:?} contains host path {marker:?}");
                leaked = true;
            }
        }
    }
    if leaked {
        anyhow::bail!("host path leaked into domain artifacts");
    }
    println!("no-leak: PASS (config+env name no host staging, home, or checkout path)");

    // 2. extends + import discovery over temp fixtures (proves non-vacuous).
    let fixture_dir = std::env::temp_dir().join(format!("r05demo-{}", std::process::id()));
    std::fs::create_dir_all(&fixture_dir)?;
    std::fs::write(fixture_dir.join("base.rhai"), "fn shared(ctx) { ctx }\n")?;
    std::fs::write(fixture_dir.join("helper.rhai"), "fn assist(ctx) { ctx }\n")?;
    std::fs::write(
        fixture_dir.join("child.rhai"),
        "//! extends: base.rhai\nimport \"helper.rhai\" as h;\nlet ctx = #{ name: \"child\" };\n",
    )?;
    let child = prepare_invocation(
        &fixture_dir.join("child.rhai"),
        "install",
        Some(&fixture_dir),
        &[],
        &[],
        &Default::default(),
    )?;
    println!("== discovery run: child.rhai ==");
    for input in &child.inputs {
        println!(
            "INPUT: path=/inputs/{} sha256={} size={} <- {}",
            input.name,
            hex::encode(input.sha256),
            input.size,
            input.referenced_as,
        );
    }
    let sources = std::fs::read_to_string(child.staging_dir().join("sources.txt"))?;
    print!("-- sources.txt --\n{sources}");

    // 3. Over-limit case: 7 fixed + 2 toolchain vars = 9 > 8 (§2).
    println!("== over-limit case: 9 env entries ==");
    let extra_env = vec![
        ("CC".to_string(), "gcc".to_string()),
        ("CXX".to_string(), "g++".to_string()),
    ];
    match prepare_invocation(
        &recipe,
        "install",
        None,
        &[],
        &extra_env,
        &Default::default(),
    ) {
        Ok(_) => anyhow::bail!("expected the 9-entry env to fail the §2 limit"),
        Err(err) => println!("REJECTED (expected): {err:#}"),
    }

    // 4. Unsupported discovery: absolute host path in `extends:`.
    println!("== unsupported case: absolute extends ==");
    std::fs::write(
        fixture_dir.join("abshost.rhai"),
        "//! extends: /etc/passwd\nlet ctx = #{ name: \"abshost\" };\n",
    )?;
    match prepare_invocation(
        &fixture_dir.join("abshost.rhai"),
        "install",
        None,
        &[],
        &[],
        &Default::default(),
    ) {
        Ok(_) => anyhow::bail!("expected the absolute extends to fail"),
        Err(err) => println!("REJECTED (expected): {err:#}"),
    }

    // 5. Unsupported discovery: missing import target.
    println!("== unsupported case: missing import ==");
    std::fs::write(
        fixture_dir.join("missing.rhai"),
        "import \"no-such-module.rhai\" as m;\nlet ctx = #{ name: \"missing\" };\n",
    )?;
    match prepare_invocation(
        &fixture_dir.join("missing.rhai"),
        "install",
        None,
        &[],
        &[],
        &Default::default(),
    ) {
        Ok(_) => anyhow::bail!("expected the missing import to fail"),
        Err(err) => println!("REJECTED (expected): {err:#}"),
    }

    // 6. `GinuxLauncher::run` ginux arm: preparation runs, then real launch is
    // attempted through the configured launcher. `/bin/true` speaks no
    // transport, so this fails explicitly without host fallback.
    println!("== GinuxLauncher::run ginux arm ==");
    let extra: Vec<ExtraInput> = Vec::new();
    let _ = extra;
    unsafe {
        std::env::set_var("RECIPE_BACKEND", "ginux");
        std::env::set_var("GINUX_BIN", "/bin/true");
        std::env::remove_var("RECIPE_WORKER");
    }
    let backend = GinuxLauncher::from_env()?;
    let err = backend
        .run(
            "install",
            &recipe,
            None,
            true,
            Default::default(),
            &[],
            &fixture_dir.join("destination"),
            Some(&fixture_dir.join("state")),
        )
        .err()
        .expect("ginux run without a speaking launcher must fail");
    println!("BACKEND-ERROR (expected): {err:#}");
    let text = format!("{err:#}");
    if !(text.contains("backend startup failure") || text.contains("backend protocol failure")) {
        anyhow::bail!("ginux arm error does not carry a backend failure category");
    }
    if text.contains("host fallback must never run") {
        anyhow::bail!("ginux arm ran the host fallback");
    }

    std::fs::remove_dir_all(&fixture_dir).ok();
    println!("R05-DEMO: PASS");
    Ok(())
}
