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
//!
//! A file somebody else linked (an upstream release) has no such room. It is
//! rewritten instead (`rewrite`): a new loadable part at its end holds the
//! loader's path and `$ORIGIN`-relative library folders, so only the loader
//! names the package's folder. Afterwards the closure check proves that
//! every file finds all it loads inside the package.

mod rewrite;

use rhai::EvalAltResult;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where the build image keeps its libraries, after the folders a recipe names.
const IMAGE_LIBRARIES: [&str; 4] = ["/lib64", "/usr/lib64", "/lib", "/usr/lib"];
/// The longest package folder whose paths fit in the stand-in's room; with a
/// longer one, files linked with the stand-in are rewritten like any other.
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
    /// Per library: the symbol versions the file needs from it (weak ones
    /// left out), from DT_VERNEED.
    versions_needed: Vec<(String, Vec<String>)>,
    /// The symbol versions the file defines, from DT_VERDEF.
    versions_defined: Vec<String>,
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
            1 => elf
                .loads
                .push((u64_at(at + 16)?, u64_at(at + 32)?, u64_at(at + 8)?)),
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
    let table = entries
        .iter()
        .find(|(tag, ..)| *tag == 5)
        .map(|(_, value, _)| *value)?;
    let (address, _, offset) = elf
        .loads
        .iter()
        .find(|(address, size, _)| table >= *address && table < address + size)?;
    let strings = usize::try_from(table - address + offset).ok()?;
    let value_of = |wanted: u64| {
        entries
            .iter()
            .find(|(tag, ..)| *tag == wanted)
            .map(|(_, value, _)| *value)
    };
    let file_at = |address: u64| {
        let (start, _, offset) = elf
            .loads
            .iter()
            .find(|(start, size, _)| address >= *start && address < start + size)?;
        usize::try_from(address - start + offset).ok()
    };
    let name_at = |offset: u32| text_at(strings.checked_add(offset as usize)?);
    // What a version table says is read as far as it is whole; a damaged
    // one only leaves the versions unchecked.
    let needed_versions = |table: u64, count: u64| -> Option<Vec<(String, Vec<String>)>> {
        let mut found = Vec::new();
        let mut at = file_at(table)?;
        for _ in 0..count.min(4096) {
            let file = name_at(u32_at(at + 4)?)?;
            let mut aux = at.checked_add(u32_at(at + 8)? as usize)?;
            let mut versions = Vec::new();
            for _ in 0..u16_at(at + 2)? {
                if u16_at(aux + 4)? & 2 == 0 {
                    versions.push(name_at(u32_at(aux + 8)?)?);
                }
                match u32_at(aux + 12)? {
                    0 => break,
                    next => aux = aux.checked_add(next as usize)?,
                }
            }
            found.push((file, versions));
            match u32_at(at + 12)? {
                0 => break,
                next => at = at.checked_add(next as usize)?,
            }
        }
        Some(found)
    };
    let defined_versions = |table: u64, count: u64| -> Option<Vec<String>> {
        let mut found = Vec::new();
        let mut at = file_at(table)?;
        for _ in 0..count.min(65536) {
            let aux = at.checked_add(u32_at(at + 12)? as usize)?;
            found.push(name_at(u32_at(aux)?)?);
            match u32_at(at + 16)? {
                0 => break,
                next => at = at.checked_add(next as usize)?,
            }
        }
        Some(found)
    };
    let versions_needed = match (value_of(0x6fff_fffe), value_of(0x6fff_ffff)) {
        (Some(table), Some(count)) => needed_versions(table, count).unwrap_or_default(),
        _ => Vec::new(),
    };
    let versions_defined = match (value_of(0x6fff_fffc), value_of(0x6fff_fffd)) {
        (Some(table), Some(count)) => defined_versions(table, count).unwrap_or_default(),
        _ => Vec::new(),
    };
    elf.versions_needed = versions_needed;
    elf.versions_defined = versions_defined;
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
                return Err(format!(
                    "{loader} and {real} do not fit in the stand-in's room"
                ));
            }
            let (address, _, offset) = elf
                .loads
                .iter()
                .find(|(_, size, offset)| {
                    (*text as u64) >= *offset && (*text as u64) < offset + size
                })
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

