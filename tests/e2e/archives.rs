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
        &r#"
let ctx = #{ name: "extract-archive" };
fn is_installed(ctx) { throw "extraction requested"; }
fn is_acquired(ctx) { ctx }
fn install(ctx) {
    extract(join_path(RECIPE_DIR, "payload.zip"), join_path(BUILD_DIR, "extract"));
    ctx
}
"#
        .replace("payload.zip", archive_name),
    );
}

#[cfg(unix)]
#[test]
fn cli_tar_extracts_payload_with_contained_symlink() {
    use std::os::unix::fs::PermissionsExt;

    let (dir, recipes) = create_test_env();
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

    let output = run_recipe(
        &["install", "extract-archive", "--no-persist-state"],
        &recipes,
    );
    assert_success(&output);
    let payload = dir.path().join("build/extract/program");
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
fn cli_zip_extracts_payload_and_preserves_executable_mode() {
    let (dir, recipes) = create_test_env();
    write_zip(
        &recipes,
        &[("bin/program", "#!/bin/sh\nprintf 'archive payload\\n'\n")],
    );

    let output = run_recipe(
        &["install", "extract-archive", "--no-persist-state"],
        &recipes,
    );
    assert_success(&output);
    let payload = dir.path().join("build/extract/bin/program");
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

    let output = run_recipe(
        &["install", "extract-archive", "--no-persist-state"],
        &recipes,
    );
    assert!(
        !output.status.success(),
        "unsafe archive was reported successful"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsafe path"));
    assert!(!dir.path().join("build/outside.txt").exists());
    assert!(!dir.path().join("build/extract/accepted.txt").exists());
}

#[cfg(unix)]
#[test]
fn cli_zip_rejects_existing_symlink_components() {
    use std::os::unix::fs::symlink;

    for component in ["ancestor", "destination", "directory", "file"] {
        let (dir, recipes) = create_test_env();
        let root = dir.path();
        let outside = root.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("payload.txt"), "preserve existing content").unwrap();
        let build = root.join("build");
        let dest = build.join("extract");
        let member = match component {
            "ancestor" => {
                symlink(&outside, &build).unwrap();
                "new/payload.txt"
            }
            "destination" => {
                std::fs::create_dir(&build).unwrap();
                symlink(&outside, &dest).unwrap();
                "payload.txt"
            }
            "directory" => {
                std::fs::create_dir_all(&dest).unwrap();
                symlink(&outside, dest.join("redirect")).unwrap();
                "redirect/payload.txt"
            }
            "file" => {
                std::fs::create_dir_all(&dest).unwrap();
                symlink(outside.join("payload.txt"), dest.join("payload.txt")).unwrap();
                "payload.txt"
            }
            _ => unreachable!(),
        };
        write_zip(&recipes, &[(member, "overwritten")]);

        let output = run_recipe(
            &["install", "extract-archive", "--no-persist-state"],
            &recipes,
        );
        assert!(
            !output.status.success(),
            "extraction through {component} symlink succeeded: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("symlink"));
        assert_eq!(
            std::fs::read_to_string(outside.join("payload.txt")).unwrap(),
            "preserve existing content"
        );
        assert!(!outside.join("new").exists());
        assert!(!outside.join("extract").exists());
    }
}
