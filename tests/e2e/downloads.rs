use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
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
        let (mut stream, received) = accept_request(&listener);
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

fn accept_request(listener: &TcpListener) -> (TcpStream, String) {
    let deadline = Instant::now() + Duration::from_secs(120);
    let (mut stream, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
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
    (stream, received)
}

/// Serve one bounded set of normal package requests on a shared origin.
pub(super) fn serve_files(files: Vec<(String, Vec<u8>)>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/payload/", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut files: std::collections::BTreeMap<_, _> = files
            .into_iter()
            .map(|(name, body)| (format!("/payload/{name}"), body))
            .collect();
        while !files.is_empty() {
            let (mut stream, request) = accept_request(&listener);
            let path = request
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            let body = files
                .remove(path)
                .expect("unexpected or repeated package request");
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    (url, server)
}

fn download_recipe(recipes: &Path, url: &str, digest: &str, prefix: Option<&str>, resume: bool) {
    let endpoints: Vec<&str> = url
        .strip_prefix("http://")
        .filter(|url| !url.chars().any(char::is_control))
        .map(|url| vec![url.split('/').next().unwrap()])
        .unwrap_or_default();
    write_recipe(
        recipes,
        "download-payload",
        &format!(
            r#"
const RECIPE_API = 2;
const NEEDS = #{{ outputs: #{{ product: "file" }}, network: {endpoints:?} }};
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
    } else {
        command.arg("--no-network");
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
const NEEDS = #{{}};
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

#[test]
fn managed_redirects_require_destination_grants_and_byte_budget() {
    use sha2::Digest;
    use std::net::TcpListener;

    for allowed in [true, false] {
        let (_dir, recipes) = create_test_env();
        let payload = b"redirected verified payload";
        let digest = hex::encode(sha2::Sha256::digest(payload));
        let denied = TcpListener::bind("127.0.0.1:0").unwrap();
        denied.set_nonblocking(true).unwrap();
        let (target, destination) = if allowed {
            let (url, task) = serve_response(
                "200 OK",
                &format!("Content-Length: {}\r\n", payload.len()),
                payload,
            );
            (url, Some(task))
        } else {
            (
                format!("http://{}/payload", denied.local_addr().unwrap()),
                None,
            )
        };
        let (url, redirect) = serve_response(
            "302 Found",
            &format!("Location: {target}\r\nContent-Length: 0\r\n"),
            b"",
        );
        download_recipe(&recipes, &url, &digest, Some("previous payload"), false);
        let mut command = download_command(&recipes, Some(&url));
        if allowed {
            let path = recipes.join("download-payload.rhai");
            let original = std::fs::read_to_string(&path).unwrap();
            let from = url
                .strip_prefix("http://")
                .unwrap()
                .split('/')
                .next()
                .unwrap();
            let to = target
                .strip_prefix("http://")
                .unwrap()
                .split('/')
                .next()
                .unwrap();
            std::fs::write(
                path,
                original.replace(
                    &format!("network: [{from:?}]"),
                    &format!("network: [{from:?}, {to:?}]"),
                ),
            )
            .unwrap();
            command.args([
                "--network-host",
                target
                    .strip_prefix("http://")
                    .unwrap()
                    .split('/')
                    .next()
                    .unwrap(),
            ]);
        }
        let output = command.output().unwrap();
        redirect.join().unwrap();
        if let Some(destination) = destination {
            destination.join().unwrap();
        }
        if allowed {
            assert_success(&output);
            let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                std::fs::read(context["outputs"]["product"].as_str().unwrap()).unwrap(),
                payload
            );
        } else {
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("DOWNLOAD_CACHE_PRESERVED"));
            assert_eq!(
                denied.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }

    let (_dir, recipes) = create_test_env();
    let payload = vec![b'x'; 16 * 1024];
    let digest = hex::encode(sha2::Sha256::digest(&payload));
    let (url, server) = serve_response(
        "200 OK",
        &format!("Content-Length: {}\r\n", payload.len()),
        &payload,
    );
    download_recipe(&recipes, &url, &digest, Some("previous payload"), false);
    let output = download_command(&recipes, Some(&url))
        .args(["--network-max-bytes", "512"])
        .output()
        .unwrap();
    // The peer may observe closure as soon as the supervisor exhausts the grant;
    // that is an I/O result inside the thread, never a panic.
    server.join().expect("download fixture server panicked");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("DOWNLOAD_CACHE_PRESERVED"));
}

#[test]
fn managed_concurrent_download_cache_survives_offline_reinstall() {
    use sha2::Digest;
    use std::process::Stdio;

    let (dir, recipes) = create_test_env();
    let payload = b"single verified acquisition shared by writers";
    let digest = hex::encode(sha2::Sha256::digest(payload));
    let (url, server) = serve_response(
        "200 OK",
        &format!("Content-Length: {}\r\n", payload.len()),
        payload,
    );
    let endpoint = url
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap();
    write_recipe(
        &recipes,
        "cached",
        &format!(
            r#"
const RECIPE_API = 2;
const NEEDS = #{{ outputs: #{{ product: "file" }}, network: [{endpoint:?}] }};
let ctx = #{{name: "cached-acquisition", source: join_path(RUNTIME.paths.build, "source"), digest: {digest:?}}};
fn is_acquired(ctx) {{
    if !is_file(ctx.source) {{ return needs_work(ctx, "fetch source"); }}
    verify_sha256(ctx.source, ctx.digest); ready(ctx)
}}
fn is_built(ctx) {{ is_acquired(ctx) }}
fn is_installed(ctx) {{
    if !is_file("/out/product") {{ return needs_work(ctx, "install source"); }}
    verify_sha256("/out/product", ctx.digest);
    ctx.outputs.product = "/out/product"; ready(ctx)
}}
fn acquire(ctx) {{ download({url:?}, ctx.source, ctx.digest); ctx }}
fn install(ctx) {{
    verify_sha256(ctx.source, ctx.digest);
    copy_file(ctx.source, "/out/product");
    ctx.outputs.product = "/out/product"; ctx
}}
fn remove(ctx) {{ rm("/out/product"); ctx.outputs = #{{}}; ctx }}
"#
        ),
    );
    let command = |operation: &str| {
        let mut command = recipe_command(&recipes);
        command
            .args([operation, "cached", "--recipes-path"])
            .arg(&recipes)
            .arg("--build-dir")
            .arg(dir.path().join("destination"))
            .arg("--state-dir")
            .arg(dir.path().join("retained"))
            .args([
                "--network-host",
                url.strip_prefix("http://")
                    .unwrap()
                    .split('/')
                    .next()
                    .unwrap(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    };
    let first = command("install").spawn().unwrap();
    let second = command("install").spawn().unwrap();
    let first = first.wait_with_output().unwrap();
    let second = second.wait_with_output().unwrap();
    assert_success(&first);
    assert_success(&second);
    // The server handles one request then closes. A second fetch cannot succeed.
    server.join().unwrap();
    for output in [&first, &second] {
        let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            std::fs::read(context["outputs"]["product"].as_str().unwrap()).unwrap(),
            payload
        );
    }
    assert_success(&command("remove").output().unwrap());
    let offline = command("install").output().unwrap();
    assert_success(&offline);
    let context: serde_json::Value = serde_json::from_slice(&offline.stdout).unwrap();
    assert_eq!(
        std::fs::read(context["outputs"]["product"].as_str().unwrap()).unwrap(),
        payload
    );
}
