//! NEEDS through the real declaration CLI and configured Ginux runtime.
use super::*;

#[test]
fn declaration_commands_never_evaluate_setup_and_reject_widening() {
    let (dir, recipes) = create_test_env();
    let marker = dir.path().join("setup-effect");
    let source = format!(
        r#"const RECIPE_API = 2;
const NEEDS = #{{ inputs: #{{ required: "file", optional: "directory?" }},
    outputs: #{{ product: "file" }}, network: ["127.0.0.1:12345"],
    memory_mib: 4096, storage_max_objects: 20000, timeout_secs: 120, network_max_bytes: 65536 }};
write_file({marker:?}, "SETUP_EXECUTED"); throw "SETUP_EXECUTED";
let ctx = #{{name:"static-needs"}};
"#,
        marker = marker.to_str().unwrap()
    );
    write_recipe(&recipes, "new", &source);
    write_recipe(
        &recipes,
        "old",
        "const RECIPE_API = 2;\nconst NEEDS = #{};\nthrow \"SETUP_EXECUTED\";\nlet ctx = #{name:\"old\"};",
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
            vec!["--network-max-bytes", "65537"],
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
    assert_eq!(value["contract"]["catalog"], 1002);
    assert_eq!(
        value["contract"]["contract"]["connect"],
        serde_json::json!([])
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
            &format!(
                "const RECIPE_API = 2;\n{declaration}\nprint(\"SETUP_EXECUTED\");\nlet ctx = #{{name:\"invalid\"}};"
            ),
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
            "write_file(\"/out/product\",\"typed bytes\"); mkdir(\"/out/tree\"); ctx.outputs = #{product:\"/out/product\",tree:\"/out/tree\"};",
            true,
        ),
        ("missing", "#{product:\"file\"}", "", false),
        (
            "wrong-kind",
            "#{product:\"file\"}",
            "mkdir(\"/out/product\"); ctx.outputs.product=\"/out/product\";",
            false,
        ),
        (
            "undeclared",
            "#{}",
            "write_file(\"/out/product\",\"bytes\"); ctx.outputs.product=\"/out/product\";",
            false,
        ),
    ] {
        write_recipe(
            &recipes,
            name,
            &format!(
                r#"const RECIPE_API = 2;
const NEEDS = #{{outputs:{outputs}}};
let ctx = #{{name:{name:?},state:#{{done:false}},contract:"recipe-owned contract fact",network_usage:"recipe-owned usage fact"}};
fn is_acquired(ctx) {{ ready(ctx) }}
fn is_built(ctx) {{ ready(ctx) }}
fn is_installed(ctx) {{ if ctx.state.done {{ return ready(ctx); }} needs_work(ctx,"install") }}
fn install(ctx) {{ {body} ctx.state.done=true; ctx }}
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
            assert_eq!(value["runtime"]["contract"]["catalog"], 1002);
            assert_eq!(value["contract"], "recipe-owned contract fact");
            assert_eq!(value["network_usage"], "recipe-owned usage fact");
        } else {
            assert!(String::from_utf8_lossy(&result.stderr).contains("NEEDS.outputs"));
            assert!(result.stdout.is_empty());
        }
    }
    let published = std::fs::read_dir(dir.path().join(".artifacts/ginux-export"))
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
        &format!(
            r#"const RECIPE_API = 2;
const NEEDS = #{{ inputs: #{{ asset: "file" }}, outputs: #{{ proof: "directory", alias: "file" }}, network: [{endpoint:?}] }};
let ctx = #{{name:"authority-leaf",state:#{{done:false}}}};
fn is_acquired(ctx) {{ ready(ctx) }}
fn is_built(ctx) {{ ready(ctx) }}
fn is_installed(ctx) {{ if ctx.state.done {{ ctx.outputs.proof="/out/proof"; ctx.outputs.alias="/out/proof/asset"; return ready(ctx); }} needs_work(ctx,"produce") }}
fn install(ctx) {{
    mkdir("/out/proof");
    write_file("/out/proof/payload",exec_output("curl",["--noproxy","*","-fsS",{url:?}]));
    copy_file("/inputs/asset","/out/proof/asset");
    ln("/usr/bin/true", "/out/proof/absolute");
    ctx.state.done=true; ctx.outputs.proof="/out/proof"; ctx.outputs.alias="/out/proof/asset"; ctx
}}
"#
        ),
    );
    write_recipe(
        &recipes,
        "parent",
        &format!(
            r#"const RECIPE_API = 2;
const NEEDS = #{{}};
let deps=["leaf"];
let ctx=#{{name:"authority-parent",state:#{{done:false}}}};
fn is_acquired(ctx) {{ ready(ctx) }}
fn is_built(ctx) {{ ready(ctx) }}
fn is_installed(ctx) {{ if ctx.state.done {{ return ready(ctx); }} needs_work(ctx,"consume") }}
fn install(ctx) {{
    let proof=join_path(RUNTIME.paths.dependencies,"leaf/proof");
    ctx.state.payload=read_file(join_path(proof,"payload"));
    ctx.state.asset=read_file(join_path(proof,"asset"));
    let alias=join_path(RUNTIME.paths.dependencies,"leaf/alias");
    if exec_output("stat",["-c","%i",alias]) != exec_output("stat",["-c","%i",join_path(proof,"asset")]) {{ throw "dependency transfer split a published hardlink"; }}
    if trim(exec_output("readlink",[join_path(proof,"absolute")])) != "/usr/bin/true" {{ throw "dependency rootfs link changed"; }}
    if is_file("/inputs/asset") {{ throw "dependency input leaked into parent"; }}
    ctx.state.denied=exec("curl",["--noproxy","*","--connect-timeout","1","--max-time","2","-fsS",{url:?}]);
    if ctx.state.denied==0 {{ throw "parent borrowed dependency network authority"; }}
    ctx.state.done=true; ctx
}}
"#
        ),
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
    assert_eq!(value["state"]["payload"], "dependency network product");
    assert_eq!(value["state"]["asset"], "explicit child binding");
    assert_ne!(value["state"]["denied"], 0);
    assert_eq!(value["runtime"]["network_usage"]["status"], "complete");
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
            assert_eq!(
                result["worker"]["ctx_summary"]["runtime"]["network_usage"]["endpoints"][0]["attempted"],
                true
            );
            assert_eq!(
                result["worker"]["ctx_summary"]["runtime"]["network_usage"]["warnings"],
                serde_json::json!([])
            );
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
            .any(|decision| decision["audit"]
                .as_str()
                .unwrap()
                .contains("policy-source=default-deny"))
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
            r#"const RECIPE_API=2;
const NEEDS=#{{network:[{endpoint:?}]}};
let ctx=#{{name:"unused-grant"}};
fn is_installed(ctx) {{ ready(ctx) }}
"#
        ),
    );
    let output = run_recipe(&["install", "unused"], &recipes);
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["runtime"]["network_usage"]["status"], "complete");
    assert_eq!(
        value["runtime"]["network_usage"]["endpoints"][0]["attempted"],
        false
    );
    assert!(
        value["runtime"]["network_usage"]["warnings"][0]
            .as_str()
            .unwrap()
            .contains(&endpoint)
    );
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
        "pins",
        r#"const RECIPE_API=2;
