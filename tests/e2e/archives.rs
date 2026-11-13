use super::*;
use std::io::Write;

fn write_zip(recipes: &Path, entries: &[(&str, &str)]) {
    let file = std::fs::File::create(recipes.join("payload.zip")).unwrap();
    let mut archive = zip::ZipWriter::new(file);
    for (name, content) in entries {
        archive
            .start_file(
                *name,
                zip::write::SimpleFileOptions::default().unix_permissions(0o755),
            )
            .unwrap();
        archive.write_all(content.as_bytes()).unwrap();
    }
    archive.finish().unwrap();
    write_extraction_recipe(recipes, "payload.zip");
}

fn write_extraction_recipe(recipes: &Path, archive_name: &str) {
    write_recipe(
        recipes,
        "extract-archive",
        &format!(
            r#"
const RECIPE_API = 2;
const NEEDS = #{{ inputs: #{{ "{archive_name}": "file" }}, outputs: #{{ extracted: "directory" }} }};
let ctx = #{{ name: "extract-archive", state: #{{installed: false}} }};
fn is_installed(ctx) {{ if ctx.state.installed {{ return ready(ctx); }} needs_work(ctx, "extract archive") }}
fn is_acquired(ctx) {{ ready(ctx) }}
fn is_built(ctx) {{ ready(ctx) }}
fn install(ctx) {{
    extract("/inputs/{archive_name}", join_path(RUNTIME.paths.build, "extract"));
    ctx.outputs.extracted = join_path(RUNTIME.paths.build, "extract");
    ctx.state.installed = true; ctx
}}
"#
        ),
    );
}

fn extract_command(recipes: &Path, archive_name: &str) -> Command {
    let mut command = recipe_command(recipes);
    command
        .args([
            "install",
            "extract-archive",
            "--no-persist-state",
            "--recipes-path",
        ])
        .arg(recipes)
        .arg("--input")
        .arg(format!(
            "{archive_name}={}",
            recipes.join(archive_name).display()
        ));
    command
}

