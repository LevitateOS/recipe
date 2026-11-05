use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

fn serve_download(
    body: &'static str,
    advertised_length: usize,
) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/payload", listener.local_addr().unwrap());
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
        loop {
            line.clear();
            if request.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                break;
            }
        }
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {advertised_length}\r\nConnection: close\r\n\r\n{body}").unwrap();
    });
    (url, server)
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
