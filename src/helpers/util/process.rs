//! Execution utilities for recipe scripts
//!
//! Provides helpers for running installed packages.

use rhai::EvalAltResult;
use std::env;
use std::process::Command;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrivilegeCommand {
    None,
    Sudo,
    Doas,
}

fn run_command(cmd: &str, args: &[String]) -> Result<std::process::Output, Box<EvalAltResult>> {
    Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to execute {}: {}", cmd, e).into())
}

fn run_checked(cmd: &str, args: &[String]) -> Result<(), Box<EvalAltResult>> {
    let output = run_command(cmd, args)?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    Err(format!(
        "Command {} failed with exit code {:?}: {}\n{}",
        cmd,
        output.status.code(),
        stderr.trim(),
        stdout.trim()
    )
    .trim()
    .to_owned()
    .into())
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn run_with_privilege(
    privilege: PrivilegeCommand,
    command: &str,
    subcommand: &[String],
) -> Result<(), Box<EvalAltResult>> {
    match privilege {
        PrivilegeCommand::None => run_checked(command, subcommand),
        PrivilegeCommand::Sudo => {
            let mut args = strings(&["-n", command]);
            args.extend(subcommand.iter().cloned());
            run_checked("sudo", &args)
        }
        PrivilegeCommand::Doas => {
            let mut args = strings(&["-n", command]);
            args.extend(subcommand.iter().cloned());
            run_checked("doas", &args)
        }
    }
}

fn trim_stdout(output: std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn command_path(name: &str) -> Option<std::path::PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(name);
        if candidate.is_file() {
            Some(candidate)
        } else {
            None
        }
    })
}

pub fn command_exists(name: &str) -> bool {
    command_path(name).is_some()
}

fn require_command(helper: &str, cmd: &str, note: &str) -> Result<(), Box<EvalAltResult>> {
    if command_exists(cmd) {
        return Ok(());
    }

    Err(format!("helper {helper} requires host command '{cmd}' in PATH; {note}").into())
}

fn require_rpm(helper: &str) -> Result<(), Box<EvalAltResult>> {
    require_command(
        helper,
        "rpm",
        "this helper is only usable on RPM-based hosts",
    )
}

fn require_dnf(helper: &str) -> Result<(), Box<EvalAltResult>> {
    require_command(
        helper,
        "dnf",
        "this helper is only usable on RPM/DNF-based hosts",
    )
}

#[cfg(test)]
fn running_as_root() -> bool {
    if env::var_os("RECIPE_TEST_FORCE_NON_ROOT").is_some() {
        return false;
    }
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(test))]
fn running_as_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn require_privilege_command(helper: &str) -> Result<PrivilegeCommand, Box<EvalAltResult>> {
    if running_as_root() {
        return Ok(PrivilegeCommand::None);
    }
    if command_exists("sudo") {
        return Ok(PrivilegeCommand::Sudo);
    }
    if command_exists("doas") {
        return Ok(PrivilegeCommand::Doas);
    }

    Err(format!(
        "helper {helper} requires host command 'sudo' or 'doas' in PATH; this helper requires non-interactive privilege escalation on the host"
    )
    .into())
}

fn require_apk(helper: &str) -> Result<(), Box<EvalAltResult>> {
    require_command(
        helper,
        "apk",
        "this helper is only usable on Alpine/APK-based hosts",
    )
}

fn parse_apk_policy(output: &str) -> Vec<(String, Vec<String>)> {
    let mut entries = Vec::new();
    let mut current_version: Option<String> = None;
    let mut current_sources: Vec<String> = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.ends_with(" policy:") {
            continue;
        }

        if trimmed.ends_with(':') && !trimmed.contains(' ') {
            if let Some(version) = current_version.take() {
                entries.push((version, std::mem::take(&mut current_sources)));
            }
            current_version = Some(trimmed.trim_end_matches(':').to_owned());
            continue;
        }

        if current_version.is_some() {
            current_sources.push(trimmed.to_owned());
        }
    }

    if let Some(version) = current_version.take() {
        entries.push((version, current_sources));
    }

    entries
}