/// The loader the C library comes with, on x86_64.
const GLIBC_LOADER: &str = "ld-linux-x86-64.so.2";

/// Whether `name` is one of the C library's own files, which must come from
/// the same build as its loader.
fn glibc_member(name: &str) -> bool {
    let stem = name.split(".so").next().unwrap_or_default();
    name == GLIBC_LOADER
        || stem.starts_with("libnss_")
        || matches!(
            stem,
            "libc"
                | "libm"
                | "libmvec"
                | "libpthread"
                | "libdl"
                | "librt"
                | "libutil"
                | "libresolv"
                | "libanl"
                | "libnsl"
                | "libBrokenLocale"
                | "libthread_db"
                | "libc_malloc_debug"
        )
}

/// What the start of a file says it is.
enum Kind {
    /// Not an ELF file: a script, data.
    Other,
    /// An ELF file this package's loader cannot load, and why.
    Foreign(String),
    /// A 64-bit little-endian x86_64 program or library.
    Native,
}

/// Whether `path` is a library this package's loader can load. Opening a
/// file is far dearer than looking it up (the sandbox mediates both), so a
/// name that is not there is not opened.
fn loadable(path: &Path) -> bool {
    path.is_file() && matches!(kind(path), Kind::Native)
}

fn kind(path: &Path) -> Kind {
    use std::io::Read;
    let mut head = [0u8; 64];
    let read = std::fs::File::open(path).and_then(|mut file| {
        let mut filled = 0;
        while filled < head.len() {
            match file.read(&mut head[filled..])? {
                0 => break,
                count => filled += count,
            }
        }
        Ok(filled)
    });
    let Ok(length) = read else {
        return Kind::Other;
    };
    let head = &head[..length];
    if head.get(..4) != Some(b"\x7fELF") {
        return Kind::Other;
    }
    match rewrite::unsupported(head) {
        None => Kind::Native,
        Some(why) => Kind::Foreign(why),
    }
}

/// `$ORIGIN` followed by the way from `from` (a folder in the package) to
/// the package's `lib/`.
fn origin_path(package: &Path, from: &Path) -> String {
    let inside: Vec<_> = from
        .strip_prefix(package)
        .map(|rest| rest.components().collect())
        .unwrap_or_default();
    let mut path = String::from("$ORIGIN");
    match inside.first() {
        Some(first) if first.as_os_str() == "lib" => {
            for _ in 1..inside.len() {
                path.push_str("/..");
            }
        }
        _ => {
            for _ in 0..inside.len() {
                path.push_str("/..");
            }
            path.push_str("/lib");
        }
    }
    path
}

/// The folder a library folder entry of a file in `from` means inside the
/// package, or None if it means a folder outside it.
fn inside(package: &Path, prefix: &str, from: &Path, entry: &str) -> Option<PathBuf> {
    let (base, rest) = match entry
        .strip_prefix("$ORIGIN")
        .or_else(|| entry.strip_prefix("${ORIGIN}"))
    {
        Some(rest) => (from.to_path_buf(), rest),
        None => (package.to_path_buf(), entry.strip_prefix(prefix)?),
    };
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    let mut folder = base;
    for part in rest
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
    {
        if part == ".." {
            folder.pop();
        } else {
            folder.push(part);
        }
    }
    folder.starts_with(package).then_some(folder)
}