const NEEDS=#{network:["many.example:443","other.example:443","127.79.0.99:443"]};
let ctx=#{name:"bounded-network-pins",state:#{hosts:""}};
fn is_installed(ctx) { ctx.state.hosts=read_file("/etc/hosts"); ready(ctx) }
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
            .args(["install", "pins", "--recipes-path"])
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
        let hosts = result["state"]["hosts"].as_str().unwrap();
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
    use std::os::unix::fs::PermissionsExt;
    let (dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "version-guard",
        r#"const RECIPE_API=2;
const NEEDS=#{};
print("WORKER_SETUP_EXECUTED");
let ctx=#{name:"version-guard"};
fn is_installed(ctx) { ready(ctx) }
"#,
    );
    let launcher = std::env::var("GINUX_BIN").unwrap();
    let wrapper = dir.path().join("old-transport");
    std::fs::write(
        &wrapper,
        format!(
            r#"#!/usr/bin/python3
import os, struct, subprocess, sys
launcher={launcher:?}
if sys.argv[1] != 'launch': os.execv(launcher,[launcher]+sys.argv[1:])
def read_exact(count):
 data=bytearray()
 while len(data)<count:
  chunk=sys.stdin.buffer.read(count-len(data))
  if not chunk: raise RuntimeError('incomplete real coordinator request')
  data.extend(chunk)
 return data
header=read_exact(4)
length=struct.unpack('>I',header)[0]
assert 14<=length<=4*1024*1024
request=header+read_exact(length-4)
request[4:8]=struct.pack('>I',8)
child=subprocess.Popen([launcher]+sys.argv[1:],stdin=subprocess.PIPE,close_fds=False)
child.communicate(request,timeout=30)
sys.exit(child.returncode)
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let result = recipe_command(&recipes)
        .env("GINUX_BIN", wrapper)
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
        error.contains("protocol version mismatch") && error.contains("want 9"),
        "{error}"
    );
    assert!(!error.contains("WORKER_SETUP_EXECUTED"), "{error}");
    assert!(result.stdout.is_empty());
    assert!(!dir.path().join(".artifacts/ginux-export").exists());
}

