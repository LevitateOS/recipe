//! Carrying shared libraries with a package (trial of the self-contained
//! packages design, LevitateOS docs/recipe-self-contained-packages.md).
//!
//! A program in a `package` output is told, when it is linked, the folder it
//! will live in once activated: its loader and `lib/` there are the only
//! places it loads from. `carry_libraries` then copies into `package/lib/`
//! the loader and every shared library the package's programs load, from the
//! folders the recipe names and from the build image, and refuses a program
//! that would still look anywhere else. Nothing then comes from the host's
//! own library folders, so the host's C library does not matter.

use rhai::EvalAltResult;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where the build image keeps its libraries, after the folders a recipe names.
const IMAGE_LIBRARIES: [&str; 4] = ["/lib64", "/usr/lib64", "/lib", "/usr/lib"];
/// A program of the build image, read for the image's loader.
const IMAGE_PROGRAM: &str = "/bin/sh";

/// What a 64-bit little-endian ELF file says about loading it.
#[derive(Default)]
struct Elf {
    interpreter: Option<String>,
    needed: Vec<String>,
    /// DT_RPATH: used for the file's own needs and for what those need.
    rpath: Vec<String>,
    /// DT_RUNPATH: used for the file's own needs only, and hides DT_RPATH.
    runpath: Vec<String>,
    dynamic: bool,
}

fn parse(bytes: &[u8]) -> Option<Elf> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let u64_at = |at: usize| Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?));
    let text_at = |at: usize| {
        let rest = bytes.get(at..)?;
        let end = rest.iter().position(|byte| *byte == 0)?;
        String::from_utf8(rest[..end].to_vec()).ok()
    };
    if bytes.get(..6)? != [0x7f, b'E', b'L', b'F', 2, 1] {
        return None;
    }
    let headers = usize::try_from(u64_at(32)?).ok()?;
    let (size, count) = (usize::from(u16_at(54)?), usize::from(u16_at(56)?));
    let mut elf = Elf::default();
    let mut loads = Vec::new();
    let mut dynamic = None;
    for index in 0..count {
        let at = headers.checked_add(index.checked_mul(size)?)?;
        let offset = usize::try_from(u64_at(at + 8)?).ok()?;
        let length = usize::try_from(u64_at(at + 32)?).ok()?;
        match u32_at(at)? {
            1 => loads.push((u64_at(at + 16)?, u64_at(at + 32)?, u64_at(at + 8)?)),
            2 => dynamic = Some((offset, length)),
            3 => elf.interpreter = text_at(offset),
            _ => {}
        }
    }
    let Some((start, length)) = dynamic else {
        return Some(elf);
    };
    elf.dynamic = true;
    let mut entries = Vec::new();
    for at in (start..start.checked_add(length)?).step_by(16) {
        let (tag, value) = (u64_at(at)?, u64_at(at + 8)?);
        if tag == 0 {
            break;
        }
        entries.push((tag, value));
    }
    let table = entries.iter().find(|(tag, _)| *tag == 5).map(|(_, value)| *value)?;
    let (address, _, offset) = loads
        .iter()
        .find(|(address, size, _)| table >= *address && table < address + size)?;
    let strings = usize::try_from(table - address + offset).ok()?;
    for (tag, value) in entries {
        let text = || text_at(strings.checked_add(usize::try_from(value).ok()?)?);
        let folders = |text: String| text.split(':').map(str::to_owned).collect::<Vec<_>>();
        match tag {
            1 => elf.needed.push(text()?),
            15 => elf.rpath = folders(text()?),
            29 => elf.runpath = folders(text()?),
            _ => {}
        }
    }
    Some(elf)
}

fn read(path: &Path) -> Result<Vec<u8>, Box<EvalAltResult>> {
    std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()).into())
}

