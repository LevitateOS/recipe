//! VM management for testing levitate-recipe.
//!
//! Uses QEMU with an Arch Linux cloud image for recipe testing.

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Get the recipe project root (parent of xtask)
fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// VM configuration paths
fn vm_dir() -> PathBuf {
    let dir = project_root().join(".vm");
    fs::create_dir_all(&dir).ok();
    dir
}

fn disk_image() -> PathBuf {
    vm_dir().join("recipe-test.qcow2")
}

fn pid_file() -> PathBuf {
    vm_dir().join("qemu.pid")
}

fn monitor_socket() -> PathBuf {
    vm_dir().join("qemu-monitor.sock")
}

fn serial_log() -> PathBuf {
    vm_dir().join("serial.log")
}

const SSH_PORT: u16 = 2222;
const DISK_SIZE: &str = "256G";
const ARCH_IMAGE_URL: &str = "https://geo.mirror.pkgbuild.com/images/latest/Arch-Linux-x86_64-cloudimg.qcow2";

/// Check if QEMU is available
fn check_qemu() -> Result<()> {
    which::which("qemu-system-x86_64")
        .context("qemu-system-x86_64 not found. Install QEMU.")?;
    Ok(())
}

/// Check if VM is running
fn is_running() -> bool {
    if let Ok(pid_str) = fs::read_to_string(pid_file()) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            return std::path::Path::new(&format!("/proc/{}", pid)).exists();
        }
    }
    false
}

/// Start the VM
pub fn start(
    detach: bool,
    gui: bool,
    memory: u32,
    cpus: u32,
    cdrom: Option<String>,
    uefi: bool,
) -> Result<()> {
    check_qemu()?;

    if is_running() {
        bail!("VM is already running. Use 'cargo xtask vm stop' first.");
    }

    let disk = disk_image();
    if !disk.exists() {
        bail!(
            "Disk image not found at {:?}\nRun 'cargo xtask vm setup' first.",
            disk
        );
    }

    // Ensure cloud-init config exists
    let cloud_init_iso = vm_dir().join("cloud-init.iso");
    if !cloud_init_iso.exists() {
        create_cloud_init_iso(&cloud_init_iso)?;
    }

    let mut args = vec![
        "-enable-kvm".to_string(),
        "-cpu".to_string(), "host".to_string(),
        "-m".to_string(), format!("{}M", memory),
        "-smp".to_string(), format!("{}", cpus),
        // Disk
        "-drive".to_string(),
        format!("file={},format=qcow2,if=virtio", disk.display()),
        // Cloud-init config
        "-drive".to_string(),
        format!("file={},format=raw,if=virtio,readonly=on", cloud_init_iso.display()),
        // Network with SSH forwarding
        "-netdev".to_string(),
        format!("user,id=net0,hostfwd=tcp::{}-:22", SSH_PORT),
        "-device".to_string(), "virtio-net-pci,netdev=net0".to_string(),
        // Monitor socket for control
        "-monitor".to_string(),
        format!("unix:{},server,nowait", monitor_socket().display()),
        // PID file
        "-pidfile".to_string(), pid_file().display().to_string(),
    ];

    // UEFI boot (requires OVMF package)
    if uefi {
        let ovmf_paths = [
            "/usr/share/edk2/ovmf/OVMF_CODE.fd",           // Fedora/RHEL
            "/usr/share/OVMF/OVMF_CODE.fd",                // Debian/Ubuntu
            "/usr/share/edk2-ovmf/x64/OVMF_CODE.fd",       // Arch
            "/usr/share/qemu/OVMF_CODE.fd",                // Generic
        ];

        let ovmf = ovmf_paths.iter().find(|p| std::path::Path::new(p).exists());
        match ovmf {
            Some(path) => {
                args.extend([
                    "-drive".to_string(),
                    format!("if=pflash,format=raw,readonly=on,file={}", path),
                ]);
            }
            None => {
                bail!("OVMF not found. Install edk2-ovmf package for UEFI support.");
            }
        }
    }

    // CDROM/ISO for installation
    if let Some(iso) = &cdrom {
        let iso_path = if iso == "arch" {
            let auto_path = vm_dir().join("arch.iso");
            if !auto_path.exists() {
                bail!("Arch ISO not found at {:?}\nDownload from https://archlinux.org/download/", auto_path);
            }
            auto_path.display().to_string()
        } else {
            iso.clone()
        };

        if !std::path::Path::new(&iso_path).exists() {
            bail!("ISO file not found: {}", iso_path);
        }

        args.extend([
            "-cdrom".to_string(), iso_path.clone(),
            "-boot".to_string(), "d".to_string(),
        ]);
        println!("  CDROM: {}", iso_path);
    }

    if gui {
        args.extend([
            "-device".to_string(), "virtio-vga-gl".to_string(),
            "-display".to_string(), "gtk,gl=on".to_string(),
            "-device".to_string(), "virtio-keyboard".to_string(),
            "-device".to_string(), "virtio-mouse".to_string(),
            "-device".to_string(), "intel-hda".to_string(),
            "-device".to_string(), "hda-duplex".to_string(),
            "-serial".to_string(), "none".to_string(),
        ]);
    } else {
        args.extend([
            "-nographic".to_string(),
            "-serial".to_string(), "mon:stdio".to_string(),
        ]);
    }

    if detach && !gui {
        args.push("-daemonize".to_string());
    }

    println!("Starting VM...");
    println!("  Memory: {} MB", memory);
    println!("  CPUs: {}", cpus);
    println!("  SSH: localhost:{}", SSH_PORT);
    println!("  GUI: {}", if gui { "enabled" } else { "disabled" });
    println!("  UEFI: {}", if uefi { "enabled" } else { "disabled (BIOS)" });

    let status = Command::new("qemu-system-x86_64")
        .args(&args)
        .status()
        .context("Failed to start QEMU")?;

    if detach && !gui {
        if status.success() {
            println!("\nVM started in background.");
            println!("  SSH: ssh -p {} arch@localhost", SSH_PORT);
            println!("  Stop: cargo xtask vm stop");
        } else {
            bail!("Failed to start VM");
        }
    }

    Ok(())
}