#[test]
fn managed_inheritance_uses_child_authority_and_effective_dependencies() {
    let (_dir, recipes) = create_test_env();
    write_recipe(
        &recipes,
        "base",
        r#"const RECIPE_API=2;
const NEEDS=#{network:["127.0.0.1:9"]};
let deps=["unused-missing-dependency"];
let ctx=#{name:"base-authority"};
fn is_installed(ctx) { ready(ctx) }
"#,
    );
    write_recipe(
        &recipes,
        "child",
        r#"//! extends: base.rhai
const NEEDS=#{};
let deps=[];
"#,
    );
    assert_success(&run_recipe(&["validate", "child"], &recipes));
    let output = run_recipe(&["install", "child"], &recipes);
    assert_success(&output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["runtime"]["contract"]["contract"]["connect"],
        serde_json::json!([])
    );
    assert_eq!(
        value["runtime"]["network_usage"]["endpoints"],
        serde_json::json!([])
    );
}

#[test]
fn dependency_product_namespaces_cannot_overlap_aliases() {
    let (_dir, recipes) = create_test_env();
    std::fs::create_dir_all(recipes.join("leaf/nested")).unwrap();
    write_recipe(
        &recipes,
        "leaf",
        "const RECIPE_API=2; const NEEDS=#{outputs:#{nested:\"directory\"}}; let ctx=#{name:\"leaf\"};",
    );
    write_recipe(
        &recipes,
        "leaf/nested/child",
        "const RECIPE_API=2; const NEEDS=#{}; let ctx=#{name:\"child\"};",
    );
    write_recipe(
        &recipes,
        "parent",
        "const RECIPE_API=2; const NEEDS=#{}; let deps=[\"leaf\",\"leaf/nested/child\"]; let ctx=#{name:\"parent\"};",
    );
    let output = run_recipe(&["inspect", "parent"], &recipes);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("dependency result binding collision")
    );
}

