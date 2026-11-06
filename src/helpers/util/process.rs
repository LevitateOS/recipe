//! Execution utilities for recipe scripts
//!
//! Provides helpers for running installed packages.

use crate::helpers::internal::cmd;
use rhai::EvalAltResult;
use std::process::Command;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PrivilegeCommand {
    None,
    Sudo,
    Doas,
}

fn run_command(cmd: &str, args: &[String]) -> Result<crate::process::Finished, Box<EvalAltResult>> {
    let mut command = Command::new(cmd);
    command.args(args);
    cmd::capture(command)
}

fn run_checked(cmd: &str, args: &[String]) -> Result<(), Box<EvalAltResult>> {
    let mut command = Command::new(cmd);
    command.args(args);
    cmd::run_checked(command)
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

fn trim_stdout(output: crate::process::Finished) -> String {
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn command_path(name: &str) -> Option<std::path::PathBuf> {
    crate::process::find_program(name)
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

type ApkPolicyEntries = Vec<(String, Vec<String>)>;

fn parse_apk_policy(output: &str) -> ApkPolicyEntries {
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

fn apk_policy(helper: &str, name: &str) -> Result<Option<ApkPolicyEntries>, Box<EvalAltResult>> {
    require_apk(helper)?;
    let args = strings(&["policy", name]);
    let output = run_command("apk", &args)?;
    if !output.exit.success() {
        return Ok(None);
    }
    Ok(Some(parse_apk_policy(&String::from_utf8_lossy(
        &output.stdout,
    ))))
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

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
/// Child stdout streams to Recipe's stderr so machine output stays intact.
pub fn exec(cmd: &str, args: rhai::Array) -> Result<i64, Box<EvalAltResult>> {
    exec_with_options(cmd, args, rhai::Map::new())
}

fn argv_command(
    program: &str,
    args: rhai::Array,
    options: rhai::Map,
) -> Result<Command, Box<EvalAltResult>> {
    let mut command = Command::new(program);
    command.args(args.into_iter().map(|value| value.to_string()));
    for (key, value) in options {
        match key.as_str() {
            "cwd" => {
                let dir = value
                    .into_string()
                    .map_err(|_| "command option cwd must be a string")?;
                if dir.is_empty() {
                    return Err("command option cwd must not be empty".into());
                }
                command.current_dir(dir);
            }
            "env" => {
                let vars = value
                    .try_cast::<rhai::Map>()
                    .ok_or("command option env must be a map")?;
                for (name, value) in vars {
                    let text = value.into_string().map_err(|_| {
                        format!("command environment value {name} must be a string")
                    })?;
                    command.env(name.as_str(), text);
                }
            }
            other => return Err(format!("unknown command option: {other}").into()),
        }
    }
    Ok(command)
}

/// Execute argv with optional cwd/env; return status and throw on execution errors.
pub fn exec_with_options(
    cmd: &str,
    args: rhai::Array,
    options: rhai::Map,
) -> Result<i64, Box<EvalAltResult>> {
    cmd::run_status(argv_command(cmd, args, options)?)
}

/// Execute argv, stream diagnostics to stderr, and throw on nonzero exit.
pub fn exec_checked(cmd: &str, args: rhai::Array) -> Result<(), Box<EvalAltResult>> {
    exec_checked_with_options(cmd, args, rhai::Map::new())
}

/// Checked argv execution with per-command cwd and environment overrides.
pub fn exec_checked_with_options(
    cmd: &str,
    args: rhai::Array,
    options: rhai::Map,
) -> Result<(), Box<EvalAltResult>> {
    cmd::run_checked(argv_command(cmd, args, options)?)
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
    exec_output_with_options(cmd, args, rhai::Map::new())
}

/// Capture checked argv stdout with per-command cwd and environment overrides.
pub fn exec_output_with_options(
    cmd: &str,
    args: rhai::Array,
    options: rhai::Map,
) -> Result<String, Box<EvalAltResult>> {
    let output = cmd::checked_capture(argv_command(cmd, args, options)?)?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Check whether an RPM package is installed
pub fn rpm_installed(name: &str) -> Result<bool, Box<EvalAltResult>> {
    require_rpm("rpm_installed")?;
    let args = strings(&["-q", name]);
    run_command("rpm", &args).map(|output| output.exit.success())
}

/// Return the installed RPM version
pub fn rpm_version(name: &str) -> Result<String, Box<EvalAltResult>> {
    require_rpm("rpm_version")?;
    let args = strings(&["-q", "--qf", "%{VERSION}", name]);
    let output = run_command("rpm", &args)?;
    if !output.exit.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("rpm_version failed for {}: {}", name, stderr.trim()).into());
    }
    Ok(trim_stdout(output))
}

/// Check whether a DNF package is available
pub fn dnf_package_available(name: &str) -> Result<bool, Box<EvalAltResult>> {
    require_dnf("dnf_package_available")?;
    let args = strings(&["-q", "info", name]);
    run_command("dnf", &args).map(|output| output.exit.success())
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
    if !output.exit.success() {
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
