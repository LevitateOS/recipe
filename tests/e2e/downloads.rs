use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

pub(super) fn serve_download(
    body: impl AsRef<[u8]>,
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
    body: impl AsRef<[u8]>,
) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/payload", listener.local_addr().unwrap());
    let headers = headers.to_owned();
    let body = body.as_ref().to_vec();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(120);
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
            "HTTP/1.1 {status}\r\n{headers}Connection: close\r\n\r\n"
        )
        .unwrap();
        stream.write_all(&body).unwrap();
        received
    });
    (url, server)
}

fn download_recipe(recipes: &Path, url: &str, digest: &str, prefix: Option<&str>, resume: bool) {
    write_recipe(
        recipes,
        "download-payload",
        &format!(
            r#"
const RECIPE_API = 2;
let ctx = #{{name: "download-payload", url: {url:?}, digest: {digest:?},
    payload: join_path(RUNTIME.paths.build, "payload.dat"),
    state: #{{downloaded: false, installed: false}}}};
fn is_acquired(ctx) {{
    if ctx.state.downloaded {{ return ready(ctx); }}
    needs_work(ctx, "fetch verified payload")
}}
fn is_built(ctx) {{ needs_work(ctx, "source preparation required") }}
fn is_installed(ctx) {{
    if ctx.state.installed {{ return ready(ctx); }}
    needs_work(ctx, "install verified payload")
}}
fn acquire(ctx) {{
    mkdir(dirname(ctx.payload));
    if {existing} {{ write_file(ctx.payload, {prefix:?}); }}
    ctx.payload = download(ctx.url, ctx.payload, ctx.digest, {resume});
    ctx.state.downloaded = true;
    ctx
}}
fn install(ctx) {{
    verify_sha256(ctx.payload, ctx.digest);
    copy_file(ctx.payload, "/out/product");
    ctx.outputs.product = "/out/product";
    ctx.state.installed = true;
    ctx
}}
fn cleanup(ctx, reason) {{
    if reason == "auto.acquire.failure" {{
        if {existing} {{
            if read_file(ctx.payload) != {prefix:?} {{ throw "previous payload changed"; }}
        }} else if is_file(ctx.payload) {{ throw "failed transfer was published"; }}
        if glob_list(join_path(dirname(ctx.payload), ".recipe-download.*")).len != 0 {{
            throw "download staging leaked";
        }}
        log("DOWNLOAD_CACHE_PRESERVED");
    }}
    ctx
}}
"#,
            existing = prefix.is_some(),
            prefix = prefix.unwrap_or("")
        ),
    );
}

fn download_command(recipes: &Path, url: Option<&str>) -> Command {
    let mut command = recipe_command(recipes);
    command
        .args([
            "install",
            "download-payload",
            "--no-persist-state",
            "--recipes-path",
        ])
        .arg(recipes);
    if let Some(url) = url {
        let authority = url
            .strip_prefix("http://")
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        command.args(["--network-host", authority]);
    }
    command
}

#[test]
fn managed_verified_download_checks_digest_before_publication() {
    use sha2::{Digest, Sha256};
    let digest = hex::encode(Sha256::digest(b"hello world"));
    for prefix in [None, Some("previous verified payload")] {
        for payload in ["hello world", "wrong world"] {
            let (_dir, recipes) = create_test_env();
            let (url, server) = serve_download(payload, payload.len());
            download_recipe(&recipes, &url, &digest, prefix, false);
            let source = std::fs::read(recipes.join("download-payload.rhai")).unwrap();
            let output = download_command(&recipes, Some(&url)).output().unwrap();
            server.join().unwrap();
            if payload == "hello world" {
                assert_success(&output);
                let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(ctx["state"]["installed"], true);
            } else {
                assert!(!output.status.success());
                let diagnostic = String::from_utf8_lossy(&output.stderr);
                assert!(
                    diagnostic.contains("integrity check failed"),
                    "{diagnostic}"
                );
                assert!(
                    diagnostic.contains("DOWNLOAD_CACHE_PRESERVED"),
                    "{diagnostic}"
                );
            }
            assert_eq!(
                std::fs::read(recipes.join("download-payload.rhai")).unwrap(),
                source
            );
            assert!(
                !recipes.join("payload.dat").exists(),
                "worker wrote into host recipe inputs"
            );
        }
    }
}

