mod tests {
    use crate::helpers::build::extract::extract_api::{
        detect_format, extract_tar_gz, extract_with_format, extract_zip,
    };
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn test_detect_format() {
        assert_eq!(detect_format("foo.tar.gz"), Some("tar.gz"));
        assert_eq!(detect_format("foo.tgz"), Some("tar.gz"));
        assert_eq!(detect_format("foo.tar.xz"), Some("tar.xz"));
        assert_eq!(detect_format("foo.txz"), Some("tar.xz"));
        assert_eq!(detect_format("foo.tar.bz2"), Some("tar.bz2"));
        assert_eq!(detect_format("foo.tbz2"), Some("tar.bz2"));
        assert_eq!(detect_format("foo.tar.zst"), Some("tar.zst"));
        assert_eq!(detect_format("foo.tzst"), Some("tar.zst"));
        assert_eq!(detect_format("foo.zip"), Some("zip"));
        assert_eq!(detect_format("foo.tar"), Some("tar"));
        assert_eq!(detect_format("foo.apk"), Some("apk"));
        assert_eq!(detect_format("foo.unknown"), None);
    }

    #[test]
    fn test_extract_tar_gz() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("test.tar.gz");
        let extract_dir = temp_dir.path().join("extracted");

        // Create a simple tar.gz archive
        let file = File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        // Add a file to the archive using append_data which handles headers correctly
        let content = b"Hello, World!";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "test.txt", &content[..])
            .unwrap();

        // Properly finish and close the archive
        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        // Extract it
        std::fs::create_dir_all(&extract_dir).unwrap();
        extract_tar_gz(&archive_path, &extract_dir).unwrap();

        // Verify
        let extracted_file = extract_dir.join("test.txt");
        assert!(extracted_file.exists());
        assert_eq!(
            std::fs::read_to_string(extracted_file).unwrap(),
            "Hello, World!"
        );
    }

    #[test]
    fn test_extract_zip() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("test.zip");
        let extract_dir = temp_dir.path().join("extracted");

        // Create a simple zip archive
        let file = File::create(&archive_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("test.txt", options).unwrap();
        zip.write_all(b"Hello from zip!").unwrap();
        zip.finish().unwrap();

        // Extract it
        std::fs::create_dir_all(&extract_dir).unwrap();
        extract_zip(&archive_path, &extract_dir).unwrap();

        // Verify
        let extracted_file = extract_dir.join("test.txt");
        assert!(extracted_file.exists());
        assert_eq!(
            std::fs::read_to_string(extracted_file).unwrap(),
            "Hello from zip!"
        );
    }

    #[test]
    fn test_extract_tar_with_nested_dirs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("nested.tar.gz");
        let extract_dir = temp_dir.path().join("extracted");

        // Create a tar.gz with nested directory structure
        let file = File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        // Add files in nested directories
        let content = b"nested content";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "foo/bar/baz.txt", &content[..])
            .unwrap();

        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        // Extract
        std::fs::create_dir_all(&extract_dir).unwrap();
        extract_tar_gz(&archive_path, &extract_dir).unwrap();

        // Verify nested structure was created
        let extracted_file = extract_dir.join("foo/bar/baz.txt");
        assert!(extracted_file.exists());
        assert_eq!(
            std::fs::read_to_string(extracted_file).unwrap(),
            "nested content"
        );
    }

    #[test]
    fn test_extract_tar_blocks_symlink_escape() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("escape.tar.gz");
        let extract_dir = temp_dir.path().join("extracted");

        let file = File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        // Create a symlink "a" -> "/" then attempt to write "a/evil.txt".
        let mut link_header = tar::Header::new_gnu();
        link_header.set_entry_type(tar::EntryType::Symlink);
        link_header.set_size(0);
        link_header.set_mode(0o777);
        link_header.set_cksum();
        link_header.set_link_name("/").unwrap();
        builder
            .append_data(&mut link_header, "a", std::io::empty())
            .unwrap();

        let content = b"pwned";
        let mut file_header = tar::Header::new_gnu();
        file_header.set_size(content.len() as u64);
        file_header.set_mode(0o644);
        file_header.set_cksum();
        builder
            .append_data(&mut file_header, "a/evil.txt", &content[..])
            .unwrap();

        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        std::fs::create_dir_all(&extract_dir).unwrap();
        let err = extract_tar_gz(&archive_path, &extract_dir).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unsafe link target") || msg.contains("symlink"),
            "expected link/symlink safety error, got: {msg}"
        );
        assert!(!extract_dir.join("a/evil.txt").exists());
    }

    #[test]
    fn test_extract_tar_blocks_hardlink_outside_dest() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("hardlink.tar.gz");
        let extract_dir = temp_dir.path().join("extracted");

        let file = File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);

        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Link);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        header.set_link_name("/etc/passwd").unwrap();
        builder
            .append_data(&mut header, "hl", std::io::empty())
            .unwrap();

        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();

        std::fs::create_dir_all(&extract_dir).unwrap();
        let err = extract_tar_gz(&archive_path, &extract_dir).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unsafe link target"),
            "expected unsafe link target error, got: {msg}"
        );
    }

    #[test]
    fn test_extract_zip_with_nested_dirs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let archive_path = temp_dir.path().join("nested.zip");
        let extract_dir = temp_dir.path().join("extracted");

        // Create a zip with nested directory structure
        let file = File::create(&archive_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();

        // Add directory entry
        zip.add_directory("foo/bar/", options).unwrap();
        zip.start_file("foo/bar/baz.txt", options).unwrap();
        zip.write_all(b"nested zip content").unwrap();
        zip.finish().unwrap();

        // Extract
        std::fs::create_dir_all(&extract_dir).unwrap();
        extract_zip(&archive_path, &extract_dir).unwrap();

        // Verify
        let extracted_file = extract_dir.join("foo/bar/baz.txt");
        assert!(extracted_file.exists());
        assert_eq!(
            std::fs::read_to_string(extracted_file).unwrap(),
            "nested zip content"
        );
    }

    #[test]
    fn test_extract_apk_multistream_extracts_data_payload() {
        fn tar_gz_member(path: &str, content: &[u8]) -> Vec<u8> {
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            {
                let mut builder = tar::Builder::new(&mut gz);
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder
                    .append_data(&mut header, path, content)
                    .expect("append tar member");
                builder.finish().expect("finish tar builder");
            }
            gz.finish().expect("finish gzip encoder")
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let apk_path = temp_dir.path().join("test.apk");
        let extract_dir = temp_dir.path().join("extracted");

        // Simulate APK layout: signature stream, control stream, data stream.
        let sig_member = tar_gz_member(
            ".SIGN.RSA.alpine-devel@lists.alpinelinux.org-6165ee59.rsa.pub",
            b"sig",
        );
        let ctl_member = tar_gz_member(".PKGINFO", b"pkginfo");
        let data_member = tar_gz_member("sbin/apk.static", b"#!/bin/sh\necho apk\n");

        let mut apk_file = File::create(&apk_path).unwrap();
        apk_file.write_all(&sig_member).unwrap();
        apk_file.write_all(&ctl_member).unwrap();
        apk_file.write_all(&data_member).unwrap();
        apk_file.flush().unwrap();

        std::fs::create_dir_all(&extract_dir).unwrap();
        extract_with_format(
            apk_path.to_str().unwrap(),
            extract_dir.to_str().unwrap(),
            "apk",
        )
        .unwrap();

        assert!(extract_dir.join(".PKGINFO").exists());
        assert!(extract_dir.join("sbin/apk.static").exists());
    }
}