fn apk_policy(
    helper: &str,
    name: &str,
) -> Result<Option<Vec<(String, Vec<String>)>>, Box<EvalAltResult>> {
    require_apk(helper)?;
    let args = strings(&["policy", name]);
    let output = run_command("apk", &args)?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(parse_apk_policy(&String::from_utf8_lossy(
        &output.stdout,
    ))))
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
fn apk_repositories_file() -> String {
    std::env::var("RECIPE_TEST_APK_REPOSITORIES_FILE")
        .unwrap_or_else(|_| "/etc/apk/repositories".to_owned())
}

#[cfg(not(test))]
fn apk_repositories_file() -> String {
    "/etc/apk/repositories".to_owned()
}

/// Execute a command with arguments
///
/// # Arguments
/// * `cmd` - The command to execute
/// * `args` - Arguments as a Rhai array
///
/// # Returns
/// Exit code of the command
pub fn exec(cmd: &str, args: rhai::Array) -> Result<i64, Box<EvalAltResult>> {
    let args: Vec<String> = args.into_iter().map(|v| v.to_string()).collect();

    let status = Command::new(cmd)
        .args(&args)
        .status()
        .map_err(|e| format!("Failed to execute {}: {}", cmd, e))?;

    Ok(status.code().unwrap_or(-1) as i64)
}

/// Execute a command and return its output
///
/// # Arguments
/// * `cmd` - The command to execute
/// * `args` - Arguments as a Rhai array
///
/// # Returns
/// stdout of the command as a string
pub fn exec_output(cmd: &str, args: rhai::Array) -> Result<String, Box<EvalAltResult>> {
    let args: Vec<String> = args.into_iter().map(|v| v.to_string()).collect();

    let output = run_command(cmd, &args)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Command {} failed: {}", cmd, stderr).into());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Check whether an RPM package is installed
pub fn rpm_installed(name: &str) -> Result<bool, Box<EvalAltResult>> {
    require_rpm("rpm_installed")?;
    let args = strings(&["-q", name]);
    run_command("rpm", &args).map(|output| output.status.success())
}

/// Return the installed RPM version
pub fn rpm_version(name: &str) -> Result<String, Box<EvalAltResult>> {
    require_rpm("rpm_version")?;
    let args = strings(&["-q", "--qf", "%{VERSION}", name]);
    let output = run_command("rpm", &args)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("rpm_version failed for {}: {}", name, stderr.trim()).into());
    }
    Ok(trim_stdout(output))
}

/// Check whether a DNF package is available
pub fn dnf_package_available(name: &str) -> Result<bool, Box<EvalAltResult>> {
    require_dnf("dnf_package_available")?;
    let args = strings(&["-q", "info", name]);
    run_command("dnf", &args).map(|output| output.status.success())
}

pub fn apk_installed(name: &str) -> Result<bool, Box<EvalAltResult>> {
    let Some(policy) = apk_policy("apk_installed", name)? else {
        return Ok(false);
    };

    Ok(policy.iter().any(|(_, sources)| {
        sources
            .iter()
            .any(|source| source == "lib/apk/db/installed")
    }))
}

pub fn apk_version(name: &str) -> Result<String, Box<EvalAltResult>> {
    let Some(policy) = apk_policy("apk_version", name)? else {
        return Err(format!("apk_version failed for {}: package is not installed", name).into());
    };

    policy
        .into_iter()
        .find(|(_, sources)| {
            sources
                .iter()
                .any(|source| source == "lib/apk/db/installed")
        })
        .map(|(version, _)| version)
        .ok_or_else(|| format!("apk_version failed for {}: package is not installed", name).into())
}

