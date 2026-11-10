//! Carrying shared libraries with a package (trial of the self-contained
//! packages design, LevitateOS docs/recipe-self-contained-packages.md).
//!
//! A program in a `package` output loads from two places only: its own
//! loader and `lib/`, in the folder the package lives in once activated.
//! That folder does not exist inside the sandbox, a build runs what it
//! builds, and the sandbox starts a program only with the build image's own
//! loader. So a program is linked as usual, with one addition: a long
//! stand-in for its library folder (`carry_link_flags`). It runs in the
//! sandbox like any other program.
//!
//! `carry_libraries` then copies into `package/lib/` the loader and every
//! shared library the package's programs load, from the folders the recipe
//! names and from the build image, and writes into each file the real
//! library folder and, for a program, the path of its own loader. Both go
//! into the room the stand-in took, so nothing in a file has to move.
//! Nothing then comes from the host's own library folders, so the host's C
//! library does not matter.

use rhai::EvalAltResult;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where the build image keeps its libraries, after the folders a recipe names.
const IMAGE_LIBRARIES: [&str; 4] = ["/lib64", "/usr/lib64", "/lib", "/usr/lib"];
/// The longest folder a package may live in.
const PREFIX_LENGTH: usize = 220;
/// The stand-in's length: room for the library folder and the loader's path
/// of the longest package folder, each with its closing zero.
const STAND_IN_LENGTH: usize = 512;

/// The stand-in for a package's library folder. It is the same text in every
/// build, so a library one recipe built is recognised by the recipe that
/// carries it. No such folder exists; a loader that looks there finds
/// nothing and goes on.
fn stand_in() -> String {
    let mut text = String::from("/recipe-carry-stand-in");
    while text.len() + 4 < STAND_IN_LENGTH {
        text.push_str(if text.len() % 16 == 0 { "/" } else { "p" });
    }
    text + "/lib"
}

/// What a 64-bit little-endian ELF file says about loading it, and where in
/// the file it says so.
#[derive(Default)]
struct Elf {
    interpreter: Option<String>,
    /// The program header that names the loader.
    interpreter_header: Option<usize>,
    needed: Vec<String>,
    /// DT_RPATH: used for the file's own needs and for what those need.
    rpath: Vec<String>,
    /// DT_RUNPATH: used for the file's own needs only, and hides DT_RPATH.
    runpath: Vec<String>,
    /// Per DT_RPATH or DT_RUNPATH entry: the entry, and its text.
    paths_at: Vec<(usize, usize)>,
    /// Loaded parts: address, size in the file, offset in the file.
    loads: Vec<(u64, u64, u64)>,
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
    let mut dynamic = None;
    for index in 0..count {
        let at = headers.checked_add(index.checked_mul(size)?)?;
        let offset = usize::try_from(u64_at(at + 8)?).ok()?;
        let length = usize::try_from(u64_at(at + 32)?).ok()?;
        match u32_at(at)? {
            1 => elf.loads.push((u64_at(at + 16)?, u64_at(at + 32)?, u64_at(at + 8)?)),
            2 => dynamic = Some((offset, length)),
            3 => {
                elf.interpreter = text_at(offset);
                elf.interpreter_header = Some(at);
            }
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
        entries.push((tag, value, at));
    }
    let table = entries.iter().find(|(tag, ..)| *tag == 5).map(|(_, value, _)| *value)?;
    let (address, _, offset) = elf
        .loads
        .iter()
        .find(|(address, size, _)| table >= *address && table < address + size)?;
    let strings = usize::try_from(table - address + offset).ok()?;
    for (tag, value, at) in entries {
        let text_offset = strings.checked_add(usize::try_from(value).ok()?)?;
        let text = || text_at(text_offset);
        let folders = |text: String| text.split(':').map(str::to_owned).collect::<Vec<_>>();
        match tag {
            1 => elf.needed.push(text()?),
            15 => {
                elf.rpath = folders(text()?);
                elf.paths_at.push((at, text_offset));
            }
            29 => {
                elf.runpath = folders(text()?);
                elf.paths_at.push((at, text_offset));
            }
            _ => {}
        }
    }
    Some(elf)
}

/// Writes a package's real folder into one file's bytes, in the room its
/// stand-in took. Returns whether the file had the stand-in.
///
/// Nothing moves. The library folder is written so that it ends where the
/// stand-in ended, and its entry is pointed at the new start and made
/// DT_RPATH, which also holds for what the file's libraries load in turn. A
/// linker may let another name share the end of a text; the end stays as it
/// was, since both end in `/lib`. For a program, the path of its own loader
/// is written at the start of the same room, and the header that names the
/// loader is pointed there: the room is in a part of the file that is loaded,
/// which the loader needs to read its own name.
fn settle(bytes: &mut [u8], elf: &Elf, prefix: &str) -> Result<bool, String> {
    let stand_in = stand_in();
    let real = format!("{prefix}/lib");
    let put = |bytes: &mut [u8], at: usize, value: u64| {
        bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
    };
    for (entry, text) in &elf.paths_at {
        let end = text + stand_in.len();
        if bytes.get(*text..end) != Some(stand_in.as_bytes()) || bytes.get(end) != Some(&0) {
            continue; // not the stand-in alone; the caller judges what it is
        }
        let start = end - real.len();
        if let (Some(header), Some(interpreter)) = (elf.interpreter_header, &elf.interpreter) {
            let name = interpreter.rsplit('/').next().unwrap_or_default();
            let loader = format!("{real}/{name}");
            if text + loader.len() + 1 > start {
                return Err(format!("{loader} and {real} do not fit in the stand-in's room"));
            }
            let (address, _, offset) = elf
                .loads
                .iter()
                .find(|(_, size, offset)| (*text as u64) >= *offset && (*text as u64) < offset + size)
                .ok_or("the stand-in is not in a loaded part of the file")?;
            let at = address + (*text as u64 - offset);
            bytes[*text..text + loader.len()].copy_from_slice(loader.as_bytes());
            bytes[text + loader.len()] = 0;
            put(bytes, header + 8, *text as u64);
            put(bytes, header + 16, at);
            put(bytes, header + 24, at);
            put(bytes, header + 32, loader.len() as u64 + 1);
            put(bytes, header + 40, loader.len() as u64 + 1);
        }
        bytes[start..end].copy_from_slice(real.as_bytes());
        let value = u64::from_le_bytes(bytes[entry + 8..entry + 16].try_into().unwrap());
        put(bytes, entry + 8, value + (start - text) as u64);
        put(bytes, *entry, 15);
        return Ok(true);
    }
    Ok(false)
}

fn read(path: &Path) -> Result<Vec<u8>, Box<EvalAltResult>> {
    std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()).into())
}

