use super::kernel_recipes::{copy_kernel_recipes, kernel_command};
use super::*;
use sha2::{Digest, Sha256};

fn prebuilt_archive(fault: &str) -> Vec<u8> {
    let encoder = xz2::write::XzEncoder::new(Vec::new(), 1);
    let mut archive = tar::Builder::new(encoder);
    // These are disposable artifact trees, not bootable kernel images/modules.
    let mut files = vec![
        ("kernel-build/include/config/kernel.release", "7.3.3-acorn"),
        ("staging/boot/vmlinuz", "downloaded disposable image"),
    ];
    if fault != "layout" {
        files.push((
            "staging/usr/lib/modules/7.3.3-acorn/payload.ko",
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
fn cli_prebuilt_kernel_offline_acquisition_validates_before_publication() {
    for fault in ["valid", "digest", "truncated", "layout", "unsafe"] {
        let (dir, recipes) = create_test_env();
        copy_kernel_recipes(&recipes);
        let previous = dir.path().join("previous target");
        let image = previous.join("staging/boot/vmlinuz");
        std::fs::create_dir_all(image.parent().unwrap()).unwrap();
        std::fs::write(&image, "previous image").unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("preserve"), "unrelated payload").unwrap();
        let archive = prebuilt_archive(fault);
        let expected = if fault == "digest" {
            "0".repeat(64)
        } else {
            hex::encode(Sha256::digest(&archive))
        };
        let bytes = if fault == "truncated" {
            &archive[..archive.len() - 16]
        } else {
            &archive
        };
        let supplied = dir.path().join("kernel.tar.xz");
        std::fs::write(&supplied, bytes).unwrap();
        let path = recipes.join("linux-prebuilt.rhai");
        // Only the fixture's declared digest changes; actual lifecycle runs.
        let source = std::fs::read_to_string(&path).unwrap().replace(
            "c3634752d628695eba64130b0b342042a283b164d6fa16e0c871c125256aabfc",
            &expected,
        );
        std::fs::write(&path, &source).unwrap();
        let mut command = kernel_command(&recipes, "install", "linux-prebuilt");
        command
            .args(["--force-acquire", "--force-install"])
            .args(["--input", &format!("kernel_archive={}", supplied.display())])
            .args(["--input", &format!("target={}", previous.display())]);
        let (output, ctx) = super::kernel_recipes::invoke_kernel(command, dir.path());
        if fault == "valid" {
            assert_success(&output);
            let ctx = ctx.unwrap();
            assert_eq!(ctx["kernel_version"], "7.3.3-acorn");
            let published = Path::new(ctx["outputs"]["kernel"].as_str().unwrap());
            assert_eq!(
                std::fs::read_to_string(published).unwrap(),
                "downloaded disposable image"
            );
            assert!(ctx["outputs"].get("source").is_none());
        } else {
            assert!(!output.status.success(), "accepted invalid payload {fault}");
            assert!(ctx.is_none(), "failed acquisition published a result");
        }
        assert_eq!(std::fs::read_to_string(&image).unwrap(), "previous image");
        assert_eq!(
            std::fs::read_to_string(outside.join("preserve")).unwrap(),
            "unrelated payload"
        );
        assert_eq!(std::fs::read(&supplied).unwrap(), bytes);
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    }
}
