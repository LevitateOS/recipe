//! `recipe runtime`: show, fetch and stage the runtime Recipe runs recipes in
//! (Ginux: its CLI, the root-owned helper tools, Recipe's root-owned guard,
//! and the runtime images as OCI image layouts; docs/ginux-cutover-design.md, sections 3 and 4).
//!
//! `stage` uses plain `std::fs` only, so the static binary can run it inside a
//! Ginux instance.

use anyhow::{Context, Result, bail, ensure};
use levitate_recipe::runtime;
use std::io::IsTerminal;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use super::cli::{Cli, RuntimeCommands, StageArgs};

const PROFILE: &str = include_str!("../../../data/profile.sh");
const SUDOERS: &str = include_str!("../../../data/sudoers-recipe-ginux");
const OFFICIAL_SOURCE: &str = "files = \"https://raw.githubusercontent.com/LevitateOS/recipes/{rev}/{path}\"\nbranch = \"master\"\n";

pub(crate) fn execute(command: &RuntimeCommands, cli: &Cli) -> Result<()> {
    match command {
        RuntimeCommands::Status => status(cli),
        RuntimeCommands::Fetch => fetch(cli),
        RuntimeCommands::Remove => {
            // The downloaded layouts (decision 26). One installed with the
            // system, one chosen with RECIPE_IMAGE_LAYOUT and the images
            // imported into Ginux's cache stay.
            let freed = runtime::remove_build_image()?;
            let image = runtime::build_image()?;
            let result = serde_json::json!({"schema_version": 1, "freed_bytes": freed,
                "path": image.path, "remaining_bytes": image.bytes,
                "system_bytes": image.system_bytes});
            let json = cli.json || !std::io::stdout().is_terminal();
            if let Some(path) = &cli.json_output {
                std::fs::write(path, serde_json::to_string(&result)?)
                    .with_context(|| format!("cannot write {}", path.display()))?;
            }
            if json && cli.json_output.is_none() {
                println!("{}", serde_json::to_string(&result)?);
            } else if !json {
                println!(
                    "Deleted {freed} bytes of downloaded runtime images from {}.{}",
                    image.path.display(),
                    if image.bytes > 0 {
                        " The image installed with the system stays."
                    } else {
                        " An image is downloaded again when a recipe needs one that Ginux has not imported."
                    }
                );
                if image.system_bytes > 0 {
                    println!(
                        "The {} bytes of images imported into Ginux's cache under {} stay: Ginux has no command that deletes an imported image.",
                        image.system_bytes,
                        runtime::state_root().unwrap_or_else(|_| runtime::STATE_ROOT.into())
                    );
                }
            }
            Ok(())
        }
        RuntimeCommands::Stage(args) => {
            let StageArgs {
                root,
                prefix,
                ginux_cli,
                ginux_helper,
                ginux_image,
                ginux_runtime,
                ginux_allocator,
                recipe_guard,
                reference,
                image,
                minimal_reference,
                minimal_image,
                catalog,
                sudoers,
            } = &**args;
            stage(&Stage {
                root,
                prefix,
                ginux: GinuxFiles {
                    cli: ginux_cli,
                    helper: ginux_helper,
                    image: ginux_image,
                    runtime: ginux_runtime,
                    allocator: ginux_allocator,
                },
                guard: recipe_guard,
                full: (reference, image.as_deref()),
                minimal: minimal_reference
                    .as_deref()
                    .map(|reference| (reference, minimal_image.as_deref())),
                catalog,
                sudoers: sudoers.as_deref(),
            })
        }
    }
}