pub fn apk_package_available(name: &str) -> Result<bool, Box<EvalAltResult>> {
    require_apk("apk_package_available")?;
    let args = strings(&["search", "-v", name]);
    let output = run_command("apk", &args)?;
    if !output.status.success() {
        return Ok(false);
    }

    let prefix = format!("{name}-");
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .any(|line| line.starts_with(&prefix)))
}

fn normalize_packages(packages: rhai::Array) -> Result<Vec<String>, Box<EvalAltResult>> {
    let mut out = Vec::new();
    for item in packages {
        let value = item.to_string();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push(trimmed.to_owned());
    }
    if out.is_empty() {
        return Err("package list is empty".into());
    }
    Ok(out)
}

fn normalize_strings(
    values: rhai::Array,
    empty_message: &str,
) -> Result<Vec<String>, Box<EvalAltResult>> {
    let mut out = Vec::new();
    for item in values {
        let value = item.to_string();
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        out.push(trimmed.to_owned());
    }
    if out.is_empty() {
        return Err(empty_message.into());
    }
    Ok(out)
}

fn dnf_install_impl(packages: rhai::Array, allow_erasing: bool) -> Result<(), Box<EvalAltResult>> {
    let helper = if allow_erasing {
        "dnf_install_allow_erasing"
    } else {
        "dnf_install"
    };
    require_dnf(helper)?;
    let privilege = require_privilege_command(helper)?;
    let package_args = normalize_packages(packages)?;
    let mut args = strings(&["install", "-y"]);
    if allow_erasing {
        args.push("--allowerasing".to_owned());
    }
    args.extend(package_args);
    run_with_privilege(privilege, "dnf", &args)
}

/// Install packages with DNF using the configured non-interactive privilege runner.
pub fn dnf_install(packages: rhai::Array) -> Result<(), Box<EvalAltResult>> {
    dnf_install_impl(packages, false)
}

/// Install packages with DNF using --allowerasing
pub fn dnf_install_allow_erasing(packages: rhai::Array) -> Result<(), Box<EvalAltResult>> {
    dnf_install_impl(packages, true)
}

fn apk_install_impl(packages: rhai::Array) -> Result<(), Box<EvalAltResult>> {
    require_apk("apk_install")?;
    let privilege = require_privilege_command("apk_install")?;
    let package_args = normalize_packages(packages)?;
    let mut args = strings(&["add", "--update-cache"]);
    args.extend(package_args);
    run_with_privilege(privilege, "apk", &args)
}

pub fn apk_install(packages: rhai::Array) -> Result<(), Box<EvalAltResult>> {
    apk_install_impl(packages)
}

/// Add a DNF repository using the configured non-interactive privilege runner.
pub fn dnf_add_repo(url: &str) -> Result<(), Box<EvalAltResult>> {
    require_dnf("dnf_add_repo")?;
    let privilege = require_privilege_command("dnf_add_repo")?;
    let args = strings(&["config-manager", "--add-repo", url]);
    run_with_privilege(privilege, "dnf", &args)
}

pub fn apk_add_repo(url: &str) -> Result<(), Box<EvalAltResult>> {
    require_apk("apk_add_repo")?;
    let privilege = require_privilege_command("apk_add_repo")?;
    let repo_file = apk_repositories_file();
    let quoted_url = shell_single_quote(url);
    let quoted_repo_file = shell_single_quote(&repo_file);
    let script = format!(
        "grep -qxF {quoted_url} {quoted_repo_file} || printf '%s\\n' {quoted_url} >> {quoted_repo_file}"
    );
    let args = vec!["-c".to_owned(), script];
    run_with_privilege(privilege, "sh", &args)
}

fn list_rpm_files(dir: &str) -> Result<Vec<String>, Box<EvalAltResult>> {
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| format!("dnf_download read_dir failed for {}: {}", dir, e))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rpm"))
        .map(|path| path.to_string_lossy().to_string())
        .collect();
    files.sort();
    Ok(files)
}

