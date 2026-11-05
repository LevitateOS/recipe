use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

pub(super) fn serve_download(
    body: &str,
    advertised_length: usize,
) -> (String, std::thread::JoinHandle<String>) {
    serve_response(
        "200 OK",
        &format!("Content-Length: {advertised_length}\r\n"),
        body,
    )
}

fn serve_response(
    status: &'static str,
    headers: &str,
    body: &str,
) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/payload", listener.local_addr().unwrap());
    let headers = headers.to_owned();
    let body = body.to_owned();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("download CLI did not connect to its HTTP fixture: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = BufReader::new(&mut stream);
        let mut line = String::new();
        let mut received = String::new();
        loop {
            line.clear();
            if request.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
            received.push_str(&line);
        }
        write!(
            stream,
            "HTTP/1.1 {status}\r\n{headers}Connection: close\r\n\r\n{body}"
        )
        .unwrap();
        received
    });
    (url, server)
}

fn write_resume_recipe(recipes: &Path, url: &str) {
    write_download_recipe(recipes, url);
    let path = recipes.join("download-payload.rhai");
    let source = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        source.replace("= download(", "= download_with_resume("),
    )
    .unwrap();
}

fn add_download_digest(recipes: &Path, expected: &str) {
    let path = recipes.join("download-payload.rhai");
    let source = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        source.replace(
            "join_path(RECIPE_DIR, \"payload.dat\"))",
            &format!("join_path(RECIPE_DIR, \"payload.dat\"), \"{expected}\")"),
        ),
    )
    .unwrap();
}

#[test]
fn cli_verified_download_publishes_only_matching_sha256() {
    use sha2::{Digest, Sha256};
    let expected = hex::encode(Sha256::digest(b"hello world"));
    for existing in [false, true] {
        for body in ["hello world", "wrong world"] {
            let (dir, recipes) = create_test_env();
            let payload = recipes.join("payload.dat");
            if existing {
                std::fs::write(&payload, "previous verified payload").unwrap();
            }
            let (url, server) = serve_download(body, body.len());
            write_download_recipe(&recipes, &url);
            add_download_digest(&recipes, &expected);
            let output = run_recipe(
                &["install", "download-payload", "--no-persist-ctx"],
                &recipes,
            );
            server.join().unwrap();
            if body == "hello world" {
                assert_success(&output);
                assert_eq!(std::fs::read(&payload).unwrap(), b"hello world");
                assert_eq!(
                    std::fs::read(dir.path().join("build/installed.dat")).unwrap(),
                    b"hello world"
                );
            } else {
                assert!(!output.status.success());
                assert!(
                    String::from_utf8_lossy(&output.stderr)
                        .contains("SHA256 integrity check failed")
                );
                if existing {
                    assert_eq!(
                        std::fs::read(&payload).unwrap(),
                        b"previous verified payload"
                    );
                } else {
                    assert!(!payload.exists());
                }
                assert!(!dir.path().join("build/installed.dat").exists());
                assert!(!std::fs::read_dir(&recipes).unwrap().any(|entry| {
                    entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".recipe-download.")
                }));
            }
        }
    }
}

#[test]
fn cli_verified_resume_checks_completed_bytes_and_matching_size_reuse() {
    use sha2::{Digest, Sha256};
    let expected = hex::encode(Sha256::digest(b"hello world"));
    for complete in [false, true] {
        for valid in [false, true] {
            let (dir, recipes) = create_test_env();
            let payload = recipes.join("payload.dat");
            let prefix = if complete {
                if valid { "hello world" } else { "wrong world" }
            } else {
                "hello "
            };
            std::fs::write(&payload, prefix).unwrap();
            let (url, server) = if complete {
                serve_response(
                    "416 Range Not Satisfiable",
                    "Content-Range: bytes */11\r\nContent-Length: 0\r\n",
                    "",
                )
            } else {
                serve_response(
                    "206 Partial Content",
                    "Content-Range: bytes 6-10/11\r\nContent-Length: 5\r\n",
                    if valid { "world" } else { "other" },
                )
            };
            write_resume_recipe(&recipes, &url);
            add_download_digest(&recipes, &expected);
            let output = run_recipe(
                &["install", "download-payload", "--no-persist-ctx"],
                &recipes,
            );
            server.join().unwrap();
            if valid {
                assert_success(&output);
                assert_eq!(std::fs::read(&payload).unwrap(), b"hello world");
                assert_eq!(
                    std::fs::read(dir.path().join("build/installed.dat")).unwrap(),
                    b"hello world"
                );
            } else {
                assert!(!output.status.success());
                assert!(
                    String::from_utf8_lossy(&output.stderr)
                        .contains("SHA256 integrity check failed")
                );
                assert_eq!(std::fs::read_to_string(&payload).unwrap(), prefix);
                assert!(!dir.path().join("build/installed.dat").exists());
            }
        }
    }
}

#[test]
fn cli_resumed_download_validates_range_and_installs_complete_payload() {
    let (dir, recipes) = create_test_env();
    let payload = recipes.join("payload.dat");
    std::fs::write(&payload, "hello ").unwrap();
    let (url, server) = serve_response(
        "206 Partial Content",
        "Content-Range: bytes 6-10/11\r\nContent-Length: 5\r\n",
        "world",
    );
    write_resume_recipe(&recipes, &url);
    let output = run_recipe(
        &["install", "download-payload", "--no-persist-ctx"],
        &recipes,
    );
    let request = server.join().unwrap().to_lowercase();
    assert_success(&output);
    assert!(request.contains("range: bytes=6-"));
    assert!(request.contains("accept-encoding: identity"));
    assert_eq!(std::fs::read(&payload).unwrap(), b"hello world");
    assert_eq!(
        std::fs::read(dir.path().join("build/installed.dat")).unwrap(),
        b"hello world"
    );
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
}

