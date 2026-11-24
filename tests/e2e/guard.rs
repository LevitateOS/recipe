//! `recipe-ginux-guard`, the root-owned program the sudoers rule names.
//!
//! Preconditions: none. The cases need no root, no sudo, no Ginux, no network
//! and no installed helper:
//!
//! - the config cases run the REAL `recipe install --execution ginux` up to
//!   the point where it hands its rendered instance config to `ginux validate
//!   FILE`; a stand-in `ginux` CLI (a shell script named by `RECIPE_GINUX`)
//!   answers the declaration-only contract check before it with the pinned
//!   digest, keeps a copy of the run's config and fails, so the text under
//!   test is exactly what Recipe imports. They then run the guard's dry run
//!   `recipe-ginux-guard --check-config FILE` (exit 0 accepts, 65 refuses)
//!   on it and on mutated copies;
//! - the image allowlist cases run `--check-config --image-allowlist LIST
//!   FILE` on Recipe's config with allowlist fixtures in the test's temporary
//!   directory. The guard trusts an allowlist only when it and every parent
//!   directory are root-owned and closed to group and others, so for a test
//!   run as an ordinary user (or with a temporary directory under the
//!   world-writable `/tmp`) every fixture is refused as an untrusted
//!   allowlist; the "listed is accepted, not listed is refused" outcomes are
//!   checked only when the test runs as root with `RECIPE_TEST_ARTIFACTS` (or
//!   `TMPDIR`) in a root-owned directory chain nobody else can write. A
//!   symbolic link, a world-writable file and a missing file are refused for
//!   every runner;
//! - the verb cases run the guard with a fake `SUDO_UID`/`SUDO_GID`, the
//!   fixed protocol and roots, and a request it must refuse before it touches
//!   anything (exit 64; the guard checks the request before it checks that it
//!   runs as root). A request in an allowed shape, run without root, stops at
//!   the root check (exit 77) and never reaches the helper; that case is
//!   skipped when the test itself runs as root. `import-image` is refused for
//!   every caller, root included. The real helper at
//!   `/usr/libexec/ginux/ginux-helper` need not exist.
//!
//! `RECIPE_TEST_GUARD_BIN` selects another guard binary.
use super::*;
use std::os::unix::fs::PermissionsExt;

const REFUSED_REQUEST: i32 = 64;
const REFUSED_CONFIG: i32 = 65;
const NOT_ROOT: i32 = 77;
const MANIFEST: &str =
    "local/recipe-compiler@sha256:0000000000000000000000000000000000000000000000000000000000000001";

fn guard_bin() -> std::path::PathBuf {
    std::env::var_os("RECIPE_TEST_GUARD_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_recipe-ginux-guard").into())
}