fn list_apk_files(dir: &str) -> Result<Vec<String>, Box<EvalAltResult>> {
    let mut files: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| format!("apk_download read_dir failed for {}: {}", dir, e))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| path.extension().is_some_and(|ext| ext == "apk"))
        .map(|path| path.to_string_lossy().to_string())
        .collect();
    files.sort();
    Ok(files)
}

fn dnf_download_impl(
    packages: rhai::Array,
    dest_dir: &str,
    arches: rhai::Array,
    resolve: bool,
) -> Result<rhai::Array, Box<EvalAltResult>> {
    require_dnf("dnf_download")?;
    let privilege = require_privilege_command("dnf_download")?;
    let package_args = normalize_packages(packages)?;
    let arch_args = normalize_strings(arches, "architecture list is empty")?;
    std::fs::create_dir_all(dest_dir)
        .map_err(|e| format!("dnf_download mkdir failed for {}: {}", dest_dir, e))?;

    let before = list_rpm_files(dest_dir)?;
    let mut args = strings(&["download", "-q"]);
    if resolve {
        args.push("--resolve".to_owned());
    }
    args.push(format!("--destdir={dest_dir}"));
    for arch in arch_args {
        args.push("--arch".to_owned());
        args.push(arch);
    }
    args.extend(package_args);
    run_with_privilege(privilege, "dnf", &args)?;

    let after = list_rpm_files(dest_dir)?;
    let before_set: std::collections::BTreeSet<String> = before.into_iter().collect();
    let downloaded: rhai::Array = after
        .into_iter()
        .filter(|path| !before_set.contains(path))
        .map(rhai::Dynamic::from)
        .collect();
    Ok(downloaded)
}

pub fn dnf_download(
    packages: rhai::Array,
    dest_dir: &str,
    arches: rhai::Array,
) -> Result<rhai::Array, Box<EvalAltResult>> {
    dnf_download_impl(packages, dest_dir, arches, true)
}

pub fn dnf_download_with_resolve(
    packages: rhai::Array,
    dest_dir: &str,
    arches: rhai::Array,
    resolve: bool,
) -> Result<rhai::Array, Box<EvalAltResult>> {
    dnf_download_impl(packages, dest_dir, arches, resolve)
}