/// `recipe runtime fetch`: make each selected image ready in Ginux, the
/// full one, then the minimal one when one is named: unless it is imported
/// already, download its layout when it is not here and import it, then add
/// its reference to the guard's allowlist ([`runtime::IMAGE_ALLOWLIST`]) so
/// callers other than root may build instances from it. Importing and the
/// allowlist are root's: the helper applies an image as root. An image
/// imported already is in the cache under its manifest digest, so it is the
/// content its reference names and is allowed as well. One line per image:
/// its reference and where it came from.
fn fetch(cli: &Cli) -> Result<()> {
    runtime::set_fetch_events(cli.machine_events);
    let state_root = runtime::state_root()?;
    let full = runtime::root_reference()?;
    let mut images = vec![(runtime::Image::Full, full.clone())];
    if let Some(minimal) = runtime::minimal_reference()?.filter(|m| *m != full) {
        images.push((runtime::Image::Minimal, minimal));
    }
    let allowed = runtime::allowed_images(Path::new(runtime::IMAGE_ALLOWLIST)).unwrap_or_default();
    for (image, reference) in images {
        let digest = runtime::root_digest(&reference)?.to_owned();
        let imported = runtime::imported(&state_root, &digest);
        if !is_root() {
            ensure!(
                imported && allowed.contains(&reference),
                "the runtime image {reference} is {}: importing it and allowing it for Recipe's guard are root's; run `sudo recipe runtime fetch`",
                if imported {
                    format!("imported but not listed in {}", runtime::IMAGE_ALLOWLIST)
                } else {
                    "not imported".to_owned()
                }
            );
            println!("{reference} imported already");
            continue;
        }
        if imported {
            println!("{reference} imported already");
        } else {
            let layout = runtime::ensure_layout(&reference, image)?;
            levitate_recipe::bin_support::import_image(&reference, &layout)?;
            println!("{reference} imported from {}", layout.display());
        }
        runtime::allow_image(&reference)?;
    }
    Ok(())
}

/// The state of one root-owned Ginux file.
fn file_state(path: &Path) -> serde_json::Value {
    match std::fs::metadata(path) {
        Ok(meta) => serde_json::json!({
            "path": path,
            "present": meta.is_file(),
            "root_owned": meta.uid() == 0,
            "mode": format!("{:04o}", meta.permissions().mode() & 0o7777),
            // Run as root: nobody else may be able to change it.
            "safe": meta.is_file() && meta.uid() == 0 && meta.permissions().mode() & 0o022 == 0,
        }),
        Err(_) => serde_json::json!({"path": path, "present": false}),
    }
}