/// The config text Recipe renders for a recipe that starts a program, as the
/// real binary hands it to `ginux validate` for the run.
///
/// Recipe validates two configs: first the declaration-only contract of each
/// recipe (`instance.id = "recipe-contract"`, `contract::inspection_config`),
/// then the run's own config. The stand-in answers the first as Ginux would,
/// with the build-base digest Recipe pins, and keeps a copy of the second.
/// The pinned digest is read from Recipe itself: a first run answers with a
/// digest no pin has and Recipe's refusal names the one it expects.
fn rendered_config(dir: &Path, recipes: &Path) -> String {
    write_recipe(
        recipes,
        "guarded",
        "const RECIPE_API = 1;\nconst NEEDS = #{};\nfn build(src, out) {}\n",
    );
    let capture = dir.join("captured-config.toml");
    let cli = dir.join("ginux-capture");
    std::fs::write(
        &cli,
        "#!/bin/sh\n\
         while [ \"$#\" -gt 0 ]; do\n\
         \tcase \"$1\" in --state-root|--run-root) shift 2 ;; *) break ;; esac\n\
         done\n\
         if [ \"$1\" = validate ] && grep -qx 'id = \"recipe-contract\"' \"$2\"; then\n\
         \techo \"Contract valid: digest=$GUARD_TEST_DIGEST\"\n\
         \texit 0\n\
         fi\n\
         if [ \"$1\" = validate ] && [ ! -e \"$GUARD_TEST_CAPTURE\" ]; then cp \"$2\" \"$GUARD_TEST_CAPTURE\"; fi\n\
         echo 'stand-in ginux: refusing every verb' >&2\n\
         exit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    let run = |digest: &str| {
        let output = host_command(recipes)
            .env("RECIPE_GINUX", &cli)
            .env("RECIPE_IMAGE", MANIFEST)
            .env_remove("RECIPE_IMAGE_LAYOUT")
            .env_remove("RECIPE_MINIMAL_IMAGE")
            .env("GUARD_TEST_CAPTURE", &capture)
            .env("GUARD_TEST_DIGEST", digest)
            .args([
                "install",
                "guarded",
                "--execution",
                "ginux",
                "--recipes-path",
            ])
            .arg(recipes)
            .output()
            .unwrap();
        // The stand-in refuses the run: that is how the test stops before any instance.
        assert!(!output.status.success());
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    let unknown = "0".repeat(64);
    let stderr = run(&unknown);
    let pinned = stderr
        .split_once("build-base digest changed: expected ")
        .and_then(|(_, rest)| rest.split_once(&format!(", got {unknown}")))
        .map(|(expected, _)| expected.to_owned())
        .unwrap_or_else(|| {
            panic!("Recipe did not name its pinned build-base digest; stderr:\n{stderr}")
        });
    assert!(
        !capture.exists(),
        "Recipe validated a run config with an unpinned contract"
    );
    let stderr = run(&pinned);
    std::fs::read_to_string(&capture).unwrap_or_else(|error| {
        panic!("Recipe never passed a run config to `ginux validate` ({error}); stderr:\n{stderr}")
    })
}

/// `--check-config` on `text`; the exit code.
fn check_config(dir: &Path, name: &str, text: &str) -> (i32, String) {
    let file = dir.join(name);
    std::fs::write(&file, text).unwrap();
    let output = Command::new(guard_bin())
        .arg("--check-config")
        .arg(&file)
        .env_clear()
        .output()
        .expect("cannot run recipe-ginux-guard");
    (
        output
            .status
            .code()
            .expect("the guard was killed by a signal"),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// `text` with `from` (which must occur exactly once) replaced by `to`, so a
/// change of the rendered shape fails loudly here, not silently as a pass.
fn mutate(text: &str, from: &str, to: &str) -> String {
    assert_eq!(
        text.matches(from).count(),
        1,
        "{from:?} is not in the rendered config once:\n{text}"
    );
    text.replacen(from, to, 1)
}

/// `text` with the value line `key = ...` of `section` replaced by `line`.
fn set_line(text: &str, section: &str, key: &str, line: &str) -> String {
    let mut current = "";
    let mut done = false;
    let mut out = Vec::new();
    for original in text.lines() {
        if original.starts_with('[') {
            current = original.trim();
        }
        if current == section && original.starts_with(&format!("{key} =")) {
            out.push(line.to_owned());
            done = true;
        } else {
            out.push(original.to_owned());
        }
    }
    assert!(
        done,
        "{section} {key} is not in the rendered config:\n{text}"
    );
    out.join("\n") + "\n"
}

#[test]
fn the_guard_accepts_the_config_recipe_renders_and_refuses_each_widening_of_it() {
    let (dir, recipes) = create_test_env();
    let config = rendered_config(dir.path(), &recipes);
    assert!(config.contains("[instance]"), "{config}");
    // `instance.id` is the invocation id `<recipe>-<unix millis>-<pid>`.
    let id = config
        .lines()
        .find_map(|line| line.strip_prefix("id = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or_else(|| panic!("no instance.id in the rendered config:\n{config}"));
    let mut parts = id.rsplitn(3, '-');
    let (pid, millis, stem) = (parts.next(), parts.next(), parts.next());
    assert!(
        stem == Some("guarded")
            && [pid, millis].iter().all(|part| part
                .is_some_and(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))),
        "instance.id {id:?} is not the invocation id guarded-<millis>-<pid>"
    );

    let (code, stderr) = check_config(dir.path(), "accepted.toml", &config);
    assert_eq!(code, 0, "the guard refused Recipe's own config: {stderr}");

    // The same config with an outbound grant, as `InstanceConfig::render`
    // writes it for NEEDS.network (backend shared, the network contract,
    // 1 to 8 ascending ports).
    let shared = |outbound: &str| -> String {
        let text = mutate(
            &config,
            "\"memory.virtual.manage\"]",
            "\"memory.virtual.manage\",\"network.inet.tcp.connect\"]",
        );
        let text = mutate(
            &text,
            ",\"syscall.connect\",\"syscall.*msg\",\"syscall.shutdown\",\"syscall.getsockname\",\"syscall.getpeername\",\"syscall.*sockopt\"",
            "",
        );
        let text = mutate(&text, "backend = \"none\"", "backend = \"shared\"");
        mutate(
            &text,
            "\n[storage]\n",
            &format!(
                "\n[contract.network]\ndefault = \"deny\"\noutbound = {outbound}\n\n[storage]\n"
            ),
        )
    };
    for (name, outbound) in [
        ("one-port", "[\"tcp:443\"]"),
        (
            "eight-ports",
            "[\"tcp:1\",\"tcp:21\",\"tcp:22\",\"tcp:80\",\"tcp:443\",\"tcp:873\",\"tcp:8080\",\"tcp:65535\"]",
        ),
    ] {
        let (code, stderr) = check_config(dir.path(), &format!("{name}.toml"), &shared(outbound));
        assert_eq!(
            code, 0,
            "the guard refused a networked config ({name}): {stderr}"
        );
    }

    let network = mutate(
        &config,
        "\n[network]\n",
        "\n[network]\nport_forwards = [\"tcp:8080:80\"]\n",
    );
    let hook = format!("{config}\n[hooks]\npre_boot = [\"/bin/true\"]\n");
    let foreign_init = set_line(&config, "[instance]", "init", "init = \"/bin/sh\"");
    let other_input = set_line(
        &config,
        "[instance]",
        "init",
        "init = \"/inputs/not-the-worker\"",
    );
    let unknown_key = mutate(
        &config,
        "\n[resources]\n",
        "\nsurprise = 1\n\n[resources]\n",
    );
    let tagged_image = set_line(
        &config,
        "[instance]",
        "root",
        "root = \"local/recipe-compiler:latest\"",
    );
    let bare_image = set_line(
        &config,
        "[instance]",
        "root",
        "root = \"local/recipe-compiler\"",
    );
    let short_digest = set_line(
        &config,
        "[instance]",
        "root",
        "root = \"local/recipe-compiler@sha256:00\"",
    );
    let virtual_root = set_line(&config, "[access]", "uid", "uid = 0");
    let path_id = set_line(&config, "[instance]", "id", "id = \"../escape\"");
    let preload = mutate(
        &config,
        "environment = [",
        "environment = [\"LD_PRELOAD=/work/x.so\", ",
    );
    let extra_args = mutate(
        &config,
        "\"/inputs/main.rhai\"]",
        "\"/inputs/main.rhai\", \"--host\"]",
    );
    let vcpus = set_line(&config, "[resources]", "vcpus", "vcpus = 17");
    let memory = set_line(&config, "[resources]", "memory_mib", "memory_mib = 15");
    let other_backend = set_line(&config, "[execution]", "backend", "backend = \"kvm\"");
    let allow_default = set_line(&config, "[contract]", "default", "default = \"allow\"");
    let other_catalog = set_line(&config, "[contract]", "catalog", "catalog = 1");
    let write_root = set_line(&config, "[contract.filesystem]", "write", "write = [\"/\"]");
    let no_input_deny = mutate(&config, "deny = [\"/inputs\"]\n", "");
    let other_deny = set_line(
        &config,
        "[contract.filesystem]",
        "deny",
        "deny = [\"/inputs/main.rhai\"]",
    );
    let storage = set_line(
        &config,
        "[storage]",
        "assignments",
        "assignments = [\"/var/lib\"]",
    );
    let device = set_line(
        &config,
        "[devices]",
        "assignments",
        "assignments = [\"null\", \"random\", \"urandom\", \"kvm\"]",
    );
    let no_audit = set_line(&config, "[audit]", "enabled", "enabled = false");
    let keep_running = set_line(&config, "[failure]", "policy", "policy = \"restart\"");
    let shared_without_grant = mutate(&config, "backend = \"none\"", "backend = \"shared\"");
    let grant_offline = mutate(
        &config,
        "\n[storage]\n",
        "\n[contract.network]\ndefault = \"deny\"\noutbound = [\"tcp:443\"]\n\n[storage]\n",
    );
    let nine_ports = shared(
        "[\"tcp:1\",\"tcp:2\",\"tcp:3\",\"tcp:4\",\"tcp:5\",\"tcp:6\",\"tcp:7\",\"tcp:8\",\"tcp:9\"]",
    );
    let no_ports = shared("[]");
    let port_zero = shared("[\"tcp:0\"]");
    let unsorted = shared("[\"tcp:443\",\"tcp:80\"]");
    let repeated = shared("[\"tcp:443\",\"tcp:443\"]");
    let udp = shared("[\"udp:53\"]");
    for (name, text) in [
        ("forward", &network),
        ("hook", &hook),
        ("init-binary", &foreign_init),
        ("init-other-input", &other_input),
        ("unknown-key", &unknown_key),
        ("image-tag", &tagged_image),
        ("image-bare", &bare_image),
        ("image-short-digest", &short_digest),
        ("uid-zero", &virtual_root),
        ("id-path", &path_id),
        ("env-preload", &preload),
        ("extra-args", &extra_args),
        ("vcpus", &vcpus),
        ("memory", &memory),
        ("backend", &other_backend),
        ("default-allow", &allow_default),
        ("catalog", &other_catalog),
        ("write-root", &write_root),
        ("no-input-deny", &no_input_deny),
        ("other-deny", &other_deny),
        ("storage", &storage),
        ("device", &device),
        ("audit-off", &no_audit),
        ("failure-policy", &keep_running),
        ("shared-without-grant", &shared_without_grant),
        ("grant-offline", &grant_offline),
        ("nine-ports", &nine_ports),
        ("no-ports", &no_ports),
        ("port-zero", &port_zero),
        ("ports-unsorted", &unsorted),
        ("ports-repeated", &repeated),
        ("udp", &udp),
    ] {
        let (code, stderr) = check_config(dir.path(), &format!("{name}.toml"), text);
        assert_eq!(
            code, REFUSED_CONFIG,
            "the guard must refuse the config with {name}: {stderr}"
        );
        assert!(
            stderr.contains("refused instance config"),
            "{name}: the refusal says why: {stderr}"
        );
    }
}

/// The guard run with sudo's caller ids `4242:4242`, Recipe's protocol and
/// roots (or `roots`), and `request`.
fn run_guard(roots: &[&str], request: &[&str]) -> std::process::Output {
    Command::new(guard_bin())
        .env_clear()
        .env("SUDO_UID", "4242")
        .env("SUDO_GID", "4242")
        .args(roots)
        .args(request)
        .output()
        .expect("cannot run recipe-ginux-guard")
}

const ROOTS: [&str; 6] = [
    "--protocol",
    "1",
    "--state-root",
    "/var/lib/recipe/ginux",
    "--run-root",
    "/run/recipe/ginux",
];

#[test]
fn the_guard_refuses_other_verbs_and_other_instance_names_before_touching_anything() {
    let (dir, _recipes) = create_test_env();
    let marker = dir.path().join("must-not-exist");
    let marker_text = marker.to_str().unwrap().to_owned();
    let bounds = [
        "--max-bytes",
        "1024",
        "--max-members",
        "3",
        "--max-path",
        "4096",
    ];
    let with_bounds = |verb: &'static str, name: &'static str, paths: &[&'static str]| {
        let mut request = vec![verb, name];
        request.extend(bounds);
        request.extend(paths);
        request
    };
    let mut seventeen = with_bounds("extract", "recipe-4242-1-ab", &[]);
    let many: Vec<String> = (0..17).map(|index| format!("/out/{index}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    seventeen.extend(&many);
    for request in [
        // Verbs that are root on any path (they are not Recipe's).
        vec!["prepare-root", "/"],
        vec!["prepare-root", "recipe-4242-1-ab"],
        vec!["run-runtime", "recipe-4242-1-ab"],
        vec!["run-runtime", "/bin/sh"],
        vec!["list"],
        vec!["export", "recipe-4242-1-ab"],
        vec!["authorize-attach", "recipe-4242-1-ab", "4242", "4242"],
        vec!["no-such-verb"],
        vec![],
        // Recipe's verbs in the right shape on instances of another uid, of
        // no uid, and on names that are paths.
        vec![
            "start",
            "recipe-0-1-ab",
            "--wait",
            "1",
            "1",
            "--stream-console",
            "--full-audit",
        ],
        vec![
            "start",
            "recipe-4243-1-ab",
            "--wait",
            "1",
            "1",
            "--stream-console",
            "--full-audit",
        ],
        vec!["delete", "recipe-4243-1-ab"],
        vec!["status", "other-4242-1-ab", "--format", "json"],
        with_bounds("extract", "recipe-42421-1-ab", &["/out"]),
        with_bounds("inject", "recipe-4243-1-ab", &["/"]),
        vec!["import", "recipe-4243-1-ab", "/tmp/config.toml"],
        vec!["delete", "../recipe-4242-1-ab"],
        vec!["delete", "recipe-4242-1-ab/../../x"],
        vec!["stop", "recipe-4242"],
        vec!["stop", "recipe-4242-"],
        vec!["delete", ""],
        // Recipe's verbs on the caller's own instance in shapes Recipe never
        // passes.
        vec![
            "extract",
            "recipe-4242-1-ab",
            "--output",
            &marker_text,
            "/out",
        ],
        with_bounds("extract", "recipe-4242-1-ab", &[]),
        with_bounds("extract", "recipe-4242-1-ab", &["out"]),
        with_bounds("extract", "recipe-4242-1-ab", &["/out/../etc"]),
        seventeen,
        vec![
            "extract",
            "recipe-4242-1-ab",
            "--max-bytes",
            "0",
            "--max-members",
            "3",
            "--max-path",
            "4096",
            "/out",
        ],
        vec![
            "inject",
            "recipe-4242-1-ab",
            "--max-bytes",
            "01",
            "--max-members",
            "3",
            "--max-path",
            "4096",
            "/",
        ],
        with_bounds("inject", "recipe-4242-1-ab", &["/", "/work"]),
        with_bounds("inject", "recipe-4242-1-ab", &["work"]),
        vec!["start", "recipe-4242-1-ab"],
        vec!["start", "recipe-4242-1-ab", "--attach"],
        vec!["start", "recipe-4242-1-ab", "--wait", "01", "1"],
        vec!["start", "recipe-4242-1-ab", "--wait", "1", "1", "--attach"],
        vec![
            "start",
            "recipe-4242-1-ab",
            "--stream-console",
            "--wait",
            "1",
            "1",
        ],
        // Recipe always asks for the whole audit log, after the console.
        vec!["start", "recipe-4242-1-ab", "--wait", "1", "1"],
        vec![
            "start",
            "recipe-4242-1-ab",
            "--wait",
            "1",
            "1",
            "--stream-console",
        ],
        vec![
            "start",
            "recipe-4242-1-ab",
            "--audit",
            "--wait",
            "1",
            "1",
            "--stream-console",
            "--full-audit",
        ],
        vec![
            "start",
            "recipe-4242-1-ab",
            "--wait",
            "1",
            "1",
            "--full-audit",
            "--stream-console",
        ],
        vec![
            "start",
            "recipe-4242-1-ab",
            "--wait",
            "1",
            "1",
            "--stream-console",
            "--full-audit",
            "--full-audit",
        ],
        vec!["status", "recipe-4242-1-ab", "--format", "text"],
        vec!["stop", "recipe-4242-1-ab", "--force"],
        vec!["delete", "recipe-4242-1-ab", "recipe-4242-2-ab"],
        vec!["import", "recipe-4242-1-ab"],
        vec!["import", "recipe-4242-1-ab", ""],
        vec![
            "import",
            "recipe-4242-1-ab",
            "/tmp/config.toml",
            "--set",
            "x=y",
        ],
        vec!["import-image", "/tmp/layout"],
        // Images are applied as root: no caller imports one through the guard.
        vec!["import-image"],
    ] {
        let output = run_guard(&ROOTS, &request);
        assert_eq!(
            output.status.code(),
            Some(REFUSED_REQUEST),
            "the guard must refuse {request:?} with exit {REFUSED_REQUEST}; stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty(), "{request:?} printed to stdout");
    }
    assert!(!marker.exists());

    // `import-image` is refused for root too (no sudo ids: the caller is root).
    for request in [&["import-image"][..], &["import-image", "/tmp/layout"]] {
        let output = Command::new(guard_bin())
            .env_clear()
            .args(ROOTS)
            .args(request)
            .output()
            .expect("cannot run recipe-ginux-guard");
        assert_eq!(
            output.status.code(),
            Some(REFUSED_REQUEST),
            "a root caller's {request:?} must be refused: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Roots and protocol other than Recipe's are refused the same way.
    for roots in [
        [
            "--protocol",
            "1",
            "--state-root",
            "/",
            "--run-root",
            "/run/recipe/ginux",
        ],
        [
            "--protocol",
            "1",
            "--state-root",
            "/var/lib/recipe/ginux",
            "--run-root",
            "/",
        ],
        [
            "--protocol",
            "2",
            "--state-root",
            "/var/lib/recipe/ginux",
            "--run-root",
            "/run/recipe/ginux",
        ],
        [
            "--protocol",
            "1",
            "--run-root",
            "/run/recipe/ginux",
            "--state-root",
            "/var/lib/recipe/ginux",
        ],
    ] {
        let output = run_guard(&roots, &["delete", "recipe-4242-1-ab"]);
        assert_eq!(
            output.status.code(),
            Some(REFUSED_REQUEST),
            "the guard must refuse {roots:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn the_guard_lets_recipes_requests_through_only_as_root() {
    use std::os::unix::fs::MetadataExt;
    if std::fs::metadata("/proc/self").map(|meta| meta.uid()).ok() == Some(0) {
        eprintln!("skipped: run as root, an allowed request would reach the real helper");
        return;
    }
    let bounds = [
        "--max-bytes",
        "1024",
        "--max-members",
        "3",
        "--max-path",
        "4096",
    ];
    let mut inject = vec!["inject", "recipe-4242-1-ab"];
    inject.extend(bounds);
    inject.push("/");
    let mut extract = vec!["extract", "recipe-4242-1-ab"];
    extract.extend(bounds);
    extract.extend(["/work", "/out"]);
    // Every shape `src/ginux.rs` makes the `ginux` CLI pass to the helper.
    for request in [
        vec!["import", "recipe-4242-1-ab", "/tmp/config.toml"],
        inject,
        vec![
            "start",
            "recipe-4242-1-ab",
            "--wait",
            "123",
            "456",
            "--stream-console",
            "--full-audit",
        ],
        vec!["status", "recipe-4242-1-ab", "--format", "json"],
        vec!["status", "recipe-4242-probe"],
        vec!["stop", "recipe-4242-1-ab"],
        vec!["delete", "recipe-4242-1-ab"],
        extract,
    ] {
        let output = run_guard(&ROOTS, &request);
        assert_eq!(
            output.status.code(),
            Some(NOT_ROOT),
            "{request:?} is a request Recipe makes: the guard must pass its check and stop only \
             at the root check (exit {NOT_ROOT}); stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // Without sudo's ids the caller counts as root, which this process is not.
    let output = Command::new(guard_bin())
        .env_clear()
        .args(ROOTS)
        .args(["delete", "recipe-4242-1-ab"])
        .output()
        .expect("cannot run recipe-ginux-guard");
    assert_eq!(output.status.code(), Some(NOT_ROOT));
    // Only one of sudo's ids is refused outright.
    let output = Command::new(guard_bin())
        .env_clear()
        .env("SUDO_UID", "4242")
        .args(ROOTS)
        .args(["delete", "recipe-4242-1-ab"])
        .output()
        .expect("cannot run recipe-ginux-guard");
    assert_eq!(output.status.code(), Some(NOT_ROOT));
}

/// `--check-config --image-allowlist LIST FILE`; the exit code and stderr.
fn check_listed(list: &Path, config: &Path) -> (i32, String) {
    let output = Command::new(guard_bin())
        .arg("--check-config")
        .arg("--image-allowlist")
        .arg(list)
        .arg(config)
        .env_clear()
        .output()
        .expect("cannot run recipe-ginux-guard");
    (
        output
            .status
            .code()
            .expect("the guard was killed by a signal"),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Whether the guard would trust a root-owned 0644 file in `dir`: this test
/// runs as root and every directory from `dir` up is root-owned and closed to
/// group and others.
fn trusted_location(dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let root = std::fs::metadata("/proc/self").map(|meta| meta.uid()).ok() == Some(0);
    root && dir.ancestors().all(|directory| {
        std::fs::symlink_metadata(directory).is_ok_and(|meta| {
            meta.file_type().is_dir() && meta.uid() == 0 && meta.mode() & 0o022 == 0
        })
    })
}

#[test]
fn the_guard_checks_the_image_against_the_root_owned_allowlist() {
    let (dir, recipes) = create_test_env();
    let config_text = rendered_config(dir.path(), &recipes);
    let config = dir.path().join("config.toml");
    std::fs::write(&config, &config_text).unwrap();
    let other = set_line(
        &config_text,
        "[instance]",
        "root",
        "root = \"local/recipe-compiler@sha256:00000000000000000000000000000000000000000000000000000000000000ff\"",
    );
    let other_config = dir.path().join("other.toml");
    std::fs::write(&other_config, &other).unwrap();
    let not_hex = set_line(
        &config_text,
        "[instance]",
        "root",
        "root = \"local/recipe-compiler@sha256:000000000000000000000000000000000000000000000000000000000000000g\"",
    );
    let not_hex_config = dir.path().join("not-hex.toml");
    std::fs::write(&not_hex_config, &not_hex).unwrap();

    let lists = dir.path().join("lists");
    std::fs::create_dir(&lists).unwrap();
    std::fs::set_permissions(&lists, std::fs::Permissions::from_mode(0o755)).unwrap();
    let list = lists.join("runtime-images");
    std::fs::write(
        &list,
        format!("# the shipped images\n\n{MANIFEST}\n  \n# end\n"),
    )
    .unwrap();
    std::fs::set_permissions(&list, std::fs::Permissions::from_mode(0o644)).unwrap();
    let trusted = trusted_location(&lists);

    let (code, stderr) = check_listed(&list, &config);
    if trusted {
        assert_eq!(code, 0, "the listed image must be accepted: {stderr}");
    } else {
        // Run as an ordinary user, or under a writable directory: the fixture
        // is not root's, so the guard trusts no image from it.
        assert_eq!(code, REFUSED_CONFIG, "{stderr}");
        assert!(stderr.contains("untrusted image allowlist"), "{stderr}");
    }
    let (code, stderr) = check_listed(&list, &other_config);
    assert_eq!(
        code, REFUSED_CONFIG,
        "a not-listed image must be refused: {stderr}"
    );
    assert!(
        stderr.contains(if trusted {
            "is not a runtime image root installed on this host"
        } else {
            "untrusted image allowlist"
        }),
        "{stderr}"
    );
    assert!(stderr.contains("sudo recipe runtime fetch"), "{stderr}");
    // The shape is checked before the allowlist, whoever runs the check.
    let (code, stderr) = check_listed(&list, &not_hex_config);
    assert_eq!(code, REFUSED_CONFIG, "{stderr}");
    assert!(
        stderr.contains("is not REGISTRY/REPO@sha256:<64 hex>"),
        "{stderr}"
    );

    // Refused for every runner: a link to the list, a list others can write,
    // a missing list, a relative path.
    let link = lists.join("link");
    std::os::unix::fs::symlink(&list, &link).unwrap();
    let writable = lists.join("writable");
    std::fs::write(&writable, format!("{MANIFEST}\n")).unwrap();
    std::fs::set_permissions(&writable, std::fs::Permissions::from_mode(0o666)).unwrap();
    for (name, path) in [
        ("symlink", link.clone()),
        ("world-writable", writable.clone()),
        ("missing", lists.join("missing")),
        ("relative", std::path::PathBuf::from("runtime-images")),
    ] {
        let (code, stderr) = check_listed(&path, &config);
        assert_eq!(code, REFUSED_CONFIG, "{name}: {stderr}");
        assert!(
            stderr.contains("untrusted image allowlist"),
            "{name}: {stderr}"
        );
    }

    // The option belongs to the dry run alone, and needs both of its operands.
    let output = Command::new(guard_bin())
        .args(["--check-config", "--image-allowlist"])
        .arg(&list)
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(REFUSED_REQUEST));
    let output = Command::new(guard_bin())
        .env_clear()
        .env("SUDO_UID", "4242")
        .env("SUDO_GID", "4242")
        .arg("--image-allowlist")
        .arg(&list)
        .args(ROOTS)
        .args(["import", "recipe-4242-1-ab"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(REFUSED_REQUEST));
}