pub fn apk_download(
    packages: rhai::Array,
    dest_dir: &str,
) -> Result<rhai::Array, Box<EvalAltResult>> {
    require_apk("apk_download")?;
    let package_args = normalize_packages(packages)?;
    std::fs::create_dir_all(dest_dir)
        .map_err(|e| format!("apk_download mkdir failed for {}: {}", dest_dir, e))?;

    let before = list_apk_files(dest_dir)?;
    let mut args = strings(&["fetch", "--output", dest_dir]);
    args.extend(package_args);
    run_checked("apk", &args)?;

    let after = list_apk_files(dest_dir)?;
    let before_set: std::collections::BTreeSet<String> = before.into_iter().collect();
    let downloaded: rhai::Array = after
        .into_iter()
        .filter(|path| !before_set.contains(path))
        .map(rhai::Dynamic::from)
        .collect();
    Ok(downloaded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    fn path_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn path_without_commands(commands: &[&str]) -> OsString {
        let Some(path) = std::env::var_os("PATH") else {
            return OsString::new();
        };

        let filtered: Vec<PathBuf> = std::env::split_paths(&path)
            .filter(|dir| !commands.iter().any(|cmd| dir.join(cmd).is_file()))
            .collect();

        std::env::join_paths(filtered).unwrap_or_else(|_| OsString::new())
    }

    struct PathGuard(Option<OsString>);

    impl PathGuard {
        fn set(path: &OsString) -> Self {
            let previous = std::env::var_os("PATH");
            unsafe { std::env::set_var("PATH", path) };
            Self(previous)
        }
    }

    impl Drop for PathGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(path) => unsafe { std::env::set_var("PATH", path) },
                None => unsafe { std::env::remove_var("PATH") },
            }
        }
    }

    fn make_executable(path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(path, perms).unwrap();
        }
    }

    fn write_exec_wrapper(bin_dir: &std::path::Path, name: &str, target: &std::path::Path) {
        let wrapper = bin_dir.join(name);
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexec {} \"$@\"\n",
                shell_single_quote(&target.display().to_string())
            ),
        )
        .unwrap();
        make_executable(&wrapper);
    }

    #[test]
    fn rpm_and_dnf_helpers_fail_fast_when_commands_are_missing() {
        let _guard = path_lock().lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let empty_path = temp.path().as_os_str().to_os_string();
        let _path_guard = PathGuard::set(&empty_path);

        assert!(!command_exists("rpm"));
        assert!(!command_exists("dnf"));
        assert!(!command_exists("sudo"));

        let rpm_err = rpm_installed("fakepkg").unwrap_err().to_string();
        assert!(rpm_err.contains("helper rpm_installed requires host command 'rpm' in PATH"));

        let dnf_err = dnf_package_available("fakepkg").unwrap_err().to_string();
        assert!(
            dnf_err.contains("helper dnf_package_available requires host command 'dnf' in PATH")
        );

        let apk_err = apk_installed("fakepkg").unwrap_err().to_string();
        assert!(apk_err.contains("helper apk_installed requires host command 'apk' in PATH"));
    }

    #[test]
    fn dnf_install_fails_fast_when_sudo_or_doas_is_missing() {
        let _guard = path_lock().lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bin_dir = temp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let dnf_path = bin_dir.join("dnf");
        std::fs::write(
            &dnf_path,
            "#!/bin/sh\nif [ \"$1\" = \"-q\" ] && [ \"$2\" = \"info\" ]; then exit 0; fi\nexit 0\n",
        )
        .unwrap();
        make_executable(&dnf_path);

        let filtered = path_without_commands(&["sudo", "doas"]);
        let joined = std::env::join_paths(
            std::iter::once(bin_dir.clone()).chain(std::env::split_paths(&filtered)),
        )
        .unwrap();
        let _path_guard = PathGuard::set(&joined);
        unsafe {
            std::env::set_var("RECIPE_TEST_FORCE_NON_ROOT", "1");
        }

        assert!(command_exists("dnf"));
        assert!(!command_exists("sudo"));
        assert!(!command_exists("doas"));
        assert!(dnf_package_available("fakepkg").unwrap());

        let err = dnf_install(vec![rhai::Dynamic::from("alpha")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("helper dnf_install requires host command 'sudo' or 'doas' in PATH"));
        unsafe {
            std::env::remove_var("RECIPE_TEST_FORCE_NON_ROOT");
        }
    }

    #[test]
    fn apk_helpers_work_with_doas_and_fail_fast_when_privilege_command_is_missing() {
        let _guard = path_lock().lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let bin_dir = temp.path().join("bin");
        let downloads_dir = temp.path().join("downloads");
        let log_file = temp.path().join("apk.log");
        let repo_file = temp.path().join("repositories");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(&downloads_dir).unwrap();

        let doas_path = bin_dir.join("doas");
        std::fs::write(
            &doas_path,
            "#!/bin/sh\nif [ \"$1\" = \"-n\" ]; then shift; fi\nexec \"$@\"\n",
        )
        .unwrap();
        make_executable(&doas_path);

        let sh_path = command_path("sh").expect("host sh not found");
        write_exec_wrapper(&bin_dir, "sh", &sh_path);

        let grep_path = command_path("grep").expect("host grep not found");
        write_exec_wrapper(&bin_dir, "grep", &grep_path);

        let touch_path = command_path("touch").expect("host touch not found");
        write_exec_wrapper(&bin_dir, "touch", &touch_path);

        let apk_path = bin_dir.join("apk");
        std::fs::write(
            &apk_path,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{}\"\nif [ \"$1\" = \"policy\" ] && [ \"$2\" = \"fakepkg\" ]; then\n  printf 'fakepkg policy:\\n  1.2.3-r0:\\n    lib/apk/db/installed\\n    https://example.invalid/repo\\n';\n  exit 0\nfi\nif [ \"$1\" = \"policy\" ]; then\n  printf '%s policy:\\n' \"$2\";\n  exit 0\nfi\nif [ \"$1\" = \"search\" ] && [ \"$2\" = \"-v\" ] && [ \"$3\" = \"fakepkg\" ]; then\n  printf 'fakepkg-1.2.3-r0 description\\n';\n  exit 0\nfi\nif [ \"$1\" = \"search\" ] && [ \"$2\" = \"-v\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"add\" ] && [ \"$2\" = \"--update-cache\" ]; then\n  exit 0\nfi\nif [ \"$1\" = \"fetch\" ] && [ \"$2\" = \"--output\" ]; then\n  destdir=\"$3\"\n  shift 3\n  for pkg in \"$@\"; do\n    touch \"$destdir/$pkg-1.2.3-r0.apk\"\n  done\n  exit 0\nfi\nexit 1\n",
                log_file.display()
            ),
        )
        .unwrap();
        make_executable(&apk_path);

        let filtered = path_without_commands(&["sudo", "doas", "apk"]);
        let joined = std::env::join_paths(
            std::iter::once(bin_dir.clone()).chain(std::env::split_paths(&filtered)),
        )
        .unwrap();
        let _path_guard = PathGuard::set(&joined);
        unsafe {
            std::env::set_var("RECIPE_TEST_APK_REPOSITORIES_FILE", &repo_file);
            std::env::set_var("RECIPE_TEST_FORCE_NON_ROOT", "1");
        }

        assert!(command_exists("apk"));
        assert!(!command_exists("sudo"));
        assert!(command_exists("doas"));
        assert!(apk_installed("fakepkg").unwrap());
        assert!(!apk_installed("missingpkg").unwrap());
        assert_eq!(apk_version("fakepkg").unwrap(), "1.2.3-r0");
        assert!(apk_package_available("fakepkg").unwrap());
        assert!(!apk_package_available("missingpkg").unwrap());
        apk_add_repo("https://example.invalid/alpine").unwrap();
        apk_install(vec![
            rhai::Dynamic::from("alpha"),
            rhai::Dynamic::from("beta"),
        ])
        .unwrap();
        let downloaded = apk_download(
            vec![rhai::Dynamic::from("alpha"), rhai::Dynamic::from("beta")],
            downloads_dir.to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(downloaded.len(), 2);

        let repo_contents = std::fs::read_to_string(&repo_file).unwrap();
        assert!(repo_contents.contains("https://example.invalid/alpine"));

        let log = std::fs::read_to_string(&log_file).unwrap();
        assert!(log.contains("add --update-cache alpha beta"));
        assert!(log.contains(&format!(
            "fetch --output {} alpha beta",
            downloads_dir.display()
        )));

        unsafe {
            std::env::remove_var("RECIPE_TEST_APK_REPOSITORIES_FILE");
            std::env::remove_var("RECIPE_TEST_FORCE_NON_ROOT");
        }

        std::fs::remove_file(&doas_path).unwrap();
        let no_privilege = path_without_commands(&["sudo", "doas", "apk"]);
        let joined = std::env::join_paths(
            std::iter::once(bin_dir).chain(std::env::split_paths(&no_privilege)),
        )
        .unwrap();
        let _path_guard = PathGuard::set(&joined);
        unsafe {
            std::env::set_var("RECIPE_TEST_FORCE_NON_ROOT", "1");
        }
        let err = apk_install(vec![rhai::Dynamic::from("alpha")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("helper apk_install requires host command 'sudo' or 'doas' in PATH"));
        unsafe {
            std::env::remove_var("RECIPE_TEST_FORCE_NON_ROOT");
        }
    }
}