/// The file name of the build image's loader, as its own programs name it.
fn image_loader() -> Result<PathBuf, Box<EvalAltResult>> {
    parse(&read(Path::new(IMAGE_PROGRAM))?)
        .and_then(|elf| elf.interpreter)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{IMAGE_PROGRAM} names no loader; cannot tell the build image's").into())
}

/// The linker flags that tell a program its place: `prefix` is the folder the
/// package will live in once activated.
pub fn carry_link_flags(prefix: &str) -> Result<String, Box<EvalAltResult>> {
    let loader = image_loader()?;
    let name = loader.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    Ok(format!(
        "-Wl,--disable-new-dtags -Wl,-rpath,{prefix}/lib -Wl,--dynamic-linker={prefix}/lib/{name}"
    ))
}

/// Copies into `package/lib/` the loader and every shared library the
/// programs in `package/bin/` load, looking in `search` and then in the build
/// image, and returns what was carried as `#{ name, from }` maps.
pub fn carry_libraries(
    package: &str,
    prefix: &str,
    search: rhai::Array,
) -> Result<rhai::Array, Box<EvalAltResult>> {
    let package = Path::new(package);
    let library = package.join("lib");
    let place = format!("{prefix}/lib");
    let loader = image_loader()?;
    let loader_name = loader.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    let folders: Vec<PathBuf> = search
        .into_iter()
        .filter_map(|folder| folder.into_string().ok().map(PathBuf::from))
        .chain(IMAGE_LIBRARIES.iter().map(PathBuf::from))
        .collect();

    // Every program must already name its own loader and library folder.
    let mut pending = Vec::new();
    let programs = std::fs::read_dir(package.join("bin"))
        .map_err(|error| format!("cannot read {}/bin: {error}", package.display()))?;
    for program in programs.flatten() {
        let path = program.path();
        let Some(elf) = parse(&read(&path)?).filter(|elf| elf.dynamic) else {
            continue; // a script, or a program with nothing to load
        };
        let told = elf.interpreter.as_deref() == Some(&format!("{place}/{loader_name}"))
            && elf.rpath.iter().any(|folder| *folder == place)
            && elf.runpath.is_empty();
        if !told {
            return Err(format!(
                "{} was not linked for its place (loader {:?}, rpath {:?}, runpath {:?})\nNext: link it with the flags carry_link_flags(prefix) returns.",
                path.display(),
                elf.interpreter,
                elf.rpath,
                elf.runpath
            )
            .into());
        }
        pending.push((path, elf));
    }

    let mut carried = BTreeMap::<String, PathBuf>::new();
    let mut copy = |name: &str, from: &Path| -> Result<Option<Elf>, Box<EvalAltResult>> {
        let bytes = read(from)?;
        let target = library.join(name);
        std::fs::create_dir_all(&library)
            .and_then(|()| std::fs::write(&target, &bytes))
            .and_then(|()| {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            })
            .map_err(|error| format!("cannot write {}: {error}", target.display()))?;
        carried.insert(name.to_owned(), from.to_path_buf());
        Ok(parse(&bytes))
    };
    if !pending.is_empty() {
        copy(loader_name, &loader)?;
    }
    while let Some((file, elf)) = pending.pop() {
        for name in elf.needed {
            let target = library.join(&name);
            if target.is_file() {
                continue;
            }
            let found = folders
                .iter()
                .map(|folder| folder.join(&name))
                .find(|candidate| candidate.is_file())
                .ok_or_else(|| {
                    format!(
                        "{} needs {name}, which is in none of: {}",
                        file.display(),
                        folders
                            .iter()
                            .map(|folder| folder.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?;
            let loaded = copy(&name, &found)?
                .ok_or_else(|| format!("{} is not a shared library", found.display()))?;
            pending.push((target, loaded));
        }
    }
    Ok(carried
        .into_iter()
        .map(|(name, from)| {
            let mut entry = rhai::Map::new();
            entry.insert("name".into(), name.into());
            entry.insert("from".into(), from.display().to_string().into());
            entry.into()
        })
        .collect())
}