/// Stop the VM
pub fn stop() -> Result<()> {
    if !is_running() {
        println!("VM is not running.");
        return Ok(());
    }

    let monitor = monitor_socket();
    if monitor.exists() {
        println!("Sending shutdown signal...");
        let _ = Command::new("sh")
            .args(["-c", &format!("echo 'system_powerdown' | socat - UNIX-CONNECT:{}", monitor.display())])
            .status();
        std::thread::sleep(std::time::Duration::from_secs(3));
    }

    if is_running() {
        if let Ok(pid_str) = fs::read_to_string(pid_file()) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                println!("Force killing VM (PID {})...", pid);
                let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }
        }
    }

    let _ = fs::remove_file(pid_file());
    let _ = fs::remove_file(monitor_socket());

    println!("VM stopped.");
    Ok(())
}

/// Show VM status
pub fn status() -> Result<()> {
    if is_running() {
        let pid = fs::read_to_string(pid_file())
            .unwrap_or_default()
            .trim()
            .to_string();
        println!("VM is running (PID {})", pid);
        println!("  SSH: ssh -p {} arch@localhost", SSH_PORT);
    } else {
        println!("VM is not running.");
    }

    let disk = disk_image();
    if disk.exists() {
        let meta = fs::metadata(&disk)?;
        println!("  Disk: {:?} ({:.1} GB)", disk, meta.len() as f64 / 1e9);
    } else {
        println!("  Disk: not created (run 'cargo xtask vm setup')");
    }

    Ok(())
}

/// Send command to VM via SSH
pub fn send(command: &str) -> Result<()> {
    if !is_running() {
        bail!("VM is not running. Start it with 'cargo xtask vm start'");
    }

    let status = Command::new("ssh")
        .args([
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR",
            "-p", &SSH_PORT.to_string(),
            "arch@localhost",
            command,
        ])
        .status()
        .context("Failed to SSH")?;

    if !status.success() {
        bail!("Command failed with exit code {:?}", status.code());
    }

    Ok(())
}

/// Show serial log
pub fn log(follow: bool) -> Result<()> {
    let log = serial_log();
    if !log.exists() {
        bail!("No log file found. Has the VM been started?");
    }

    if follow {
        Command::new("tail")
            .args(["-f", &log.display().to_string()])
            .status()
            .context("Failed to tail log")?;
    } else {
        let content = fs::read_to_string(&log)?;
        println!("{}", content);
    }

    Ok(())
}

/// SSH into VM
pub fn ssh() -> Result<()> {
    if !is_running() {
        bail!("VM is not running. Start it with 'cargo xtask vm start'");
    }

    Command::new("ssh")
        .args([
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-p", &SSH_PORT.to_string(),
            "arch@localhost",
        ])
        .status()
        .context("Failed to SSH")?;

    Ok(())
}