/// The linker flags every program and library of a package is linked with:
/// they reserve the room `carry_libraries` writes the package's place into.
pub fn carry_link_flags() -> String {
    format!("-Wl,--disable-new-dtags -Wl,-rpath,{}", stand_in())
}

/// Copies into `package/lib/` the loader and every shared library the
/// programs in `package/bin/` load, looking in `search` and then in the build
/// image, writes `prefix` (the folder the package will live in) into every
/// file, and returns what was carried as `#{ name, from }` maps.
pub fn carry_libraries(
    package: &str,
    prefix: &str,
    search: rhai::Array,
) -> Result<rhai::Array, Box<EvalAltResult>> {
    if prefix.len() > PREFIX_LENGTH || !prefix.starts_with('/') || prefix.ends_with('/') {
        return Err(format!(
            "the package's folder must be an absolute path of at most {PREFIX_LENGTH} characters, got {prefix:?}"
        )
        .into());
    }
    let package = Path::new(package);
    let library = package.join("lib");
    let place = format!("{prefix}/lib");
    let folders: Vec<PathBuf> = search
        .into_iter()
        .filter_map(|folder| folder.into_string().ok().map(PathBuf::from))
        .chain(IMAGE_LIBRARIES.iter().map(PathBuf::from))
        .collect();

    // Copies one file into lib/ under the name it is asked for by, settled.
    // A library that would look in a folder of its own choosing is refused.
    let mut carried = BTreeMap::<String, PathBuf>::new();
    let mut copy = |name: &str, from: &Path| -> Result<Option<Elf>, Box<EvalAltResult>> {
        let mut bytes = read(from)?;
        if let Some(built) = parse(&bytes) {
            settle(&mut bytes, &built, prefix).map_err(|error| format!("{}: {error}", from.display()))?;
        }
        let after = parse(&bytes);
        if let Some(after) = &after {
            let own = |folder: &String| *folder == place || folder == "$ORIGIN";
            if !after.rpath.iter().chain(&after.runpath).all(own) {
                return Err(format!(
                    "{} would look for its libraries in {:?} {:?}, outside the package",
                    from.display(),
                    after.rpath,
                    after.runpath
                )
                .into());
            }
        }
        let target = library.join(name);
        std::fs::create_dir_all(&library)
            .and_then(|()| std::fs::write(&target, &bytes))
            .and_then(|()| {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            })
            .map_err(|error| format!("cannot write {}: {error}", target.display()))?;
        carried.insert(name.to_owned(), from.to_path_buf());
        Ok(after)
    };

    // Everything the recipe put in the package is looked at: programs in
    // bin/, and under lib/ the plug-ins a program opens by name and any
    // program kept beside them. A program is given its place and its loader
    // is carried; a plug-in is settled; the needs of both are followed.
    let mut pending = Vec::new();
    let mut folders_left = vec![package.join("bin"), library.clone()];
    while let Some(folder) = folders_left.pop() {
        for item in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let path = item.path();
            if path.is_dir() {
                folders_left.push(path);
                continue;
            }
            let mut bytes = read(&path)?;
            let Some(placed) = parse(&bytes).filter(|elf| elf.dynamic) else {
                continue; // a script, data, or a program with nothing to load
            };
            let settled = settle(&mut bytes, &placed, prefix)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let elf = parse(&bytes).ok_or_else(|| format!("{} cannot be read back", path.display()))?;
            if let Some(loader) = &placed.interpreter {
                let name = loader.rsplit('/').next().unwrap_or_default().to_owned();
                let told = settled
                    && elf.interpreter.as_deref() == Some(&format!("{place}/{name}"))
                    && elf.rpath == [place.clone()]
                    && elf.runpath.is_empty();
                if !told {
                    return Err(format!(
                        "{} was not linked for carrying (loader {:?}, rpath {:?}, runpath {:?})\nNext: link it with the flags carry_link_flags() returns, and with no other -rpath.",
                        path.display(),
                        placed.interpreter,
                        placed.rpath.iter().map(|folder| folder.chars().take(40).collect::<String>()).collect::<Vec<_>>(),
                        placed.runpath
                    )
                    .into());
                }
                if !library.join(&name).is_file() {
                    copy(&name, Path::new(loader))?;
                }
            } else {
                let own = |folder: &String| *folder == place || folder == "$ORIGIN";
                if !elf.rpath.iter().chain(&elf.runpath).all(own) {
                    return Err(format!(
                        "{} would look for its libraries in {:?} {:?}, outside the package",
                        path.display(),
                        elf.rpath,
                        elf.runpath
                    )
                    .into());
                }
            }
            if settled {
                std::fs::write(&path, &bytes)
                    .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
            }
            pending.push((path, elf));
        }
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
