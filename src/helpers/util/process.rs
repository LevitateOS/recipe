//! Execution utilities for recipe scripts
//!
//! Provides helpers for running installed packages.

use rhai::EvalAltResult;
use std::env;
use std::process::Command;

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

fn sudo_dnf_args(subcommand: &[&str]) -> Vec<String> {
    let mut args = strings(&["-n", "dnf"]);
    args.extend(strings(subcommand));
    args
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

fn require_sudo(helper: &str) -> Result<(), Box<EvalAltResult>> {
    require_command(
        helper,
        "sudo",
        "this helper requires non-interactive sudo on the host",
    )
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
    require_dnf(if allow_erasing {
        "dnf_install_allow_erasing"
    } else {
        "dnf_install"
    })?;
    require_sudo(if allow_erasing {
        "dnf_install_allow_erasing"
    } else {
        "dnf_install"
    })?;
    let package_args = normalize_packages(packages)?;
    let mut args = sudo_dnf_args(&["install", "-y"]);
    if allow_erasing {
        args.push("--allowerasing".to_owned());
    }
    args.extend(package_args);
    run_checked("sudo", &args)
}

/// Install packages with DNF using non-interactive sudo
pub fn dnf_install(packages: rhai::Array) -> Result<(), Box<EvalAltResult>> {
    dnf_install_impl(packages, false)
}

/// Install packages with DNF using --allowerasing
pub fn dnf_install_allow_erasing(packages: rhai::Array) -> Result<(), Box<EvalAltResult>> {
    dnf_install_impl(packages, true)
}

/// Add a DNF repository using non-interactive sudo
pub fn dnf_add_repo(url: &str) -> Result<(), Box<EvalAltResult>> {
    require_dnf("dnf_add_repo")?;
    require_sudo("dnf_add_repo")?;
    let args = sudo_dnf_args(&["config-manager", "--add-repo", url]);
    run_checked("sudo", &args)
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

fn dnf_download_impl(
    packages: rhai::Array,
    dest_dir: &str,
    arches: rhai::Array,
    resolve: bool,
) -> Result<rhai::Array, Box<EvalAltResult>> {
    require_dnf("dnf_download")?;
    require_sudo("dnf_download")?;
    let package_args = normalize_packages(packages)?;
    let arch_args = normalize_strings(arches, "architecture list is empty")?;
    std::fs::create_dir_all(dest_dir)
        .map_err(|e| format!("dnf_download mkdir failed for {}: {}", dest_dir, e))?;

    let before = list_rpm_files(dest_dir)?;
    let mut args = sudo_dnf_args(&["download", "-q"]);
    if resolve {
        args.push("--resolve".to_owned());
    }
    args.push(format!("--destdir={dest_dir}"));
    for arch in arch_args {
        args.push("--arch".to_owned());
        args.push(arch);
    }
    args.extend(package_args);
    run_checked("sudo", &args)?;

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
    }

    #[test]
    fn dnf_install_fails_fast_when_sudo_is_missing() {
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

        let filtered = path_without_commands(&["sudo"]);
        let joined = std::env::join_paths(
            std::iter::once(bin_dir.clone()).chain(std::env::split_paths(&filtered)),
        )
        .unwrap();
        let _path_guard = PathGuard::set(&joined);

        assert!(command_exists("dnf"));
        assert!(!command_exists("sudo"));
        assert!(dnf_package_available("fakepkg").unwrap());

        let err = dnf_install(vec![rhai::Dynamic::from("alpha")])
            .unwrap_err()
            .to_string();
        assert!(err.contains("helper dnf_install requires host command 'sudo' in PATH"));
    }
}