fn extracted_root(output: &std::process::Output) -> std::path::PathBuf {
    assert_success(output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    context["outputs"]["extracted"].as_str().unwrap().into()
}

#[cfg(unix)]
#[test]
fn cli_tar_extracts_payload_with_contained_symlink() {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, recipes) = create_test_env();
    let file = std::fs::File::create(recipes.join("payload.tar")).unwrap();
    let mut archive = tar::Builder::new(file);
    let content = b"#!/bin/sh\nprintf 'tar payload\\n'\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    archive
        .append_data(&mut header, "bin/program", &content[..])
        .unwrap();
    let mut link = tar::Header::new_gnu();
    link.set_entry_type(tar::EntryType::Symlink);
    link.set_size(0);
    link.set_mode(0o777);
    archive
        .append_link(&mut link, "program", "bin/program")
        .unwrap();
    archive.finish().unwrap();
    write_extraction_recipe(&recipes, "payload.tar");

    let output = extract_command(&recipes, "payload.tar").output().unwrap();
    let payload = extracted_root(&output).join("program");
    assert_eq!(std::fs::read(&payload).unwrap(), content);
    assert!(
        std::fs::symlink_metadata(&payload)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::metadata(&payload).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn cli_tar_hardlink_targets_resolve_inside_the_destination() {
    let (_dir, recipes) = create_test_env();
    let file = std::fs::File::create(recipes.join("payload.tar")).unwrap();
    let mut archive = tar::Builder::new(file);
    let content = b"shared payload\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive
        .append_data(&mut header, "share/original", &content[..])
        .unwrap();
    let mut link = tar::Header::new_gnu();
    link.set_entry_type(tar::EntryType::Link);
    link.set_size(0);
    link.set_mode(0o644);
    archive
        .append_link(&mut link, "bin/alias", "share/original")
        .unwrap();
    archive.finish().unwrap();
    write_extraction_recipe(&recipes, "payload.tar");

    let output = extract_command(&recipes, "payload.tar").output().unwrap();
    let root = extracted_root(&output);
    assert_eq!(std::fs::read(root.join("bin/alias")).unwrap(), content);
    assert_eq!(std::fs::read(root.join("share/original")).unwrap(), content);
}

#[test]
fn cli_zip_extracts_payload_and_preserves_executable_mode() {
    let (_dir, recipes) = create_test_env();
    write_zip(
        &recipes,
        &[("bin/program", "#!/bin/sh\nprintf 'archive payload\\n'\n")],
    );

    let output = extract_command(&recipes, "payload.zip").output().unwrap();
    let payload = extracted_root(&output).join("bin/program");
    assert_eq!(
        std::fs::read_to_string(&payload).unwrap(),
        "#!/bin/sh\nprintf 'archive payload\\n'\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(payload).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
}

#[test]
fn cli_zip_rejects_unsafe_member_instead_of_silently_skipping_it() {
    let (dir, recipes) = create_test_env();
    write_zip(
        &recipes,
        &[("../outside.txt", "escaped"), ("accepted.txt", "partial")],
    );

    let output = extract_command(&recipes, "payload.zip").output().unwrap();
    assert!(
        !output.status.success(),
        "unsafe archive was reported successful"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsafe path"));
    assert!(output.stdout.is_empty());
    assert!(!dir.path().join("state/recipe/exports").exists());
}

#[cfg(unix)]
#[test]
fn cli_zip_rejects_existing_symlink_components() {
    for component in ["ancestor", "destination", "directory", "file"] {
        let (_dir, recipes) = create_test_env();
        let (prepare, destination, member) = match component {
            "ancestor" => (
                "ln(\"/work/build/outside\", \"/work/build/redirect\");",
                "/work/build/redirect/extract",
                "new/payload.txt",
            ),
            "destination" => (
                "ln(\"/work/build/outside\", \"/work/build/extract\");",
                "/work/build/extract",
                "payload.txt",
            ),
            "directory" => (
                "mkdir(\"/work/build/extract\"); ln(\"/work/build/outside\", \"/work/build/extract/redirect\");",
                "/work/build/extract",
                "redirect/payload.txt",
            ),
            "file" => (
                "mkdir(\"/work/build/extract\"); ln(\"/work/build/outside/payload.txt\", \"/work/build/extract/payload.txt\");",
                "/work/build/extract",
                "payload.txt",
            ),
            _ => unreachable!(),
        };
        write_zip(&recipes, &[(member, "overwritten")]);
        write_recipe(
            &recipes,
            "extract-archive",
            &format!(
                r#"
const RECIPE_API = 2;
const NEEDS = #{{ inputs: #{{ "payload.zip": "file" }} }};
let ctx = #{{name: "zip-boundary"}};
fn is_acquired(ctx) {{ ready(ctx) }}
fn is_built(ctx) {{ ready(ctx) }}
fn is_installed(ctx) {{ needs_work(ctx, "validate ZIP boundary") }}
fn install(ctx) {{
    mkdir("/work/build/outside");
    write_file("/work/build/outside/payload.txt", "preserve existing content");
    {prepare}
    extract("/inputs/payload.zip", "{destination}");
    throw "extraction through symlink unexpectedly succeeded";
}}
fn cleanup(ctx, reason) {{
    if read_file("/work/build/outside/payload.txt") != "preserve existing content" {{ throw "symlink target overwritten"; }}
    if is_dir("/work/build/outside/new") || is_dir("/work/build/outside/extract") {{ throw "symlink target received archive data"; }}
    log("ZIP_TARGET_PRESERVED"); ctx
}}
"#
            ),
        );
        let output = extract_command(&recipes, "payload.zip").output().unwrap();
        assert!(
            !output.status.success(),
            "extraction through {component} symlink succeeded"
        );
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostics.contains("symlink in path component"),
            "{diagnostics}"
        );
        assert!(
            diagnostics.contains("ZIP_TARGET_PRESERVED"),
            "{diagnostics}"
        );
        assert!(output.stdout.is_empty());
    }
}
