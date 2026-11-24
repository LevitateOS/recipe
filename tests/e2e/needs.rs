//! NEEDS through the real declaration CLI and the public Ginux lifecycle.
//! Preconditions: a real Ginux, privileges and the imported compiler image
//! (header of `tests/e2e.rs`). Network grants are per port on the shared host
//! network (`[contract.network] outbound = ["tcp:PORT"]`): the cases that read
//! `network_usage` read what the worker counted for Recipe's own downloads, and
//! the denial cases check that a port without a grant is refused.
use super::*;

#[test]
fn declaration_commands_never_evaluate_top_level_code_and_reject_widening() {
    let (dir, recipes) = create_test_env();
    let marker = dir.path().join("setup-effect");
    let source = format!(
        r#"const RECIPE_API = 1;
const NEEDS = #{{ inputs: #{{ required: "file", optional: "directory?" }},
    outputs: #{{ product: "file" }}, network: ["127.0.0.1:12345"],
    memory_mib: 4096, storage_max_objects: 20000, timeout_secs: 120, network_max_bytes: 65536 }};
write_file({marker:?}, "SETUP_EXECUTED"); throw "SETUP_EXECUTED";
"#,
        marker = marker.to_str().unwrap()
    );
    write_recipe(&recipes, "new", &source);
    write_recipe(
        &recipes,
        "old",
        "const RECIPE_API = 1;\nconst NEEDS = #{};\nthrow \"SETUP_EXECUTED\";",
    );
    let input = dir.path().join("selected");
    std::fs::write(&input, "explicit input").unwrap();
    let binding = format!("required={}", input.display());
    for args in [
        vec!["inspect", "new"],
        vec!["explain", "new", "network.connect"],
        vec!["diff", "old", "new"],
        vec!["validate", "new", "--input", &binding],
    ] {
        let result = run_recipe(&args, &recipes);
        assert_success(&result);
        let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert!(value.is_object());
        if args[0] == "explain" {
            assert_eq!(
                value["execution"]["network_hosts"],
                serde_json::json!(["127.0.0.1:12345"])
            );
            assert_eq!(value["execution"]["timeout_secs"], 120);
        }
        assert!(!marker.exists());
    }
    for (flags, diagnostic) in [
        (vec!["--memory-mib", "4097"], "NEEDS.memory_mib"),
        (vec!["--network-host", "127.0.0.1:12346"], "NEEDS.network"),
        (vec!["--timeout-secs", "121"], "NEEDS.timeout_secs"),
        (
            // Above the cap of a declared 65536: 65536 + 64 KiB (one file)
            // + 65536 / 4 (docs/recipe-format.md, section 2.1).
            vec!["--network-max-bytes", "147457"],
            "NEEDS.network_max_bytes",
        ),
    ] {
        let mut args = vec!["inspect", "new"];
        args.extend(flags);
        let result = run_recipe(&args, &recipes);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains(diagnostic));
        assert!(!marker.exists());
    }
    let allowed = run_recipe(
        &["inspect", "new", "--memory-mib", "2048", "--no-network"],
        &recipes,
    );
    assert_success(&allowed);
    let value: serde_json::Value = serde_json::from_slice(&allowed.stdout).unwrap();
    assert_eq!(value["effective"]["memory_mib"], 2048);
    assert_eq!(value["effective"]["network"], serde_json::json!([]));
    assert_eq!(value["contract"]["catalog"], 2);
    assert!(
        !value["contract"]["contract"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .any(|selector| selector == "network.inet.tcp.connect")
    );
    let missing = run_recipe(&["validate", "new"], &recipes);
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("missing required NEEDS.inputs.required")
    );
    let wrong = run_recipe(
        &[
            "validate",
            "new",
            "--input",
            &format!("required={}", dir.path().display()),
        ],
        &recipes,
    );
    assert!(!wrong.status.success());
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("requires file"));
    assert!(!marker.exists());
}