/// Setup/create the base Arch Linux image from cloud image
pub fn setup(force: bool) -> Result<()> {
    check_qemu()?;

    let disk = disk_image();
    if disk.exists() && !force {
        println!("Disk image already exists at {:?}", disk);
        println!("Use --force to recreate.");
        return Ok(());
    }

    println!("=== Recipe VM Setup ===\n");

    which::which("qemu-img").context("qemu-img not found")?;

    println!("[1/2] Downloading Arch Linux cloud image (~500MB)...");
    let status = Command::new("curl")
        .args([
            "-L",
            "--progress-bar",
            "-o", &disk.display().to_string(),
            ARCH_IMAGE_URL,
        ])
        .status()
        .context("Failed to run curl")?;

    if !status.success() {
        bail!("Failed to download Arch cloud image");
    }
    println!("Downloaded: {:?}", disk);

    println!("[2/2] Resizing disk to {}...", DISK_SIZE);
    let status = Command::new("qemu-img")
        .args(["resize", &disk.display().to_string(), DISK_SIZE])
        .status()?;

    if !status.success() {
        bail!("Failed to resize disk image");
    }

    println!("\n=== Setup Complete ===\n");
    println!("Arch Linux cloud image ready: {:?}", disk);
    println!();
    println!("Default credentials:");
    println!("  Username: arch");
    println!("  Password: arch");
    println!();
    println!("Next steps:");
    println!("  1. cargo xtask vm prepare     # Build recipe binary");
    println!("  2. cargo xtask vm start --gui # Boot VM");
    println!("  3. cargo xtask vm copy        # Copy recipe to VM");
    println!("  4. recipe install <package>   # Install packages");
    println!();

    Ok(())
}

/// Build recipe binary and prepare files for VM
pub fn prepare() -> Result<()> {
    println!("=== Preparing recipe files for VM ===\n");

    println!("[1/2] Building recipe binary...");
    let status = Command::new("cargo")
        .args(["build", "--release", "--bin", "recipe"])
        .current_dir(project_root())
        .status()
        .context("Failed to build recipe")?;

    if !status.success() {
        bail!("Failed to build recipe binary");
    }

    let recipe_src = project_root().join("target/release/recipe");
    let recipe_dst = vm_dir().join("recipe");

    if recipe_src.exists() {
        fs::copy(&recipe_src, &recipe_dst)?;
        println!("   Built: {:?}", recipe_dst);
    } else {
        bail!("Binary not found at {:?}", recipe_src);
    }

    println!("[2/2] Recipes ready in examples/");
    let recipes_dir = project_root().join("examples");
    let count = fs::read_dir(&recipes_dir)?
        .filter(|e| e.as_ref().map(|e| e.path().extension().map(|x| x == "recipe").unwrap_or(false)).unwrap_or(false))
        .count();
    println!("   Found {} recipes", count);

    println!("\n=== Preparation Complete ===\n");
    println!("Next: cargo xtask vm start --gui");

    Ok(())
}

/// Show the install script to run inside VM
pub fn install_script() -> Result<()> {
    println!("=== Testing recipes in the VM ===\n");
    println!("# After VM boots and you SSH in:");
    println!();
    println!("# The recipe binary and examples are copied to:");
    println!("#   /usr/local/bin/recipe");
    println!("#   /usr/share/recipe/examples/");
    println!();
    println!("# Test a recipe:");
    println!("recipe install /usr/share/recipe/examples/ripgrep.recipe");
    println!();
    println!("# Or run interactively:");
    println!("recipe install /usr/share/recipe/examples/fd.recipe --dry-run");

    Ok(())
}

