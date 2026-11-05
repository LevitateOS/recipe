use super::downloads::serve_download;
use super::kernel_recipes::{copy_kernel_recipes, kernel_command};
use super::*;
use sha2::{Digest, Sha256};

fn prebuilt_archive(fault: &str) -> Vec<u8> {
    let encoder = xz2::write::XzEncoder::new(Vec::new(), 1);
    let mut archive = tar::Builder::new(encoder);
    // These are disposable artifact trees, not bootable kernel images/modules.
    let mut files = vec![
        ("kernel-build/include/config/kernel.release", "6.19.0-acorn"),
        ("staging/boot/vmlinuz", "downloaded disposable image"),
    ];
    if fault != "layout" {
        files.push((
            "staging/usr/lib/modules/6.19.0-acorn/payload.ko",
            "disposable module payload",
        ));
    }
    for (path, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, path, bytes.as_bytes())
            .unwrap();
    }
    if fault == "unsafe" {
        let mut header = tar::Header::new_gnu();
        header.set_path("staging/escape").unwrap();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_link_name("../../outside").unwrap();
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        archive.append(&header, std::io::empty()).unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap()
}

#[test]
fn cli_prebuilt_kernel_acquisition_validates_download_and_layout_before_publication() {
    for fault in ["valid", "digest", "truncated", "layout", "unsafe"] {
        let (dir, recipes) = create_test_env();
        copy_kernel_recipes(&recipes);
        let root = dir.path().join("artifacts/kernel 'quoted'");
        let image = root.join("staging/boot/vmlinuz");
        let release = root.join("kernel-build/include/config/kernel.release");
        std::fs::create_dir_all(image.parent().unwrap()).unwrap();
        std::fs::create_dir_all(release.parent().unwrap()).unwrap();
        std::fs::write(&image, "previous image").unwrap();
        std::fs::write(&release, "previous release").unwrap();
        let outside = dir.path().join("artifacts/outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("preserve"), "unrelated payload").unwrap();
        let downloads = dir.path().join("acorn/downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let cache = downloads.join("acorn-6.19.0-acorn-runtime.v2.tar.xz");
        std::fs::write(&cache, "previous cache bytes").unwrap();
        std::fs::write(downloads.join("unrelated"), "preserve").unwrap();

        let archive = prebuilt_archive(fault);
        let expected = if fault == "digest" {
            "0".repeat(64)
        } else {
            hex::encode(Sha256::digest(&archive))
        };
        let sent = if fault == "truncated" {
            &archive[..archive.len() - 16]
        } else {
            &archive
        };
        let (url, server) = serve_download(sent, archive.len());
        let path = recipes.join("linux-prebuilt.rhai");
        // Only fixture URL/digest facts change; the actual lifecycle is untouched.
        let source = std::fs::read_to_string(&path)
            .unwrap()
            .replace("https://github.com/", &format!("{url}/"))
            .replace(
                "04e10c6392680b75b80a09beb9036e5954b62cfabddd4cb0fe3c975c0a79e96e",
                &expected,
            );
        std::fs::write(&path, &source).unwrap();
        let define = format!("KERNEL_ARTIFACT_ROOT={}", root.display());
        let output = kernel_command(&recipes, "install", "linux-prebuilt")
            .args(["--define", &define, "--define", "KERNEL_FORCE_REBUILD=1"])
            .output()
            .unwrap();
        let request = server.join().unwrap();
        assert!(request.contains("acorn-6.19.0-acorn-runtime.v2.tar.xz"));
        if fault == "valid" {
            assert_success(&output);
            let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(ctx["kernel_version"], "6.19.0-acorn");
            assert_eq!(
                std::fs::read_to_string(&image).unwrap(),
                "downloaded disposable image"
            );
            assert_eq!(std::fs::read_to_string(&release).unwrap(), "6.19.0-acorn");
            assert!(
                !cache.exists(),
                "successful acquire left its temporary archive"
            );
            for action in ["isacquired", "isbuilt", "isinstalled"] {
                let output = kernel_command(&recipes, action, "linux-prebuilt")
                    .args(["--define", &define])
                    .env_remove("KERNEL_FORCE_REBUILD")
                    .output()
                    .unwrap();
                assert_success(&output);
            }
        } else {
            assert!(
                !output.status.success(),
                "accepted invalid prebuilt payload: {fault}"
            );
            assert!(output.stdout.is_empty());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains(match fault {
                    "digest" => "SHA256 integrity check failed",
                    "truncated" => "read error",
                    "layout" => "Missing staged kernel modules",
                    "unsafe" => "unsafe link target",
                    _ => unreachable!(),
                }),
                "{stderr}"
            );
            assert_eq!(std::fs::read_to_string(&image).unwrap(), "previous image");
            assert_eq!(
                std::fs::read_to_string(&release).unwrap(),
                "previous release"
            );
            if fault == "digest" || fault == "truncated" {
                assert_eq!(
                    std::fs::read_to_string(&cache).unwrap(),
                    "previous cache bytes"
                );
            }
        }
        assert!(!root.with_file_name("kernel 'quoted'.prebuilt.tmp").exists());
        assert_eq!(
            std::fs::read_to_string(outside.join("preserve")).unwrap(),
            "unrelated payload"
        );
        assert_eq!(
            std::fs::read_to_string(downloads.join("unrelated")).unwrap(),
            "preserve"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    }
}