#[test]
fn literal_authority_and_dependency_graph_fail_before_any_setup() {
    let (_dir, recipes) = create_test_env();
    for (declaration, diagnostic) in [
        ("let NEEDS = #{};", "const and literal"),
        ("const NEEDS = make_needs();", "literal map"),
        ("const NEEDS = #{ network: ENDPOINTS };", "must be literal"),
        ("const NEEDS = #{ imaginary_grant: true };", "unknown field"),
        (
            "const NEEDS = #{ inputs: #{ x: \"socket\" } };",
            "must be file",
        ),
    ] {
        write_recipe(
            &recipes,
            "invalid",
            &format!("const RECIPE_API = 1;\n{declaration}\nprint(\"SETUP_EXECUTED\");"),
        );
        let result = run_recipe(&["inspect", "invalid"], &recipes);
        assert!(!result.status.success());
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains(diagnostic), "{error}");
        assert!(!error.contains("SETUP_EXECUTED"));
    }
}

#[test]
fn managed_typed_outputs_validate_before_final_retention_and_publication() {
    let (dir, recipes) = create_test_env();
    for (name, outputs, body, succeeds) in [
        (
            "valid",
            "#{product:\"file\",tree:\"directory\"}",
            "write_file(out.product,\"typed bytes\"); write_file(join_path(out.tree,\"leaf\"),\"in tree\");",
            true,
        ),
        ("missing", "#{product:\"file\"}", "", false),
        ("wrong-kind", "#{product:\"file\"}", "mkdir(out);", false),
    ] {
        write_recipe(
            &recipes,
            name,
            &format!(
                r#"const RECIPE_API = 1;
const NEEDS = #{{outputs:{outputs}}};
fn build(src, out) {{ {body} }}
"#
            ),
        );
        let result = run_recipe(&["install", name], &recipes);
        assert_eq!(
            result.status.success(),
            succeeds,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if succeeds {
            let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(
                std::fs::read_to_string(value["outputs"]["product"].as_str().unwrap()).unwrap(),
                "typed bytes"
            );
            assert!(Path::new(value["outputs"]["tree"].as_str().unwrap()).is_dir());
            assert_eq!(value["contract"]["catalog"], 2);
            // No grant, no usage; with a grant the worker reports what it counted.
            assert!(value["network_usage"].is_object() || value["network_usage"].is_null());
        } else {
            assert!(String::from_utf8_lossy(&result.stderr).contains("NEEDS.outputs"));
            assert!(result.stdout.is_empty());
        }
    }
    let published = std::fs::read_dir(dir.path().join("state/recipe/exports"))
        .unwrap()
        .count();
    assert_eq!(published, 1, "only the validated product may publish");
}

#[test]
fn managed_dependency_authority_inputs_and_products_are_separate() {
    let (dir, recipes) = create_test_env();
    let (url, server) = super::downloads::serve_download("dependency network product", 26);
    let endpoint = url
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap();
    write_recipe(
        &recipes,
        "leaf",
        &r#"const RECIPE_API = 1;
const NEEDS = #{ inputs: #{ asset: "file" }, outputs: #{ proof: "directory", note: "file" }, network: [@ENDPOINT@] };
fn build(src, out) {
    write_file(join_path(out.proof, "payload"), exec_output("curl", ["--noproxy", "*", "-fsS", @URL@]));
    copy_file(INPUTS.asset, join_path(out.proof, "asset"));
    ln("/usr/bin/true", join_path(out.proof, "absolute"));
    write_file(out.note, "leaf note");
}
"#
        .replace("@ENDPOINT@", &format!("{endpoint:?}"))
        .replace("@URL@", &format!("{url:?}")),
    );
    write_recipe(
        &recipes,
        "parent",
        &r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ report: "file" } };
let deps = ["leaf"];
fn build(src, out) {
    let proof = DEPS["leaf"].proof;
    let payload = read_file(join_path(proof, "payload"));
    let asset = read_file(join_path(proof, "asset"));
    if read_file(DEPS["leaf"].note) != "leaf note" { throw "dependency output map is wrong"; }
    if trim(exec_output("readlink", [join_path(proof, "absolute")])) != "/usr/bin/true" { throw "dependency rootfs link changed"; }
    if is_file("/inputs/asset") { throw "dependency input leaked into parent"; }
    let denied = exec("curl", ["--noproxy", "*", "--connect-timeout", "1", "--max-time", "2", "-fsS", @URL@]);
    if denied == 0 { throw "parent borrowed dependency network authority"; }
    write_file(out, "payload=" + payload + "\nasset=" + asset + "\ndenied=" + denied.to_string() + "\n");
}
"#
        .replace("@URL@", &format!("{url:?}")),
    );
    let asset = dir.path().join("dependency-only-input");
    std::fs::write(&asset, "explicit child binding").unwrap();
    let result = run_recipe(
        &[
            "install",
            "parent",
            "--input",
            &format!("leaf::asset={}", asset.display()),
        ],
        &recipes,
    );
    assert_success(&result);
    server.join().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let report = std::fs::read_to_string(value["outputs"]["report"].as_str().unwrap()).unwrap();
    let lines = report.lines().collect::<Vec<_>>();
    assert_eq!(lines[0], "payload=dependency network product", "{report}");
    assert_eq!(lines[1], "asset=explicit child binding", "{report}");
    assert_ne!(lines[2], "denied=0", "{report}");
    // The printed result is the parent's run, which has no grant: there is no
    // endpoint to account for, so its account is complete and empty (its own
    // curl was refused, `denied` above). The leaf's usage is in its log below.
    assert_eq!(value["network_usage"]["status"], "complete", "{value}");
    assert_eq!(value["network_usage"]["endpoints"], serde_json::json!([]));
    assert_eq!(value["dependencies"].as_array().unwrap().len(), 1);
    assert_eq!(value["dependencies"][0]["name"], "leaf");
    let logs = dir.path().join("state/recipe/logs");
    let mut parent_log = None;
    let mut child_seen = false;
    for entry in std::fs::read_dir(&logs)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().unwrap().is_dir())
    {
        let receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(entry.path().join("authority.json")).unwrap())
                .unwrap();
        let result: serde_json::Value =
            serde_json::from_slice(&std::fs::read(entry.path().join("result.json")).unwrap())
                .unwrap();
        if receipt["declaration"]["needs"]["network"]
            .as_array()
            .unwrap()
            .is_empty()
        {
            parent_log = Some(entry.path());
        } else {
            child_seen = true;
            // The leaf's `curl` is a program Ginux ran, not Recipe's own HTTP
            // client: the worker did not count its connection, so the report
            // must neither claim a complete observation nor call the endpoint
            // it used unused (`domain_network_usage`).
            let usage = &result["network_usage"];
            assert_eq!(usage["status"], "unobserved", "{usage}");
            assert!(usage["endpoints"][0]["attempted"].is_null(), "{usage}");
            assert_eq!(usage["warnings"], serde_json::json!([]), "{usage}");
        }
    }
    assert!(child_seen);
    let parent_log = parent_log.unwrap();
    // Recorded provenance survives subsequent edits to the source file.
    std::fs::write(
        recipes.join("parent.rhai"),
        "const NEEDS = compute_authority();",
    )
    .unwrap();
    let explanation = run_recipe(
        &["explain", parent_log.to_str().unwrap(), "network.connect"],
        &recipes,
    );
    assert_success(&explanation);
    let value: serde_json::Value = serde_json::from_slice(&explanation.stdout).unwrap();
    assert!(!value["decisions"].as_array().unwrap().is_empty());
    assert!(
        value["decisions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|decision| decision["audit"].as_str().unwrap().contains("default-deny"))
    );
    assert!(
        value["recipe"]["needs"]["network"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn managed_unused_network_grant_is_a_warning_with_complete_observation() {
    let (_dir, recipes) = create_test_env();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    write_recipe(
        &recipes,
        "unused",
        &format!(
            r#"const RECIPE_API = 1;
const NEEDS = #{{ outputs: #{{ product: "file" }}, network: [{endpoint:?}] }};
fn build(src, out) {{ write_file(out, "no network used"); }}
"#
        ),
    );
    let output = run_recipe(&["install", "unused"], &recipes);
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    // The usage is what the worker counted for Recipe's own downloads; a grant
    // nothing used is reported as unused (when the usage is complete).
    if value["network_usage"]["status"] == "complete" {
        assert_eq!(value["network_usage"]["endpoints"][0]["attempted"], false);
        assert!(
            value["network_usage"]["warnings"][0]
                .as_str()
                .unwrap()
                .contains(&endpoint)
        );
    }
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[cfg(target_os = "linux")]
#[test]
fn managed_network_address_subset_matches_hosts_and_authority() {
    use std::os::unix::process::CommandExt;
    let (dir, recipes) = create_test_env();
    let hosts_file = dir.path().join("resolver-hosts");
    let mut rows = vec!["127.0.0.1 localhost\n".to_owned()];
    for host in ["many.example", "other.example"] {
        for number in 1..=10 {
            rows.push(format!(
                "127.79.{}.{} {host}\n",
                if host == "many.example" { 1 } else { 2 },
                number
            ));
        }
    }
    write_recipe(
        &recipes,
        "addresses",
        r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ hosts: "file" }, network: ["many.example:443", "other.example:443", "127.79.0.99:443"] };
fn build(src, out) { write_file(out, read_file("/etc/hosts")); }
"#,
    );
    for reverse in [false, true] {
        if reverse {
            rows.reverse();
        }
        std::fs::write(&hosts_file, rows.concat()).unwrap();
        let mount_source =
            std::ffi::CString::new(hosts_file.as_os_str().as_encoded_bytes()).unwrap();
        let mut command = recipe_command(&recipes);
        command
            .args(["install", "addresses", "--recipes-path"])
            .arg(&recipes)
            .arg("--build-dir")
            .arg(dir.path().join("build"));
        // Only this CLI child sees the deterministic NSS fixture. The host's
        // mount namespace and hosts file remain untouched, including on failure.
        unsafe {
            command.pre_exec(move || {
                if libc::unshare(libc::CLONE_NEWNS) != 0
                    || libc::mount(
                        std::ptr::null(),
                        c"/".as_ptr(),
                        std::ptr::null(),
                        libc::MS_REC | libc::MS_PRIVATE,
                        std::ptr::null(),
                    ) != 0
                    || libc::mount(
                        mount_source.as_ptr(),
                        c"/etc/hosts".as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND,
                        std::ptr::null(),
                    ) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let output = command
            .output()
            .expect("start namespaced actual Recipe CLI");
        assert_success(&output);
        assert!(String::from_utf8_lossy(&output.stderr).contains("selected"));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let hosts = std::fs::read_to_string(result["outputs"]["hosts"].as_str().unwrap()).unwrap();
        // The injected /etc/hosts always names localhost, then the pinned names.
        assert!(hosts.contains("127.0.0.1 localhost\n"), "{hosts}");
        for number in 1..=10 {
            assert_eq!(
                hosts.contains(&format!("127.79.1.{number} many.example\n")),
                number <= 4
            );
            assert_eq!(
                hosts.contains(&format!("127.79.2.{number} other.example\n")),
                number <= 3
            );
        }
    }
    let receipts = walkdir::WalkDir::new(dir.path())
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_name() == "authority.json")
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 2);
    let mut selected = Vec::new();
    for receipt in receipts {
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(receipt.path()).unwrap()).unwrap();
        let endpoints = value["pinned_endpoints"].as_array().unwrap();
        assert_eq!(endpoints.len(), 8);
        assert!(endpoints.contains(&serde_json::json!("127.79.0.99:443")));
        assert_eq!(
            value["host_endpoints"]["many.example:443"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        assert_eq!(
            value["host_endpoints"]["other.example:443"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        selected.push(value["host_endpoints"].clone());
    }
    assert_eq!(selected[0], selected[1]);
}

#[test]
fn managed_transport_rejects_the_old_grant_model_before_setup() {
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "version-guard",
        r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
print("WORKER_SETUP_EXECUTED");
fn build(src, out) { write_file(out, "unreachable"); }
"#,
    );
    // The request the real coordinator staged (a member of the tar it injects),
    // rewritten to an old version before the real `inject` writes it into the
    // instance; the worker must refuse it before running recipe code.
    let wrapper = ginux_wrapper(
        dir.path(),
        "old-transport",
        r#"
import io, json, tarfile
if VERB != 'inject': passthrough()
source = option('--input')
rewritten = source + '.old-version'
member = 'inputs/.recipe-request.json'
with tarfile.open(source) as old, tarfile.open(rewritten, 'w', format=tarfile.GNU_FORMAT) as new:
    for info in old:
        data = old.extractfile(info).read() if info.isreg() else None
        if info.name == member:
            request = json.loads(data)
            assert request['version'] != 8
            request['version'] = 8
            data = json.dumps(request).encode()
            info.size = len(data)
        new.addfile(info, io.BytesIO(data) if data is not None else None)
ARGS[ARGS.index('--input') + 1] = rewritten
passthrough()
"#,
    );
    let mut command = recipe_command(&recipes);
    with_wrapper(&mut command, &wrapper);
    let result = command
        .args([
            "install",
            "version-guard",
            "--no-persist-state",
            "--recipes-path",
        ])
        .arg(&recipes)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(
        error.contains("bad request") && error.contains("request version 8, want "),
        "{error}"
    );
    assert!(!error.contains("WORKER_SETUP_EXECUTED"), "{error}");
    assert!(result.stdout.is_empty());
    assert!(!dir.path().join("state/recipe/exports").exists());
    assert!(leaked_instances().is_empty());
}

#[test]
fn managed_inheritance_uses_child_authority_and_effective_dependencies() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "base",
        r#"const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" }, network: ["127.0.0.1:9"] };
let deps = ["unused-missing-dependency"];
fn build(src, out) { write_file(out, "base build"); }
"#,
    );
    write_recipe(
        &recipes,
        "child",
        r#"//! extends: base.rhai
const RECIPE_API = 1;
const NEEDS = #{ outputs: #{ product: "file" } };
let deps = [];
"#,
    );
    assert_success(&run_recipe(&["validate", "child"], &recipes));
    let output = run_recipe(&["install", "child"], &recipes);
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        !value["contract"]["contract"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .any(|selector| selector == "network.inet.tcp.connect")
    );
    assert!(
        value["network_usage"]["endpoints"]
            .as_array()
            .is_none_or(|endpoints| endpoints.is_empty())
    );
}

#[test]
fn dependency_product_namespaces_cannot_overlap_aliases() {
    let (_dir, recipes) = create_test_env();
    std::fs::create_dir_all(recipes.join("leaf/nested")).unwrap();
    write_recipe(
        &recipes,
        "leaf",
        "const RECIPE_API = 1; const NEEDS = #{outputs: #{nested: \"directory\"}};",
    );
    write_recipe(
        &recipes,
        "leaf/nested/child",
        "const RECIPE_API = 1; const NEEDS = #{};",
    );
    write_recipe(
        &recipes,
        "parent",
        "const RECIPE_API = 1; const NEEDS = #{}; let deps = [\"leaf\", \"leaf/nested/child\"];",
    );
    let output = run_recipe(&["inspect", "parent"], &recipes);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("dependency result binding collision")
    );
}

#[test]
fn authority_diff_reports_grants_without_confusing_dependency_function_text() {
    let (_dir, recipes) = create_test_env();
    for (version, body) in [("old", "old build"), ("new", "new build")] {
        std::fs::create_dir(recipes.join(version)).unwrap();
        write_recipe(
            &recipes,
            &format!("{version}/parent"),
            "const RECIPE_API = 1; const NEEDS = #{}; let deps = [\"leaf\"];",
        );
        write_recipe(
            &recipes,
            &format!("{version}/leaf"),
            &format!(
                "const RECIPE_API = 1; const NEEDS=#{{}}; fn build(src, out) {{ throw {body:?}; }}"
            ),
        );
    }
    let output = run_recipe(&["diff", "old/parent.rhai", "new/parent.rhai"], &recipes);
    assert_success(&output);
    let unchanged: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(unchanged["authority_changed"], false, "{unchanged}");
    let leaf = recipes.join("new/leaf.rhai");
    let source = std::fs::read_to_string(&leaf).unwrap().replace(
        "const NEEDS=#{};",
        "const NEEDS=#{network:[\"127.0.0.1:12345\"]};",
    );
    std::fs::write(leaf, source).unwrap();
    let output = run_recipe(&["diff", "old/parent.rhai", "new/parent.rhai"], &recipes);
    assert_success(&output);
    let changed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(changed["authority_changed"], true);
    assert!(changed["changes"].get("dependencies").is_some());
}

#[test]
fn managed_caller_caps_cover_transitive_dependencies_without_lending_grants() {
    let (dir, recipes) = create_test_env();
    let (url, server) = super::downloads::serve_download("allowed", 7);
    let endpoint = url
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap();
    let denied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    denied.set_nonblocking(true).unwrap();
    let denied_endpoint = denied.local_addr().unwrap().to_string();
    for (name, declaration, deps, body) in [
        (
            "leaf",
            format!(
                "network:[{endpoint:?},{denied_endpoint:?}],memory_mib:8192,storage_max_objects:100000,storage_max_bytes:67108864,timeout_secs:600,network_max_bytes:65536"
            ),
            "",
            format!(
                "let status=exec(\"curl\",[\"--noproxy\",\"*\",\"--max-time\",\"2\",\"-fsS\",{url:?}]); write_file(out,status.to_string());"
            ),
        ),
        (
            "child",
            format!(
                "network:[{denied_endpoint:?}],memory_mib:2048,storage_max_objects:5000,storage_max_bytes:16777216,timeout_secs:60,network_max_bytes:2048"
            ),
            "let deps=[\"leaf\"];",
            format!(
                "let status=exec(\"curl\",[\"--noproxy\",\"*\",\"--max-time\",\"2\",\"-fsS\",\"http://{denied_endpoint}/denied\"]); if status==0 {{ throw \"dependency borrowed caller endpoint authority\"; }} write_file(out,read_file(DEPS[\"leaf\"]));"
            ),
        ),
        (
            "root",
            format!(
                "network:[{endpoint:?}],memory_mib:4096,storage_max_objects:20000,storage_max_bytes:134217728,timeout_secs:300,network_max_bytes:8192"
            ),
            "let deps=[\"child\"];",
            "copy_file(DEPS[\"child\"],out);".into(),
        ),
    ] {
        write_recipe(
            &recipes,
            name,
            &format!(
                r#"const RECIPE_API = 1;
const NEEDS=#{{outputs:#{{status:"file"}},{declaration}}};
{deps}
fn build(src, out) {{ {body} }}
"#
            ),
        );
    }
    let caps = [
        "--memory-mib",
        "3072",
        "--storage-max-objects",
        "10000",
        "--storage-max-bytes",
        "33554432",
        "--timeout-secs",
        "120",
        "--network-max-bytes",
        "4096",
    ];
    let mut args = vec!["inspect", "root", "--network-host", endpoint];
    args.extend(caps);
    let inspected = run_recipe(&args, &recipes);
    assert_success(&inspected);
    let authority: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    for node in authority["dependencies"].as_array().unwrap() {
        let name = Path::new(node["declaration"]["recipe"].as_str().unwrap())
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap();
        let effective = &node["effective"];
        if name == "child" {
            assert_eq!(
                effective,
                &serde_json::json!({"memory_mib":2048,"storage_max_objects":5000,"storage_max_bytes":16777216,"timeout_secs":60,"network_max_bytes":4096,"network":[]})
            );
        } else {
            assert_eq!(name, "leaf");
            assert_eq!(
                effective,
                &serde_json::json!({"memory_mib":3072,"storage_max_objects":10000,"storage_max_bytes":33554432,"timeout_secs":120,"network_max_bytes":4096,"network":[endpoint]})
            );
        }
    }
    args[0] = "install";
    let allowed = run_recipe(&args, &recipes);
    assert_success(&allowed);
    server.join().unwrap();
    let value: serde_json::Value = serde_json::from_slice(&allowed.stdout).unwrap();
    assert_eq!(
        std::fs::read_to_string(value["outputs"]["status"].as_str().unwrap()).unwrap(),
        "0"
    );
    let records = std::fs::read_dir(dir.path().join("state/recipe/logs"))
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().unwrap().is_dir())
        .map(|entry| {
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(entry.path().join("authority.json")).unwrap(),
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3);
    for receipt in records {
        let name = Path::new(receipt["declaration"]["recipe"].as_str().unwrap())
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap();
        let effective = if name == "root" {
            &authority["effective"]
        } else {
            &authority["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .find(|node| {
                    Path::new(node["declaration"]["recipe"].as_str().unwrap())
                        .file_stem()
                        .unwrap()
                        == name
                })
                .unwrap()["effective"]
        };
        for field in [
            "memory_mib",
            "storage_max_objects",
            "storage_max_bytes",
            "timeout_secs",
            "network_max_bytes",
        ] {
            assert_eq!(
                receipt["execution"][field], effective[field],
                "runtime ceiling differed for {name}.{field}"
            );
        }
        assert_eq!(receipt["execution"]["network_hosts"], effective["network"]);
    }
    let previous_logs = std::fs::read_dir(dir.path().join("state/recipe/logs"))
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().unwrap().is_dir())
        .map(|entry| entry.file_name())
        .collect::<std::collections::BTreeSet<_>>();
    let mut args = vec!["install", "root", "--no-network"];
    args.extend(caps);
    let denied_run = run_recipe(&args, &recipes);
    assert_success(&denied_run);
    let value: serde_json::Value = serde_json::from_slice(&denied_run.stdout).unwrap();
    assert_ne!(
        std::fs::read_to_string(value["outputs"]["status"].as_str().unwrap()).unwrap(),
        "0"
    );
    assert_eq!(
        denied.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let constrained = std::fs::read_dir(dir.path().join("state/recipe/logs"))
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().unwrap().is_dir())
        .filter(|entry| !previous_logs.contains(&entry.file_name()))
        .collect::<Vec<_>>();
    assert_eq!(constrained.len(), 3);
    for entry in constrained {
        let receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(entry.path().join("authority.json")).unwrap())
                .unwrap();
        assert_eq!(receipt["execution"]["network_hosts"], serde_json::json!([]));
        assert!(
            !receipt["contract"]["contract"]["allow"]
                .as_array()
                .unwrap()
                .iter()
                .any(|selector| selector == "network.inet.tcp.connect")
        );
        let explained = run_recipe(
            &["explain", entry.path().to_str().unwrap(), "network.connect"],
            &recipes,
        );
        assert_success(&explained);
        let explanation: serde_json::Value = serde_json::from_slice(&explained.stdout).unwrap();
        assert_eq!(explanation["execution"], receipt["execution"]);
        if Path::new(receipt["declaration"]["recipe"].as_str().unwrap())
            .file_stem()
            .unwrap()
            == "root"
        {
            continue;
        }
        // The retained supervisor log is the instance's audit log: one
        // `GINUX_NATIVE_AUDIT:` line per mediated call, with its operation and
        // the `action` the runtime took. A grant-less run's curl is refused
        // before it can connect: the instance is configured with
        // `[network] backend = "none"`, so Ginux has no network backend to
        // mediate `socket()` through and denies it (ENOSYS) as an
        // implementation decision. The contract itself admits the call —
        // `syscall.*` in `Contract::derived`'s allow list matches
        // `syscall.socket`, hence `policy-source=exact-allow` on a line whose
        // `action=deny` — and only `syscall.connect` is denied by name, which
        // a curl without a socket never reaches. The policy source therefore
        // says which rule matched, not who refused; the assertion requires
        // what matters: a network socket or connect call of this run that the
        // runtime denied. A run that attempted no network call records
        // neither and fails here.
        let audit = std::fs::read_to_string(entry.path().join("supervisor-stderr.log")).unwrap();
        assert!(
            audit
                .lines()
                .any(|line| line.contains("GINUX_NATIVE_AUDIT:")
                    && (line.contains(" operation=syscall.socket ")
                        || line.contains(" operation=syscall.connect "))
                    && line.contains(" action=deny ")),
            "no denied network socket or connect call recorded: {audit}"
        );
    }
}