/// Copy recipe binary and recipes to running VM via SCP
pub fn copy_files() -> Result<()> {
    if !is_running() {
        bail!("VM is not running. Start it first with: cargo xtask vm start --gui");
    }

    let recipe_bin = vm_dir().join("recipe");
    let recipes_dir = project_root().join("examples");

    if !recipe_bin.exists() {
        bail!("Recipe binary not found. Run: cargo xtask vm prepare");
    }

    let has_sshpass = which::which("sshpass").is_ok();
    if !has_sshpass {
        println!("Tip: Install 'sshpass' to avoid password prompts\n");
    }

    println!("=== Copying files to VM ===\n");

    let run_ssh = |args: &[&str]| -> std::io::Result<std::process::ExitStatus> {
        let mut cmd_args = vec![
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR",
            "-p", "2222",
            "arch@localhost",
        ];
        cmd_args.extend(args);

        if has_sshpass {
            Command::new("sshpass")
                .args(["-p", "arch", "ssh"])
                .args(&cmd_args)
                .status()
        } else {
            Command::new("ssh")
                .args(&cmd_args)
                .status()
        }
    };

    let run_scp = |src: &str, dst: &str| -> std::io::Result<std::process::ExitStatus> {
        let scp_args = [
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-o", "LogLevel=ERROR",
            "-P", "2222",
            src,
            dst,
        ];

        if has_sshpass {
            Command::new("sshpass")
                .args(["-p", "arch", "scp"])
                .args(&scp_args)
                .status()
        } else {
            Command::new("scp")
                .args(&scp_args)
                .status()
        }
    };

    println!("[1/4] Copying recipe binary...");
    let status = run_scp(&recipe_bin.display().to_string(), "arch@localhost:/tmp/recipe")?;
    if !status.success() {
        bail!("Failed to copy recipe binary. Is SSH running in the VM?");
    }

    println!("[2/4] Installing recipe...");
    run_ssh(&["sudo", "install", "-m755", "/tmp/recipe", "/usr/local/bin/recipe"])?;

    println!("[3/4] Setting up examples directory...");
    run_ssh(&["sudo", "mkdir", "-p", "/usr/share/recipe/examples"])?;
    run_ssh(&["sudo", "chown", "-R", "arch:arch", "/usr/share/recipe"])?;

    println!("[4/4] Copying recipes...");
    let tar_file = vm_dir().join("recipes.tar");

    Command::new("tar")
        .args(["-cf", &tar_file.display().to_string(), "-C", &recipes_dir.display().to_string(), "."])
        .status()?;

    run_scp(&tar_file.display().to_string(), "arch@localhost:/tmp/recipes.tar")?;
    run_ssh(&["tar", "-xf", "/tmp/recipes.tar", "-C", "/usr/share/recipe/examples/"])?;
    run_ssh(&["rm", "/tmp/recipes.tar", "/tmp/recipe"])?;

    let count = fs::read_dir(&recipes_dir)?
        .filter(|e| e.as_ref().map(|e| e.path().extension().map(|x| x == "recipe").unwrap_or(false)).unwrap_or(false))
        .count();

    println!("\n=== Copy Complete ({} recipes) ===\n", count);
    println!("Run in VM:");
    println!("  recipe install /usr/share/recipe/examples/ripgrep.recipe");
    println!("  recipe install /usr/share/recipe/examples/fd.recipe --dry-run");

    Ok(())
}

/// Create a cloud-init ISO to configure the Arch cloud image
fn create_cloud_init_iso(iso_path: &PathBuf) -> Result<()> {
    which::which("genisoimage")
        .or_else(|_| which::which("mkisofs"))
        .context("genisoimage or mkisofs not found. Install genisoimage package.")?;

    let cloud_dir = vm_dir().join("cloud-init");
    fs::create_dir_all(&cloud_dir)?;

    let meta_data = cloud_dir.join("meta-data");
    fs::write(&meta_data, "instance-id: recipe-test\nlocal-hostname: recipe-test\n")?;

    let user_data = cloud_dir.join("user-data");
    fs::write(&user_data, r#"#cloud-config
users:
  - name: arch
    plain_text_passwd: arch
    lock_passwd: false
    sudo: ALL=(ALL) NOPASSWD:ALL
    groups: wheel
    shell: /bin/bash

ssh_pwauth: true
disable_root: false

chpasswd:
  expire: false

packages:
  - openssh-server
  - sudo
  - base-devel
  - meson
  - ninja
  - cmake
  - pkg-config
  - git

runcmd:
  - systemctl enable --now sshd
  - growpart /dev/vda 2 || true
  - resize2fs /dev/vda2 || true
"#)?;

    let iso_tool = which::which("genisoimage")
        .unwrap_or_else(|_| which::which("mkisofs").unwrap());

    let status = Command::new(&iso_tool)
        .args([
            "-output", &iso_path.display().to_string(),
            "-volid", "cidata",
            "-joliet",
            "-rock",
            &cloud_dir.display().to_string(),
        ])
        .output()
        .context("Failed to create cloud-init ISO")?;

    if !status.status.success() {
        bail!("Failed to create cloud-init ISO: {}", String::from_utf8_lossy(&status.stderr));
    }

    println!("Created cloud-init config: {:?}", iso_path);
    Ok(())
}
