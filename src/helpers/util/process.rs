//! Execution utilities for recipe scripts
//!
//! Provides helpers for running installed packages.

use rhai::EvalAltResult;
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
pub fn rpm_installed(name: &str) -> bool {
    let args = strings(&["-q", name]);
    run_command("rpm", &args)
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Return the installed RPM version
pub fn rpm_version(name: &str) -> Result<String, Box<EvalAltResult>> {
    let args = strings(&["-q", "--qf", "%{VERSION}", name]);
    let output = run_command("rpm", &args)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("rpm_version failed for {}: {}", name, stderr.trim()).into());
    }
    Ok(trim_stdout(output))
}

/// Check whether a DNF package is available
pub fn dnf_package_available(name: &str) -> bool {
    let args = strings(&["-q", "info", name]);
    run_command("dnf", &args)
        .map(|output| output.status.success())
        .unwrap_or(false)
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

fn dnf_install_impl(packages: rhai::Array, allow_erasing: bool) -> Result<(), Box<EvalAltResult>> {
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
    let args = sudo_dnf_args(&["config-manager", "--add-repo", url]);
    run_checked("sudo", &args)
}