/// The folders a file looks in for what it loads, as the loader would:
/// its DT_RUNPATH, or else its DT_RPATH and then the program's, which
/// reaches the package's `lib/`. An entry outside the package is an error.
fn search_folders(
    package: &Path,
    prefix: &str,
    from: &Path,
    elf: &Elf,
) -> Result<Vec<PathBuf>, String> {
    let own = if elf.runpath.is_empty() {
        &elf.rpath
    } else {
        &elf.runpath
    };
    let mut folders = Vec::new();
    for entry in own.iter().filter(|entry| !entry.is_empty()) {
        folders.push(
            inside(package, prefix, from, entry).ok_or_else(|| {
                format!("would look for libraries in {entry}, outside the package")
            })?,
        );
    }
    if elf.runpath.is_empty() {
        folders.push(package.join("lib"));
    }
    Ok(folders)
}

/// One carry step: the package, where it will live, and where libraries are
/// looked for.
struct Carry<'a> {
    package: &'a Path,
    library: PathBuf,
    prefix: &'a str,
    folders: Vec<PathBuf>,
    /// Where the C library comes from: the folder its loader was taken from.
    glibc: Option<PathBuf>,
    carried: BTreeMap<String, PathBuf>,
}

impl Carry<'_> {
    /// Tells one file, about to sit in folder `at` of the package, where its
    /// loader and libraries are. Returns the new bytes if they changed, and
    /// what the file says afterwards.
    ///
    /// A file linked with `carry_link_flags()` has room reserved and is
    /// settled in place. Any other file is rewritten: a program's loader
    /// becomes `PREFIX/lib/LOADER`; its library folders become the `$ORIGIN`
    /// folders it already had that stay inside the package, then (for a
    /// library under `lib/`) its own folder, then the way from its own folder
    /// to `lib/`. A file that loads nothing and names no loader (the loader
    /// itself) is left alone.
    fn place(&self, bytes: &[u8], at: &Path) -> Result<(Option<Vec<u8>>, Elf), String> {
        let elf = parse(bytes).ok_or("cannot be read as a 64-bit ELF file")?;
        if self.prefix.len() <= PREFIX_LENGTH {
            let mut settled = bytes.to_vec();
            if settle(&mut settled, &elf, self.prefix)? {
                let after = parse(&settled).ok_or("cannot be read back after settling")?;
                return Ok((Some(settled), after));
            }
        }
        let loader = elf.interpreter.as_ref().map(|path| {
            let name = path.rsplit('/').next().unwrap_or_default();
            format!("{}/lib/{name}", self.prefix)
        });
        let own = if elf.runpath.is_empty() {
            &elf.rpath
        } else {
            &elf.runpath
        };
        let mut wanted: Vec<String> = Vec::new();
        for entry in own {
            let origin = entry.starts_with("$ORIGIN") || entry.starts_with("${ORIGIN}");
            if origin && inside(self.package, self.prefix, at, entry).is_some() {
                let entry = entry.replacen("${ORIGIN}", "$ORIGIN", 1);
                if !wanted.contains(&entry) {
                    wanted.push(entry);
                }
            }
        }
        // A library kept in a folder of its own under lib/ (an upstream
        // bundle's) finds its companions beside it, as the bundle's program
        // told it to; then lib/.
        let reach = origin_path(self.package, at);
        let beside = (elf.interpreter.is_none() && at.starts_with(&self.library))
            .then(|| "$ORIGIN".to_owned());
        for entry in beside.into_iter().chain([reach]) {
            if !wanted.contains(&entry) {
                wanted.push(entry);
            }
        }
        let needs_paths = !elf.needed.is_empty() || !own.is_empty();
        let paths_right = !needs_paths || *own == wanted;
        if elf.interpreter == loader && paths_right {
            return Ok((None, elf));
        }
        let folders = if needs_paths {
            wanted.join(":")
        } else {
            origin_path(self.package, at)
        };
        let rewritten = rewrite::rewrite(bytes, loader.as_deref(), &folders)?;
        let after = parse(&rewritten).ok_or("cannot be read back after rewriting")?;
        Ok((Some(rewritten), after))
    }

    /// The folder the C library and its loader come from: the first folder
    /// the recipe names that holds the loader, else the build image's (the
    /// folder the program's own loader path names, then the image's
    /// library folders).
    fn glibc_home(&mut self, image_loader: Option<&Path>) -> Result<PathBuf, String> {
        if let Some(home) = &self.glibc {
            return Ok(home.clone());
        }
        let home = self
            .folders
            .iter()
            .cloned()
            .chain(image_loader.and_then(Path::parent).map(Path::to_path_buf))
            .chain(IMAGE_LIBRARIES.iter().map(PathBuf::from))
            .find(|folder| loadable(&folder.join(GLIBC_LOADER)))
            .ok_or_else(|| format!("no folder holds the C library's loader {GLIBC_LOADER}"))?;
        self.glibc = Some(home.clone());
        Ok(home)
    }

    /// Where library `name` is taken from. The C library's files come only
    /// from the folder its loader came from; anything else from the first
    /// folder that holds a 64-bit x86_64 file of that name.
    fn find(&mut self, name: &str, image_loader: Option<&Path>) -> Result<PathBuf, String> {
        if glibc_member(name) {
            let home = self.glibc_home(image_loader)?;
            let candidate = home.join(name);
            return match loadable(&candidate) {
                true => Ok(candidate),
                false => Err(format!(
                    "{name} belongs to the C library, which comes whole from {} with its loader, and is not there",
                    home.display()
                )),
            };
        }
        let mut folders = self.folders.clone();
        for folder in IMAGE_LIBRARIES.iter().map(PathBuf::from) {
            if !folders.contains(&folder) {
                folders.push(folder);
            }
        }
        folders
            .iter()
            .map(|folder| folder.join(name))
            .find(|candidate| loadable(candidate))
            .ok_or_else(|| {
                format!(
                    "{name}, which is in none of: {}",
                    folders
                        .iter()
                        .map(|folder| folder.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    /// Copies library `name` from `from` into `lib/`, placed.
    fn copy(&mut self, name: &str, from: &Path) -> Result<Elf, String> {
        let bytes =
            std::fs::read(from).map_err(|e| format!("cannot read {}: {e}", from.display()))?;
        let (placed, elf) = self
            .place(&bytes, &self.library)
            .map_err(|error| format!("{}: {error}", from.display()))?;
        let target = self.library.join(name);
        std::fs::create_dir_all(&self.library)
            .and_then(|()| std::fs::write(&target, placed.as_deref().unwrap_or(&bytes)))
            .and_then(|()| {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))
            })
            .map_err(|error| format!("cannot write {}: {error}", target.display()))?;
        self.carried.insert(name.to_owned(), from.to_path_buf());
        Ok(elf)
    }
}

/// Every file under `folder` that is no symlink, folders first-found order.
fn files_under(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut left = vec![folder.to_path_buf()];
    while let Some(folder) = left.pop() {
        for item in std::fs::read_dir(&folder).into_iter().flatten().flatten() {
            let path = item.path();
            match item.file_type() {
                Ok(kind) if kind.is_dir() => left.push(path),
                Ok(kind) if kind.is_file() => files.push(path),
                _ => {}
            }
        }
    }
    files.sort();
    files
}

/// Copies into `package/lib/` the loader and every shared library the
/// programs in `package/bin/`, and the programs and plug-ins the recipe put
/// under `package/lib/`, load, looking in `search` and then in the build
/// image; tells every such file the paths it will have under `prefix` (the
/// folder the package will live in, `RUNTIME.paths.prefix`); checks that
/// everything each file loads is then inside the package; and returns what
/// was carried as `#{ name, from }` maps.
///
/// The C library comes whole from one place: the first folder in `search`
/// holding its loader, else the build image. So an `sdk` that brings its own
/// glibc gives the loader and C library its libraries were built against.
pub fn carry_libraries(
    package: &str,
    prefix: &str,
    search: rhai::Array,
) -> Result<rhai::Array, Box<EvalAltResult>> {
    if !prefix.starts_with('/')
        || prefix.len() < 2
        || prefix.ends_with('/')
        || prefix.contains(':')
        || prefix.chars().any(char::is_control)
    {
        return Err(format!(
            "the package's folder must be an absolute path without ':' or a final '/', got {prefix:?}\nNext: pass RUNTIME.paths.prefix, which is set when the package will be activated (not with --no-activate)"
        )
        .into());
    }
    let package = Path::new(package);
    let mut carry = Carry {
        package,
        library: package.join("lib"),
        prefix,
        folders: search
            .into_iter()
            .filter_map(|folder| folder.into_string().ok().map(PathBuf::from))
            .collect(),
        glibc: None,
        carried: BTreeMap::new(),
    };

    // What the recipe put in the package: programs in bin/, and under lib/
    // the plug-ins a program opens by name and any program kept beside them.
    // Each is told its place; the loader of each program is carried.
    let mut pending = Vec::new();
    let mut loaders = BTreeMap::<String, PathBuf>::new();
    let mut own = files_under(&package.join("bin"));
    own.extend(files_under(&carry.library));
    for path in own {
        match kind(&path) {
            Kind::Other => continue,
            Kind::Foreign(why) if !path.starts_with(&carry.library) => {
                return Err(format!(
                    "{} {why}; a package's programs must be 64-bit x86_64",
                    path.display()
                )
                .into());
            }
            Kind::Foreign(_) => continue, // not loadable here; left as it is
            Kind::Native => {}
        }
        let bytes = read(&path)?;
        if !parse(&bytes).is_some_and(|elf| elf.dynamic) {
            continue; // a static program loads nothing
        }
        let folder = path.parent().unwrap_or(package);
        let (placed, elf) = carry
            .place(&bytes, folder)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if let Some(placed) = placed {
            std::fs::write(&path, placed)
                .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        }
        if let Some(interpreter) = parse(&bytes).and_then(|before| before.interpreter) {
            let name = interpreter
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_owned();
            loaders
                .entry(name)
                .or_insert_with(|| PathBuf::from(interpreter));
        }
        pending.push((path, elf));
    }

    // The loaders first, so that the C library follows the loader's folder.
    for (name, image_loader) in &loaders {
        if carry.library.join(name).is_file() {
            continue;
        }
        let from = if name == GLIBC_LOADER {
            carry.find(name, Some(image_loader))?
        } else {
            // Another loader: from the folders, else from its own path.
            carry
                .find(name, Some(image_loader))
                .unwrap_or_else(|_| image_loader.clone())
        };
        carry.copy(name, &from)?;
    }
    let image_loader = loaders.get(GLIBC_LOADER).cloned();

    // What each file loads, and what that loads in turn.
    // A library the file finds in its own folders (one the upstream release
    // keeps beside its program, say) is already in the package.
    let mut not_found = Vec::new();
    while let Some((file, elf)) = pending.pop() {
        let folder = file.parent().unwrap_or(package);
        let own = search_folders(package, prefix, folder, &elf).unwrap_or_default();
        for name in elf.needed {
            let target = carry.library.join(&name);
            if target.is_file() || own.iter().any(|folder| loadable(&folder.join(&name))) {
                continue;
            }
            match carry.find(&name, image_loader.as_deref()) {
                Ok(found) => {
                    let loaded = carry.copy(&name, &found)?;
                    pending.push((target, loaded));
                }
                Err(error) => not_found.push(format!("{} needs {error}", file.display())),
            }
        }
    }
    if !not_found.is_empty() {
        not_found.sort();
        not_found.dedup();
        return Err(format!(
            "libraries the package loads were not found:\n  {}",
            not_found.join("\n  ")
        )
        .into());
    }

    check_closure(package, prefix).map_err(|error| -> Box<EvalAltResult> { error.into() })?;

    Ok(carry
        .carried
        .into_iter()
        .map(|(name, from)| {
            let mut entry = rhai::Map::new();
            entry.insert("name".into(), name.into());
            entry.insert("from".into(), from.display().to_string().into());
            entry.into()
        })
        .collect())
}

/// `carry_libraries` given no package folder: RUNTIME.paths.prefix is unit.
pub fn carry_without_prefix() -> Result<rhai::Array, Box<EvalAltResult>> {
    Err("carry_libraries needs the package's folder, and RUNTIME.paths.prefix is unset: the package will not be activated (--no-activate, a dependency, or a recipe whose name is not a literal)".into())
}

/// The closure check: every program and every file under `lib/` finds each
/// library it loads inside the package (in `lib/`, or beside itself as its
/// library folders say), and every program's loader is the package's own.
/// A program cannot be run in the sandbox to prove it, so this is the proof
/// the install gets; it fails naming each file and what it misses.
fn check_closure(package: &Path, prefix: &str) -> Result<(), String> {
    let library = package.join("lib");
    let mut files = files_under(&package.join("bin"));
    files.extend(files_under(&library));
    let mut missing = Vec::new();
    let mut seen = std::collections::HashMap::new();
    let mut defines = std::collections::HashMap::<PathBuf, Vec<String>>::new();
    for path in files {
        if !matches!(kind(&path), Kind::Native) {
            continue;
        }
        let bytes =
            std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let Some(elf) = parse(&bytes).filter(|elf| elf.dynamic) else {
            continue;
        };
        if let Some(interpreter) = &elf.interpreter {
            let name = interpreter.rsplit('/').next().unwrap_or_default();
            if *interpreter != format!("{prefix}/lib/{name}") || !library.join(name).is_file() {
                missing.push(format!(
                    "{} starts with the loader {interpreter}, not the package's own",
                    path.display()
                ));
            }
        }
        let folder = path.parent().unwrap_or(package);
        let folders = match search_folders(package, prefix, folder, &elf) {
            Ok(folders) => folders,
            Err(error) => {
                missing.push(format!("{} {error}", path.display()));
                continue;
            }
        };
        for name in &elf.needed {
            let found = folders
                .iter()
                .map(|folder| folder.join(name))
                .filter(|_| !name.contains('/'))
                .find(|candidate| {
                    *seen
                        .entry(candidate.clone())
                        .or_insert_with(|| loadable(candidate))
                });
            if let Some(found) = found {
                // The loader refuses a library that lacks a symbol version
                // the file asks of it (a library built against a newer C
                // library than the one carried, say).
                let defined = defines.entry(found.clone()).or_insert_with(|| {
                    std::fs::read(&found)
                        .ok()
                        .and_then(|bytes| parse(&bytes))
                        .map(|elf| elf.versions_defined)
                        .unwrap_or_default()
                });
                let lacking: Vec<&str> = elf
                    .versions_needed
                    .iter()
                    .filter(|(file, _)| file == name)
                    .flat_map(|(_, versions)| versions)
                    .filter(|version| !defined.contains(version))
                    .map(String::as_str)
                    .collect();
                if !lacking.is_empty() {
                    missing.push(format!(
                        "{} needs {} of {name}, which {} does not define",
                        path.display(),
                        lacking.join(", "),
                        found.display()
                    ));
                }
            } else {
                missing.push(format!(
                    "{} needs {name}, which is not in {}",
                    path.display(),
                    folders
                        .iter()
                        .map(|folder| folder.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" or ")
                ));
            }
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let shown = missing.len().min(40);
    Err(format!(
        "the package does not hold everything it loads:\n  {}{}",
        missing[..shown].join("\n  "),
        if missing.len() > shown {
            format!("\n  ... and {} more", missing.len() - shown)
        } else {
            String::new()
        }
    ))
}
