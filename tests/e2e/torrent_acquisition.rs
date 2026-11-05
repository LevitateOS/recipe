//! Actual BitTorrent seeder, loopback tracker, and Recipe CLI acquisition.
use super::*;
use librqbit::{AddTorrent, AddTorrentOptions, CreateTorrentOptions, Session, SessionOptions};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

struct TorrentFixture {
    url: String,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<()>>,
    session: Arc<Session>,
    runtime: tokio::runtime::Runtime,
    _seed: TempDir,
}

impl TorrentFixture {
    fn new(multifile: bool, manifest_digest: Option<&str>) -> Self {
        let seed = tempfile::tempdir().unwrap();
        std::fs::write(seed.path().join("payload.iso"), b"actual torrent payload\n").unwrap();
        if multifile {
            std::fs::create_dir(seed.path().join("nested")).unwrap();
            std::fs::write(seed.path().join("nested/second.txt"), b"second file\n").unwrap();
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let tracker_url = format!("{base_url}/announce");
        let manifest_digest = manifest_digest
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{:x}", Sha256::digest(b"actual torrent payload\n")));
        let checksum = format!("SHA256 (payload.iso) = {manifest_digest}\n").into_bytes();
        let (session, metadata) = runtime.block_on(async {
            let path = if multifile {
                seed.path().to_path_buf()
            } else {
                seed.path().join("payload.iso")
            };
            let torrent = librqbit::create_torrent(&path, CreateTorrentOptions::default())
                .await
                .unwrap();
            let original = torrent.as_bytes().unwrap();
            let session = Session::new_with_opts(
                seed.path().to_path_buf(),
                SessionOptions {
                    disable_dht: true,
                    disable_dht_persistence: true,
                    disable_upload: false,
                    listen_port_range: Some(21000..61000),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let handle = session
                .add_torrent(
                    AddTorrent::from_bytes(original.clone()),
                    Some(AddTorrentOptions {
                        overwrite: true,
                        disable_trackers: true,
                        output_folder: Some(seed.path().to_string_lossy().into_owned()),
                        ..Default::default()
                    }),
                )
                .await
                .unwrap()
                .into_handle()
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), handle.wait_until_completed())
                .await
                .unwrap()
                .unwrap();
            // Add an announce field to the outer bencoded dictionary. The info
            // dictionary (and therefore torrent identity) stays unchanged.
            let mut metadata =
                format!("d8:announce{}:{tracker_url}", tracker_url.len()).into_bytes();
            metadata.extend_from_slice(&original[1..]);
            (session, metadata)
        });
        let port = session.tcp_listen_port().unwrap();
        let mut tracker = b"d8:intervali1e5:peers6:".to_vec();
        tracker.extend_from_slice(&[127, 0, 0, 1]);
        tracker.extend_from_slice(&port.to_be_bytes());
        tracker.push(b'e');
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let server_stop = stop.clone();
        let server_requests = requests.clone();
        let server = std::thread::spawn(move || {
            while !server_stop.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => panic!("torrent HTTP fixture: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut first = String::new();
                if reader.read_line(&mut first).is_err() {
                    continue;
                }
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if line == "\r\n" => break,
                        _ => {}
                    }
                }
                let body = if first.starts_with("GET /announce?") {
                    &tracker
                } else if first.starts_with("GET /CHECKSUM ") {
                    &checksum
                } else {
                    server_requests.fetch_add(1, Ordering::Relaxed);
                    &metadata
                };
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream
                    .write_all(headers.as_bytes())
                    .and_then(|_| stream.write_all(body));
            }
        });
        Self {
            url: format!("{base_url}/payload.torrent"),
            requests,
            stop,
            server: Some(server),
            session,
            runtime,
            _seed: seed,
        }
    }
}

impl Drop for TorrentFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.server.take().unwrap().join().unwrap();
        self.runtime.block_on(self.session.stop());
    }
}