#[test]
fn managed_resume_validates_ranges_and_complete_digest() {
    use sha2::{Digest, Sha256};
    let digest = hex::encode(Sha256::digest(b"hello world"));
    for (status, headers, body, prefix, valid) in [
        (
            "206 Partial Content",
            "Content-Range: bytes 6-10/11\r\nContent-Length: 5\r\n",
            "world",
            "hello ",
            true,
        ),
        (
            "206 Partial Content",
            "Content-Range: bytes 6-10/11\r\nContent-Length: 5\r\n",
            "other",
            "hello ",
            false,
        ),
        (
            "416 Range Not Satisfiable",
            "Content-Range: bytes */11\r\nContent-Length: 0\r\n",
            "",
            "hello world",
            true,
        ),
        (
            "416 Range Not Satisfiable",
            "Content-Range: bytes */11\r\nContent-Length: 0\r\n",
            "",
            "wrong world",
            false,
        ),
        (
            "416 Range Not Satisfiable",
            "Content-Range: bytes */12\r\nContent-Length: 0\r\n",
            "",
            "hello world",
            false,
        ),
        (
            "206 Partial Content",
            "Content-Range: bytes 5-9/10\r\nContent-Length: 5\r\n",
            "world",
            "hello ",
            false,
        ),
        (
            "206 Partial Content",
            "Content-Length: 5\r\n",
            "world",
            "hello ",
            false,
        ),
        (
            "206 Partial Content",
            "Content-Range: bytes 6-10/11\r\nContent-Length: 5\r\n",
            "wo",
            "hello ",
            false,
        ),
        (
            "200 OK",
            "Content-Length: 11\r\n",
            "hello world",
            "hello ",
            false,
        ),
    ] {
        let (_dir, recipes) = create_test_env();
        let (url, server) = serve_response(status, headers, body);
        download_recipe(&recipes, &url, &digest, Some(prefix), true);
        let output = download_command(&recipes, Some(&url)).output().unwrap();
        let request = server.join().unwrap().to_lowercase();
        assert!(request.contains(&format!("range: bytes={}-", prefix.len())));
        assert!(request.contains("accept-encoding: identity"));
        if valid {
            assert_success(&output);
        } else {
            assert!(
                !output.status.success(),
                "accepted invalid resume {headers}"
            );
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            assert!(
                diagnostic.contains("DOWNLOAD_CACHE_PRESERVED"),
                "{diagnostic}"
            );
        }
    }
}

#[test]
fn managed_interrupted_download_preserves_previous_bytes() {
    use sha2::{Digest, Sha256};
    let digest = hex::encode(Sha256::digest(b"complete payload"));
    for resume in [false, true] {
        for prefix in [None, Some("previous payload")] {
            let (_dir, recipes) = create_test_env();
            let (url, server) = serve_download("truncated", 1024);
            download_recipe(&recipes, &url, &digest, prefix, resume);
            let output = download_command(&recipes, Some(&url)).output().unwrap();
            server.join().unwrap();
            assert!(!output.status.success());
            let diagnostic = String::from_utf8_lossy(&output.stderr);
            assert!(
                diagnostic.contains("DOWNLOAD_CACHE_PRESERVED"),
                "{diagnostic}"
            );
        }
    }
}

#[test]
fn managed_download_rejects_invalid_arguments_without_network() {
    for (url, digest, diagnostic) in [
        (
            "https://example.invalid/payload",
            "not-a-sha256".to_owned(),
            "64 hexadecimal",
        ),
        (
            "file:///inputs/source",
            "0".repeat(64),
            "http:// or https://",
        ),
        (
            "https://example.invalid/\npayload",
            "0".repeat(64),
            "http:// or https://",
        ),
    ] {
        let (_dir, recipes) = create_test_env();
        download_recipe(
            &recipes,
            url,
            &digest,
            Some("previous verified payload"),
            false,
        );
        let output = download_command(&recipes, None).output().unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(diagnostic), "{stderr}");
        assert!(stderr.contains("DOWNLOAD_CACHE_PRESERVED"), "{stderr}");
    }
}

#[test]
fn managed_download_rejects_directory_destinations_without_network() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "download-payload",
        &format!(
            r#"
const RECIPE_API = 2;
let ctx = #{{name: "download-payload", state: #{{checked: false}}}};
fn is_acquired(ctx) {{ ready(ctx) }}
fn is_built(ctx) {{ ready(ctx) }}
fn is_installed(ctx) {{
    if ctx.state.checked {{ return ready(ctx); }}
    needs_work(ctx, "validate destinations")
}}
fn install(ctx) {{
    mkdir("/work/build/cache");
    write_file("/work/build/cache/previous", "keep verified bytes");
    for destination in ["/work/build/cache", "/"] {{
        let rejected = false;
        try {{ download("https://example.invalid/payload", destination, {digest:?}); }}
        catch (error) {{
            if !error.to_string().contains("regular file") {{ throw error; }}
            rejected = true;
        }}
        if !rejected {{ throw "directory destination accepted"; }}
    }}
    if read_file("/work/build/cache/previous") != "keep verified bytes" {{ throw "cache was changed"; }}
    ctx.state.checked = true;
    ctx
}}
"#,
            digest = "0".repeat(64)
        ),
    );
    let output = download_command(&recipes, None).output().unwrap();
    assert_success(&output);
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["state"]["checked"], true);
}