/// Whether this user may run Recipe's guard on Recipe's roots through sudo
/// without a password: `sudo -n -l -- GUARD --protocol 1 --state-root S
/// --run-root R status recipe-<uid>-probe` lists the command when the rule
/// matches it (the rule pins the arguments, so the full form of a real call
/// is asked, with a harmless verb); it runs nothing. The rule names the
/// guard, never the helper.
fn sudo_allowed(guard: &Path, state_root: &str, run_root: &str) -> bool {
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    std::process::Command::new("sudo")
        .args(["-n", "-l", "--"])
        .arg(guard)
        .args([
            "--protocol",
            "1",
            "--state-root",
            state_root,
            "--run-root",
            run_root,
        ])
        .args(["status", &format!("recipe-{uid}-probe")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Where `image`'s layout, selected at `path`, comes from.
fn selected_from(image: runtime::Image, path: &Path) -> &'static str {
    if std::env::var_os(image.layout_env()).is_some() {
        image.layout_env()
    } else if path == image.shipped_layout() {
        "installed with Recipe"
    } else {
        "download cache"
    }
}

fn status(cli: &Cli) -> Result<()> {
    runtime::refuse_gone_variables()?;
    let reference = runtime::root_reference()?;
    let digest = runtime::root_digest(&reference)?.to_owned();
    let origin = runtime::reference_origin(runtime::Image::Full);
    let (ginux_cli, cli_from) = match std::env::var_os(runtime::GINUX_ENV) {
        Some(path) if !path.is_empty() => (PathBuf::from(path), runtime::GINUX_ENV),
        _ => (runtime::ginux_cli(), "install prefix"),
    };
    let cli_ok = ginux_cli.is_file()
        && std::fs::metadata(&ginux_cli).is_ok_and(|m| m.permissions().mode() & 0o111 != 0);
    let helper = runtime::ginux_helper();
    let helper_from = if std::env::var_os(runtime::HELPER_ENV).is_some_and(|v| !v.is_empty()) {
        runtime::HELPER_ENV
    } else {
        "installed"
    };
    let state_root = runtime::state_root()?;
    let run_root = runtime::run_root()?;
    let guard = runtime::guard_helper();
    let allowed = is_root() || sudo_allowed(&guard, &state_root, &run_root);
    let image = runtime::layout_path(&reference, runtime::Image::Full)?;
    let image_from = selected_from(runtime::Image::Full, &image);
    let imported = runtime::imported(&state_root, &digest);
    // The guard's allowlist: which images callers other than root may use.
    let allowlist = runtime::allowed_images(Path::new(runtime::IMAGE_ALLOWLIST));
    let listed = |reference: &str| {
        allowlist
            .as_ref()
            .is_ok_and(|references| references.iter().any(|listed| listed == reference))
    };
    let allowlisted = listed(&reference);
    // The minimal image, when one is named: (reference, digest, layout, from, imported, allowlisted).
    let minimal = match runtime::minimal_reference()? {
        Some(reference) => {
            let digest = runtime::root_digest(&reference)?.to_owned();
            let path = runtime::layout_path(&reference, runtime::Image::Minimal)?;
            let from = selected_from(runtime::Image::Minimal, &path);
            let imported = runtime::imported(&state_root, &digest);
            let allowlisted = listed(&reference);
            Some((reference, digest, path, from, imported, allowlisted))
        }
        None => None,
    };
    let minimal_origin = runtime::reference_origin(runtime::Image::Minimal);
    let sources = runtime::shipped_sources();
    let tools = serde_json::json!({
        "guard": file_state(&guard),
        "helper": file_state(&helper),
        "image_tool": file_state(Path::new(runtime::GINUX_IMAGE_TOOL)),
        "runtime": file_state(Path::new(runtime::GINUX_RUNTIME)),
        "allocator": file_state(Path::new(runtime::GINUX_ALLOCATOR)),
    });
    let result = serde_json::json!({
        "schema_version": 2,
        "reference": reference,
        "reference_from": origin,
        "digest": digest,
        "prefix": runtime::prefix(),
        "ginux": {"path": ginux_cli, "from": cli_from, "exists": ginux_cli.exists(),
            "executable": cli_ok},
        "helper_from": helper_from,
        "tools": tools,
        "state_root": state_root,
        "run_root": run_root,
        "sudo": allowed,
        "image": {"path": image, "from": image_from, "present": image.exists(),
            "imported": imported, "allowlisted": allowlisted},
        "image_allowlist": {"path": runtime::IMAGE_ALLOWLIST, "trusted": allowlist.is_ok(),
            "problem": allowlist.as_ref().err().map(|error| format!("{error:#}"))},
        "minimal": minimal.as_ref().map(|(reference, digest, path, from, imported, allowlisted)| serde_json::json!({
            "reference": reference, "reference_from": minimal_origin, "digest": digest,
            "image": {"path": path, "from": from, "present": path.exists(),
                "imported": imported, "allowlisted": allowlisted}})),
        "shipped_sources": {"path": sources, "exists": sources.exists()},
    });
    let json = cli.json || !std::io::stdout().is_terminal();
    if let Some(path) = &cli.json_output {
        std::fs::write(path, serde_json::to_string(&result)?)
            .with_context(|| format!("cannot write {}", path.display()))?;
        if !json {
            println!("Runtime status saved to {}.", path.display());
        }
    } else if json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        let presence = |path: &Path, imported: bool| {
            if imported {
                "imported into Ginux"
            } else if path.exists() {
                "present, imported on first use or with 'recipe runtime fetch'"
            } else {
                "absent, downloaded and imported on first use or with 'recipe runtime fetch'"
            }
        };
        println!("Runtime image: {reference}\n  selected by: {origin}");
        println!(
            "ginux CLI:     {} ({}; {})",
            ginux_cli.display(),
            cli_from,
            match (ginux_cli.exists(), cli_ok) {
                (false, _) => "missing",
                (true, false) => "present, not executable",
                (true, true) => "present, executable",
            }
        );
        for (label, key) in [
            ("Guard:", "guard"),
            ("Helper:", "helper"),
            ("Image tool:", "image_tool"),
            ("Runtime:", "runtime"),
            ("Allocator:", "allocator"),
        ] {
            let state = &result["tools"][key];
            println!(
                "{label:<15}{} ({})",
                state["path"].as_str().unwrap_or_default(),
                match (state["present"] == true, state["safe"] == true) {
                    (false, _) => "missing".to_owned(),
                    (true, true) =>
                        format!("root-owned, mode {}", state["mode"].as_str().unwrap_or("")),
                    (true, false) => format!(
                        "NOT root-owned or writable by others (mode {}); it runs as root",
                        state["mode"].as_str().unwrap_or("")
                    ),
                }
            );
        }
        println!("State root:    {state_root}\nRun root:      {run_root}");
        println!(
            "sudo:          {}",
            if allowed {
                "this user may run the guard on these roots"
            } else {
                "this user may not run the guard without a password: Ginux runs need root, or the sudoers rule /etc/sudoers.d/recipe-ginux for one of this user's groups"
            }
        );
        let allowed_text = |allowlisted: bool| {
            if allowlisted {
                "allowed for Recipe's guard".to_owned()
            } else {
                format!(
                    "NOT in {}: only root can build instances from it; run `sudo recipe runtime fetch`",
                    runtime::IMAGE_ALLOWLIST
                )
            }
        };
        println!(
            "Image layout:  {} ({}; {})",
            image.display(),
            image_from,
            presence(&image, imported)
        );
        println!("  allowlist:   {}", allowed_text(allowlisted));
        if let Err(error) = &allowlist {
            println!(
                "Allowlist:     {} is not trusted ({error:#}); the guard allows no image for callers other than root",
                runtime::IMAGE_ALLOWLIST
            );
        }
        match &minimal {
            Some((reference, _, path, from, imported, allowlisted)) => {
                println!("Minimal image: {reference}\n  selected by: {minimal_origin}");
                println!(
                    "Minimal layout: {} ({}; {})",
                    path.display(),
                    from,
                    presence(path, *imported)
                );
                println!("  allowlist:   {}", allowed_text(*allowlisted));
            }
            None => println!(
                "Minimal image: none named; recipes with NEEDS.runtime \"minimal\" run in the full image"
            ),
        }
        println!(
            "Sources:       {}{}",
            sources.display(),
            if sources.exists() { "" } else { " (absent)" }
        );
    }
    Ok(())
}

fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// Create `path` and any missing parents as 0755.
fn mkdirs(path: &Path) -> Result<()> {
    let mut missing: Vec<&Path> = path
        .ancestors()
        .take_while(|p| !p.as_os_str().is_empty() && !p.exists())
        .collect();
    missing.reverse();
    for directory in missing {
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(directory)
            .with_context(|| format!("cannot create {}", directory.display()))?;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Replace `destination` with `content` at `mode`.
fn put(destination: &Path, content: &[u8], mode: u32) -> Result<()> {
    mkdirs(
        destination
            .parent()
            .context("destination has no directory")?,
    )?;
    remove_if_present(destination)?;
    std::fs::write(destination, content)
        .with_context(|| format!("cannot write {}", destination.display()))?;
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(mode))?;
    eprintln!("wrote {}", destination.display());
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => bail!(
            "{} is a directory; cannot replace it with a file",
            path.display()
        ),
        Ok(_) => {
            std::fs::remove_file(path).with_context(|| format!("cannot replace {}", path.display()))
        }
        Err(_) => Ok(()),
    }
}

fn copy_file(source: &Path, destination: &Path, mode: u32) -> Result<()> {
    mkdirs(
        destination
            .parent()
            .context("destination has no directory")?,
    )?;
    remove_if_present(destination)?;
    std::fs::copy(source, destination).with_context(|| {
        format!(
            "cannot copy {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(mode))?;
    eprintln!("wrote {}", destination.display());
    Ok(())
}

/// Copy the files of `source` into `destination`, keeping links; directories
/// 0755, files 0644 unless executable. Git's own `.git` entries are left out.
fn copy_tree(source: &Path, destination: &Path) -> Result<usize> {
    mkdirs(destination)?;
    let mut count = 0;
    for entry in
        std::fs::read_dir(source).with_context(|| format!("cannot read {}", source.display()))?
    {
        let entry = entry?;
        if entry.file_name().as_bytes() == b".git" {
            continue;
        }
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let meta = std::fs::symlink_metadata(&from)?;
        if meta.is_dir() {
            count += copy_tree(&from, &to)?;
        } else if meta.file_type().is_symlink() {
            remove_if_present(&to)?;
            std::os::unix::fs::symlink(std::fs::read_link(&from)?, &to)
                .with_context(|| format!("cannot link {}", to.display()))?;
            count += 1;
        } else if meta.is_file() {
            let mode = if meta.mode() & 0o111 != 0 {
                0o755
            } else {
                0o644
            };
            remove_if_present(&to)?;
            std::fs::copy(&from, &to)
                .with_context(|| format!("cannot copy {} to {}", from.display(), to.display()))?;
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(mode))?;
            count += 1;
        }
    }
    Ok(count)
}

/// Check that `layout`, when given, is the image layout tar `reference`
/// names: its manifest digest (from `index.json` and the manifest blob) is
/// the reference's. Ginux verifies every blob when it imports it.
fn check_image(reference: &str, layout: Option<&Path>, option: &str) -> Result<()> {
    runtime::validate_reference(reference)?;
    let digest = runtime::root_digest(reference)?;
    if let Some(layout) = layout {
        ensure!(
            layout.is_file(),
            "{option} {} is not a file; pass the image layout's tar (image.oci.tar from recipe-image)",
            layout.display()
        );
        let actual = levitate_recipe::image::layout_manifest_digest(layout)?;
        ensure!(
            actual == digest,
            "the image layout {} has manifest digest {actual}, not {digest} from the reference; pass the matching layout or leave out {option}",
            layout.display()
        );
    }
    Ok(())
}

/// The five Ginux files a runtime tree ships.
struct GinuxFiles<'a> {
    cli: &'a Path,
    helper: &'a Path,
    image: &'a Path,
    runtime: &'a Path,
    allocator: &'a Path,
}

/// What `recipe runtime stage` writes. `full` and `minimal` are each an
/// image reference with the layout tar to ship, if any; without a minimal
/// reference the install names no minimal image.
struct Stage<'a> {
    root: &'a Path,
    prefix: &'a Path,
    ginux: GinuxFiles<'a>,
    /// Recipe's guard (`recipe-ginux-guard`), shipped root-owned 0755.
    guard: &'a Path,
    full: (&'a str, Option<&'a Path>),
    minimal: Option<(&'a str, Option<&'a Path>)>,
    catalog: &'a Path,
    sudoers: Option<&'a str>,
}

/// A group name sudoers takes after `%`.
fn group_name(group: &str) -> bool {
    !group.is_empty()
        && group.len() <= 32
        && group
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && group
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

fn stage(stage: &Stage<'_>) -> Result<()> {
    let Stage {
        root,
        prefix,
        ginux,
        guard,
        full,
        minimal,
        catalog,
        sudoers,
    } = stage;
    check_image(full.0, full.1, "--image")?;
    if let Some((reference, image)) = minimal {
        check_image(reference, *image, "--minimal-image")?;
    }
    for (option, path) in [
        ("--ginux-cli", ginux.cli),
        ("--ginux-helper", ginux.helper),
        ("--ginux-image", ginux.image),
        ("--ginux-runtime", ginux.runtime),
        ("--ginux-allocator", ginux.allocator),
        ("--recipe-guard", *guard),
    ] {
        ensure!(
            path.is_file(),
            "{option} {} is not a file; pass the Ginux file to ship",
            path.display()
        );
    }
    if let Some(group) = sudoers {
        ensure!(
            group_name(group),
            "--sudoers {group:?} is not a group name (lowercase letters, digits, `_` and `-`)"
        );
        // The rule names the guard and pins the roots Recipe uses; it must
        // never name the helper (any verb of which is root). The template
        // must agree.
        ensure!(
            SUDOERS.contains(runtime::GUARD_HELPER)
                && SUDOERS.contains(runtime::STATE_ROOT)
                && SUDOERS.contains(runtime::RUN_ROOT)
                && !SUDOERS
                    .lines()
                    .any(|line| !line.starts_with('#') && line.contains(runtime::GINUX_HELPER)),
            "data/sudoers-recipe-ginux does not name Recipe's guard and roots, or names the helper"
        );
    }
    ensure!(
        catalog.join("index.json").is_file(),
        "{} has no index.json; run 'recipe index' in that catalog and commit it",
        catalog.display()
    );
    let prefix_text = prefix.to_str().context("--prefix is not valid UTF-8")?;
    ensure!(
        prefix_text.starts_with('/')
            && prefix_text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-')),
        "--prefix {prefix_text:?} must be an absolute path of letters, digits and `/._-`"
    );

    let base = root.join(prefix_text.trim_start_matches('/'));
    let under_root = |path: &str| root.join(path.trim_start_matches('/'));
    // The CLI lives with Recipe; the guard, the helper and the tools it runs
    // as root are at the paths the sudoers rule, the CLI and the helper name,
    // whatever the prefix. They must end up root-owned and writable by nobody
    // else: the package or installer that installs this tree owns them as root.
    copy_file(guard, &under_root(runtime::GUARD_HELPER), 0o755)?;
    copy_file(ginux.cli, &base.join("libexec/recipe/ginux"), 0o755)?;
    copy_file(ginux.helper, &under_root(runtime::GINUX_HELPER), 0o755)?;
    copy_file(ginux.image, &under_root(runtime::GINUX_IMAGE_TOOL), 0o755)?;
    copy_file(ginux.runtime, &under_root(runtime::GINUX_RUNTIME), 0o755)?;
    copy_file(
        ginux.allocator,
        &under_root(runtime::GINUX_ALLOCATOR),
        0o755,
    )?;
    let runtime_dir = base.join("share/recipe/runtime");
    for (selected, reference_name, image_name) in [
        (Some(*full), "root-reference", "compiler.oci.tar"),
        (*minimal, "minimal-reference", "minimal.oci.tar"),
    ] {
        let Some((reference, image)) = selected else {
            // No minimal image named: one an earlier stage shipped goes, so
            // the install never names an image this stage did not choose.
            for name in [reference_name, image_name] {
                remove_file_if_present(&runtime_dir.join(name))?;
            }
            continue;
        };
        put(
            &runtime_dir.join(reference_name),
            format!("{reference}\n").as_bytes(),
            0o644,
        )?;
        let image_path = runtime_dir.join(image_name);
        if let Some(image) = image {
            copy_file(image, &image_path, 0o644)?;
        } else {
            // A new reference without a layout: a layout an earlier stage
            // shipped is for the old reference, so it goes.
            remove_file_if_present(&image_path)?;
        }
    }
    // The guard's allowlist: the references this tree ships, at the fixed
    // path the guard reads whatever the prefix. Root-owned once the package
    // or installer installs the tree as root.
    let allowlist = under_root(runtime::IMAGE_ALLOWLIST);
    mkdirs(
        allowlist
            .parent()
            .context("the allowlist path has no directory")?,
    )?;
    let references: Vec<String> = std::iter::once(full.0)
        .chain(minimal.map(|(reference, _)| reference))
        .map(str::to_owned)
        .collect();
    runtime::write_image_allowlist(&allowlist, &references)?;
    eprintln!("wrote {}", allowlist.display());
    let sources = base.join("share/recipe/sources");
    let files = copy_tree(catalog, &sources.join("levitate"))?;
    eprintln!(
        "wrote {} ({files} files)",
        sources.join("levitate").display()
    );
    put(
        &sources.join("levitate.toml"),
        OFFICIAL_SOURCE.as_bytes(),
        0o644,
    )?;

    let mut profile = PROFILE.to_owned();
    if prefix_text.trim_end_matches('/') != "/usr" {
        let bin = format!("{}/bin", prefix_text.trim_end_matches('/'));
        profile = profile.replacen(
            "export PATH\n",
            &format!(
                "case \":$PATH:\" in *:{bin}:*) ;; *) PATH=\"{bin}:$PATH\" ;; esac\nexport PATH\n"
            ),
            1,
        );
    }
    put(
        &root.join("etc/profile.d/recipe.sh"),
        profile.as_bytes(),
        0o644,
    )?;
    let sudoers_path = under_root(runtime::SUDOERS);
    match sudoers {
        Some(group) => put_sudoers(
            &sudoers_path,
            SUDOERS.replace("@GROUP@", group).as_bytes(),
            *root == Path::new("/"),
        )?,
        // A rule an earlier stage wrote goes with the choice.
        None => remove_file_if_present(&sudoers_path)?,
    }
    Ok(())
}

/// Install the sudoers rule at `destination` (never through a link): a
/// temporary file in the same directory whose name starts with `.` (sudo
/// skips such names in `sudoers.d`), mode 0440 before it is renamed, checked
/// with `visudo -cf` when `live` (staging into `/`), then renamed over
/// `destination`. Staged into another root, the tree's own sudo is not this
/// host's, so `visudo` is not run there; the installer of that tree checks it.
fn put_sudoers(destination: &Path, content: &[u8], live: bool) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let directory = destination
        .parent()
        .context("destination has no directory")?;
    mkdirs(directory)?;
    match std::fs::symlink_metadata(destination) {
        Ok(meta) if !meta.file_type().is_file() => bail!(
            "{} is a symbolic link or not a regular file; not replacing it (move it away)",
            destination.display()
        ),
        _ => {}
    }
    let name = destination
        .file_name()
        .context("destination has no file name")?
        .to_string_lossy();
    let temporary = directory.join(format!(".{name}.{}.new", std::process::id()));
    remove_file_if_present(&temporary)?;
    let written = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o440)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .with_context(|| format!("cannot create {}", temporary.display()))?;
        file.write_all(content)
            .with_context(|| format!("cannot write {}", temporary.display()))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o440))?;
        file.sync_all()?;
        drop(file);
        if live {
            let output = std::process::Command::new("visudo")
                .arg("-cf")
                .arg(&temporary)
                .stdin(std::process::Stdio::null())
                .output()
                .context("cannot run visudo to check the sudoers rule")?;
            ensure!(
                output.status.success(),
                "visudo refuses the sudoers rule, so it was not installed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        std::fs::rename(&temporary, destination)
            .with_context(|| format!("cannot move the rule to {}", destination.display()))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written?;
    eprintln!("wrote {}", destination.display());
    Ok(())
}

fn remove_file_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
    }
}