#[test]
fn authority_diff_reports_grants_without_confusing_dependency_hook_text() {
    let (_dir, recipes) = create_test_env();
    for (version, body) in [("old", "old hook"), ("new", "new hook")] {
        std::fs::create_dir(recipes.join(version)).unwrap();
        write_recipe(
            &recipes,
            &format!("{version}/parent"),
            "const RECIPE_API=2; const NEEDS=#{}; let deps=[\"leaf\"]; let ctx=#{name:\"parent\"};",
        );
        write_recipe(
            &recipes,
            &format!("{version}/leaf"),
            &format!(
                "const RECIPE_API=2; const NEEDS=#{{}}; let ctx=#{{name:\"leaf\"}}; fn install(ctx) {{ throw {body:?}; }}"
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
                "let status=exec(\"curl\",[\"--noproxy\",\"*\",\"--max-time\",\"2\",\"-fsS\",{url:?}]); write_file(\"/out/status\",status.to_string());"
            ),
        ),
        (
            "child",
            format!(
                "network:[{denied_endpoint:?}],memory_mib:2048,storage_max_objects:5000,storage_max_bytes:16777216,timeout_secs:60,network_max_bytes:2048"
            ),
            "let deps=[\"leaf\"];",
            format!(
                "let status=exec(\"curl\",[\"--noproxy\",\"*\",\"--max-time\",\"2\",\"-fsS\",\"http://{denied_endpoint}/denied\"]); if status==0 {{ throw \"dependency borrowed caller endpoint authority\"; }} write_file(\"/out/status\",read_file(join_path(RUNTIME.paths.dependencies,\"leaf/status\")));"
            ),
        ),
        (
            "root",
            format!(
                "network:[{endpoint:?}],memory_mib:4096,storage_max_objects:20000,storage_max_bytes:134217728,timeout_secs:300,network_max_bytes:8192"
            ),
            "let deps=[\"child\"];",
            "copy_file(join_path(RUNTIME.paths.dependencies,\"child/status\"),\"/out/status\");"
                .into(),
        ),
    ] {
        write_recipe(
            &recipes,
            name,
            &format!(
                r#"const RECIPE_API=2;
const NEEDS=#{{outputs:#{{status:"file"}},{declaration}}};
{deps}
let ctx=#{{name:{name:?}}};
fn is_acquired(ctx) {{ ready(ctx) }}
fn is_built(ctx) {{ ready(ctx) }}
fn is_installed(ctx) {{ if is_file("/out/status") {{ ctx.outputs.status="/out/status"; return ready(ctx); }} needs_work(ctx,"exercise this invocation's effective authority") }}
fn install(ctx) {{ {body} ctx.outputs.status="/out/status"; ctx }}
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
                &serde_json::json!({"memory_mib":2048,"storage_max_objects":5000,"storage_max_bytes":16777216,"timeout_secs":60,"network_max_bytes":2048,"network":[]})
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
        assert_eq!(
            receipt["contract"]["contract"]["connect"],
            serde_json::json!([])
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
        let audit = std::fs::read_to_string(entry.path().join("supervisor-stderr.log")).unwrap();
        assert!(
            audit
                .lines()
                .any(|line| line.contains("policy-source=default-deny")
                    && line
                        .split_whitespace()
                        .any(|field| matches!(field, "syscall=41" | "first-syscall=41"))),
            "no actual socket denial recorded: {audit}"
        );
    }
}

#[test]
fn managed_phase_outputs_allow_typed_subsets_and_require_complete_installed_reuse() {
    let (dir, recipes) = create_test_env();
    let source = r#"const RECIPE_API=2;
const NEEDS=#{outputs:#{source:"file",product:"file"}};
let ctx=#{name:"phase-products"};
fn is_acquired(ctx) { if is_file("/work/source") { return ready(ctx); } needs_work(ctx,"acquire") }
fn is_built(ctx) { if is_file("/work/built") { return ready(ctx); } needs_work(ctx,"build") }
fn is_installed(ctx) {
    if !is_file("/out/product") { return needs_work(ctx,"install"); }
    ctx.outputs=#{source:"/work/source",product:"/out/product"}; ready(ctx)
}
fn acquire(ctx) { write_file("/work/source","source"); ctx.outputs.source="/work/source"; ctx }
fn build(ctx) { copy_file("/work/source","/work/built"); ctx }
fn install(ctx) { copy_file("/work/built","/out/product"); ctx.outputs.product="/out/product"; ctx }
"#;
    write_recipe(&recipes, "phases", source);
    for _ in 0..2 {
        let output = run_recipe(&["install", "phases"], &recipes);
        assert_success(&output);
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["outputs"].as_object().unwrap().len(), 2);
        assert_eq!(
            std::fs::read_to_string(value["outputs"]["product"].as_str().unwrap()).unwrap(),
            "source"
        );
    }
    // A manual installed check which omits a declared product cannot publish.
    write_recipe(
        &recipes,
        "missing-reuse",
        r#"const RECIPE_API=2;
const NEEDS=#{outputs:#{source:"file",product:"file"}};
let ctx=#{name:"missing-reuse"};
fn is_installed(ctx) { write_file("/out/product","present"); ctx.outputs.product="/out/product"; ready(ctx) }
"#,
    );
    let output = run_recipe(&["isinstalled", "missing-reuse"], &recipes);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("missing required NEEDS.outputs.source")
    );
    let invalid = source
        .replace(
            "write_file(\"/work/source\",\"source\");",
            "mkdir(\"/work/source\");",
        )
        .replace(
            "if is_file(\"/work/source\")",
            "if is_dir(\"/work/source\")",
        );
    write_recipe(&recipes, "bad-intermediate", &invalid);
    let output = run_recipe(&["install", "bad-intermediate"], &recipes);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("NEEDS.outputs.source requires file"));
    let logs = dir.path().join("state/recipe/logs");
    let result = std::fs::read_dir(logs)
        .unwrap()
        .map(Result::unwrap)
        .filter_map(|entry| std::fs::read(entry.path().join("result.json")).ok())
        .map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).unwrap())
        .find(|value| {
            value
                .to_string()
                .contains("NEEDS.outputs.source requires file")
        })
        .unwrap();
    assert_eq!(result["checkpoints"], serde_json::json!([]));
}