#[test]
fn cli_resume_handles_complete_range_response_and_rejects_wrong_total() {
    for total in [11, 12] {
        let (dir, recipes) = create_test_env();
        let payload = recipes.join("payload.dat");
        std::fs::write(&payload, "hello world").unwrap();
        let (url, server) = serve_response(
            "416 Range Not Satisfiable",
            &format!("Content-Range: bytes */{total}\r\nContent-Length: 0\r\n"),
            "",
        );
        write_resume_recipe(&recipes, &url);
        let output = run_recipe(
            &["install", "download-payload", "--no-persist-ctx"],
            &recipes,
        );
        server.join().unwrap();
        if total == 11 {
            assert_success(&output);
            assert_eq!(
                std::fs::read(dir.path().join("build/installed.dat")).unwrap(),
                b"hello world"
            );
        } else {
            assert!(!output.status.success());
            assert!(!dir.path().join("build/installed.dat").exists());
        }
        assert_eq!(std::fs::read(&payload).unwrap(), b"hello world");
    }
}

#[test]
fn cli_invalid_or_interrupted_resume_preserves_destination() {
    for (status, headers, body) in [
        (
            "206 Partial Content",
            "Content-Range: bytes 5-9/10\r\nContent-Length: 5\r\n",
            "world",
        ),
        (
            "206 Partial Content",
            "Content-Range: bytes 6-11/11\r\nContent-Length: 5\r\n",
            "world",
        ),
        ("206 Partial Content", "Content-Length: 5\r\n", "world"),
        (
            "206 Partial Content",
            "Content-Range: bytes 6-10/11\r\nContent-Length: 5\r\n",
            "wo",
        ),
        (
            "206 Partial Content",
            "Content-Range: bytes 6-10/11\r\nContent-Length: 4\r\n",
            "worl",
        ),
        (
            "206 Partial Content",
            "Content-Range: bytes 6-8/11\r\nContent-Length: 3\r\n",
            "wor",
        ),
        ("200 OK", "Content-Length: 11\r\n", "hello world"),
    ] {
        let (dir, recipes) = create_test_env();
        let payload = recipes.join("payload.dat");
        std::fs::write(&payload, "hello ").unwrap();
        let (url, server) = serve_response(status, headers, body);
        write_resume_recipe(&recipes, &url);
        let output = run_recipe(
            &["install", "download-payload", "--no-persist-ctx"],
            &recipes,
        );
        server.join().unwrap();
        assert!(
            !output.status.success(),
            "accepted invalid resume: {headers}"
        );
        assert_eq!(std::fs::read(&payload).unwrap(), b"hello ");
        assert!(!dir.path().join("build/installed.dat").exists());
        assert!(!std::fs::read_dir(&recipes).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".recipe-download.")
        }));
    }
}

#[test]
fn cli_interrupted_fresh_resume_never_publishes_partial_destination() {
    let (dir, recipes) = create_test_env();
    let (url, server) = serve_download("truncated", 1024);
    write_resume_recipe(&recipes, &url);
    let output = run_recipe(
        &["install", "download-payload", "--no-persist-ctx"],
        &recipes,
    );
    server.join().unwrap();
    assert!(!output.status.success());
    assert!(!recipes.join("payload.dat").exists());
    assert!(!dir.path().join("build/installed.dat").exists());
}

fn write_download_recipe(recipes: &Path, url: &str) {
    write_recipe(
        recipes,
        "download-payload",
        &r#"
let ctx = #{ name: "download-payload", url: "DOWNLOAD_URL" };
fn is_installed(ctx) { throw "install requested"; }
fn is_acquired(ctx) { throw "download requested"; }
fn acquire(ctx) {
    ctx.payload = download(ctx.url, join_path(RECIPE_DIR, "payload.dat"));
    ctx
}
fn install(ctx) {
    copy_file(ctx.payload, join_path(BUILD_DIR, "installed.dat"));
    ctx
}
"#
        .replace("DOWNLOAD_URL", url),
    );
}

#[test]
fn cli_interrupted_download_preserves_previous_file_and_removes_partial() {
    for existing in [false, true] {
        let (dir, recipes) = create_test_env();
        let payload = recipes.join("payload.dat");
        if existing {
            std::fs::write(&payload, "previous verified payload").unwrap();
        }
        let (url, server) = serve_download("truncated", 1024);
        write_download_recipe(&recipes, &url);

        let output = run_recipe(
            &["install", "download-payload", "--no-persist-ctx"],
            &recipes,
        );
        server.join().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("read error"));
        if existing {
            assert_eq!(
                std::fs::read_to_string(&payload).unwrap(),
                "previous verified payload"
            );
        } else {
            assert!(
                !payload.exists(),
                "interrupted download published a partial destination"
            );
        }
        assert!(!dir.path().join("build/installed.dat").exists());
        assert!(!std::fs::read_dir(&recipes).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".recipe-download.")
        }));
    }
}

#[test]
fn cli_complete_download_replaces_destination_and_installs_payload() {
    let (dir, recipes) = create_test_env();
    let payload = recipes.join("payload.dat");
    std::fs::write(&payload, "old content").unwrap();
    let body = "complete downloaded payload";
    let (url, server) = serve_download(body, body.len());
    write_download_recipe(&recipes, &url);

    let output = run_recipe(
        &["install", "download-payload", "--no-persist-ctx"],
        &recipes,
    );
    server.join().unwrap();
    assert_success(&output);
    assert_eq!(std::fs::read_to_string(&payload).unwrap(), body);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("build/installed.dat")).unwrap(),
        body
    );
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap();
}