fn torrent_cli(
    recipes: &Path,
    fixture: &TorrentFixture,
    existing: bool,
    mode: &str,
) -> std::process::Output {
    let digest = if mode == "wrong-digest" {
        "0".repeat(64)
    } else {
        format!("{:x}", Sha256::digest(b"actual torrent payload\n"))
    };
    let expression = if mode == "multifile" {
        format!("torrent({:?}, ctx.cache)", fixture.url)
    } else {
        format!(
            "torrent({:?}, ctx.cache, \"payload.iso\", {:?})",
            fixture.url, digest
        )
    };
    write_recipe(
        recipes,
        "torrent-acquisition",
        &format!(
            r#"
let ctx = #{{name: "torrent-acquisition", cache: join_path(BUILD_DIR, "cache with spaces"), existing: {existing}}};
mkdir(ctx.cache);
let initialized = join_path(ctx.cache, ".fixture-created");
if !is_file(initialized) {{
    if ctx.existing {{ write_file(join_path(ctx.cache, "payload.iso"), "previous verified cache\n"); }}
    write_file(join_path(ctx.cache, "unrelated"), "keep");
    write_file(initialized, "initialized");
}}
fn acquire(ctx) {{ ctx.path = {expression}; ctx }}
fn install(ctx) {{
    if read_file(join_path(ctx.cache, "payload.iso")) != "actual torrent payload\n" {{ throw "wrong transferred payload"; }}
    if {multifile} && read_file(join_path(ctx.cache, "nested/second.txt")) != "second file\n" {{ throw "wrong second payload"; }}
    if read_file(join_path(ctx.cache, "unrelated")) != "keep" {{ throw "unrelated cache was changed"; }}
    if glob_list(join_path(ctx.cache, ".torrent-part-*")).len != 0 {{ throw "staging leaked"; }}
    ctx.transfer_verified = true;
    ctx
}}
fn cleanup(ctx, reason) {{
    if reason == "auto.acquire.failure" {{
        let path = join_path(ctx.cache, "payload.iso");
        if ctx.existing {{
            if read_file(path) != "previous verified cache\n" {{ throw "old cache was changed"; }}
        }} else if is_file(path) {{ throw "failed transfer was published"; }}
        if glob_list(join_path(ctx.cache, ".torrent-part-*")).len != 0 {{ throw "failed staging leaked"; }}
        log("TORRENT_CACHE_PRESERVED");
    }}
    ctx
}}
"#,
            multifile = mode == "multifile"
        ),
    );
    run_recipe(
        &["install", "torrent-acquisition", "--no-persist-ctx"],
        recipes,
    )
}

#[test]
fn cli_verified_torrent_publishes_only_the_expected_digest_and_preserves_old_files() {
    let fixture = TorrentFixture::new(false, None);
    for (existing, mode) in [
        (false, "verified"),
        (true, "verified"),
        (false, "wrong-digest"),
        (true, "wrong-digest"),
    ] {
        let (_dir, recipes) = create_test_env();
        let output = torrent_cli(&recipes, &fixture, existing, mode);
        if mode == "verified" {
            assert_success(&output);
            let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(ctx["transfer_verified"], true);
        } else {
            assert!(!output.status.success());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("integrity check failed"), "{stderr}");
            assert!(stderr.contains("TORRENT_CACHE_PRESERVED"), "{stderr}");
        }
    }
    assert!(fixture.requests.load(Ordering::Relaxed) >= 4);
}

#[test]
fn cli_torrent_checks_metadata_selection_and_retains_legacy_multifile_outputs() {
    let fixture = TorrentFixture::new(true, None);
    let (_dir, recipes) = create_test_env();
    let output = torrent_cli(&recipes, &fixture, false, "wrong-selection");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("exactly the expected file"), "{stderr}");
    assert!(stderr.contains("TORRENT_CACHE_PRESERVED"), "{stderr}");
    // A fresh invocation owns its work view; no cross-invocation resume is assumed.
    let (_dir, recipes) = create_test_env();
    let output = torrent_cli(&recipes, &fixture, false, "multifile");
    assert_success(&output);
    let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(ctx["transfer_verified"], true);
}

#[test]
fn cli_fedora_refresh_keeps_verified_cache_and_marker_when_torrent_digest_fails() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../distro-builder/recipes");
    for corrupt_replacement in [false, true] {
        let (_dir, recipes) = create_test_env();
        let old_bytes = b"previous verified Fedora cache\n";
        let digest = format!(
            "{:x}",
            Sha256::digest(if corrupt_replacement {
                old_bytes.as_slice()
            } else {
                b"actual torrent payload\n"
            })
        );
        let fixture = TorrentFixture::new(false, Some(&digest));
        std::fs::create_dir(recipes.join("sources")).unwrap();
        let facts = std::fs::read_to_string(source.join("sources/fedora.rhai")).unwrap();
        let marker_code = &facts[facts.find("\nfn marker_path").unwrap()..];
        let fixture_facts = format!(
            "fn facts() {{ #{{iso_name: \"payload.iso\", sha256: {:?}, torrent_url: {:?}, checksum_url: {:?}}} }}\n{}",
            digest,
            fixture.url,
            fixture.url.replace("/payload.torrent", "/CHECKSUM"),
            marker_code
        );
        std::fs::write(recipes.join("sources/fedora.rhai"), fixture_facts).unwrap();
        let before_marker = format!("sha256={digest}\nfingerprint={}\n", old_bytes.len());
        // Keep production acquisition/readiness/install code and actual marker
        // helpers. Only package facts and a diagnostic fixture hook are added.
        let mut recipe = std::fs::read_to_string(source.join("fedora-preseed-iso.rhai")).unwrap();
        recipe.push_str(&format!(r#"
mkdir(BUILD_DIR);
if !is_file(join_path(BUILD_DIR, ".fixture-created")) {{
    write_file(ctx.iso_path, "previous verified Fedora cache\n");
    write_file(source::marker_path(ctx.trust_dir), {before_marker:?});
    write_file(join_path(BUILD_DIR, ".fixture-created"), "initialized");
}}
fn cleanup(ctx, reason) {{
    if reason == "auto.acquire.failure" {{
        if read_file(ctx.iso_path) != "previous verified Fedora cache\n" {{ throw "old Fedora cache changed"; }}
        if read_file(source::marker_path(ctx.trust_dir)) != {before_marker:?} {{ throw "old Fedora marker changed"; }}
        log("FEDORA_CACHE_AND_MARKER_PRESERVED");
    }} else if reason == "auto.install.success" {{
        verify_sha256(ctx.iso_path, ctx.sha256);
        if !source::marker_is_valid(ctx.trust_dir, ctx.iso_path, ctx.sha256) {{ throw "new Fedora marker invalid"; }}
        ctx.transfer_verified = true;
    }}
    ctx
}}
"#));
        std::fs::write(recipes.join("fedora-preseed-iso.rhai"), recipe).unwrap();
        let output = recipe_command(&recipes)
            .args(["install", "fedora-preseed-iso", "--no-persist-ctx"])
            .arg("--recipes-path")
            .arg(&recipes)
            .arg("--build-dir")
            .arg(recipes.parent().unwrap().join("build"))
            .env_remove("FEDORA_FORCE_REFRESH")
            .env("ROOTFS_SOURCE_FORCE_REFRESH", "1")
            .output()
            .unwrap();
        if corrupt_replacement {
            assert!(!output.status.success());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.contains("integrity check failed"), "{stderr}");
            assert!(
                stderr.contains("FEDORA_CACHE_AND_MARKER_PRESERVED"),
                "{stderr}"
            );
        } else {
            assert_success(&output);
            let ctx: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(ctx["transfer_verified"], true);
        }
        assert!(fixture.requests.load(Ordering::Relaxed) > 0);
    }
}
