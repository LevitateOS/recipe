//! Telling a file somebody else linked where its loader and libraries are,
//! when it has no room reserved for that (an upstream release: Electron,
//! Chromium, Zed). This is what `patchelf --set-interpreter --set-rpath`
//! does, for 64-bit little-endian x86_64 files only.
//!
//! Nothing already in the file moves. One loadable, page-aligned part is
//! added at the end of the file, holding:
//!
//! 1. the program header table, one entry longer (the new part itself; and a
//!    PT_PHDR for a program that lacked one), since the old table has no room
//!    to grow where it is. PT_PHDR is pointed at the copy;
//! 2. the new loader path, which PT_INTERP is pointed at (programs only);
//! 3. a copy of the dynamic string table with the new library folders
//!    appended, which DT_STRTAB and DT_STRSZ are pointed at. Every old string
//!    keeps its offset, so nothing else that names a string changes;
//! 4. the dynamic table, only when it has no free entry for the new
//!    DT_RPATH: then it is copied here one entry longer and PT_DYNAMIC is
//!    pointed at the copy, and the part is made writable, because the loader
//!    writes into DT_DEBUG.
//!
//! In the dynamic table every DT_RPATH and DT_RUNPATH entry is replaced by
//! one DT_RPATH naming the new folders. DT_RPATH rather than DT_RUNPATH: it
//! is looked at before LD_LIBRARY_PATH, so nothing from the host's folders
//! comes first, and it also serves what the file's libraries load in turn.
//! Section headers, where the file has them, are pointed at the moved
//! `.interp`, `.dynstr` and `.dynamic`; a stripped file without them is
//! rewritten from its program headers and dynamic table alone.
//!
//! The kernel finds a program header table outside the first loaded part
//! through PT_PHDR (Linux 5.18 and later compute AT_PHDR from it), which both
//! distributions' kernels do.

const PAGE: u64 = 0x1000;
const PHENT: usize = 56;
const DYNENT: usize = 16;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_PHDR: u32 = 6;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

const DT_NULL: u64 = 0;
const DT_STRTAB: u64 = 5;
const DT_STRSZ: u64 = 10;
const DT_RPATH: u64 = 15;
const DT_RUNPATH: u64 = 29;

const SHT_PROGBITS: u32 = 1;
const SHT_STRTAB: u32 = 3;
const SHT_DYNAMIC: u32 = 6;

/// One program header.
#[derive(Clone, Copy)]
struct Segment {
    kind: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    paddr: u64,
    filesz: u64,
    memsz: u64,
    align: u64,
}

impl Segment {
    fn read(bytes: &[u8], at: usize) -> Option<Self> {
        Some(Self {
            kind: u32_at(bytes, at)?,
            flags: u32_at(bytes, at + 4)?,
            offset: u64_at(bytes, at + 8)?,
            vaddr: u64_at(bytes, at + 16)?,
            paddr: u64_at(bytes, at + 24)?,
            filesz: u64_at(bytes, at + 32)?,
            memsz: u64_at(bytes, at + 40)?,
            align: u64_at(bytes, at + 48)?,
        })
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.kind.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        for value in [
            self.offset,
            self.vaddr,
            self.paddr,
            self.filesz,
            self.memsz,
            self.align,
        ] {
            out.extend_from_slice(&value.to_le_bytes());
        }
    }

    /// Points this header at `size` bytes at `offset` / `vaddr`.
    fn place(&mut self, offset: u64, vaddr: u64, size: u64) {
        self.offset = offset;
        self.vaddr = vaddr;
        self.paddr = vaddr;
        self.filesz = size;
        self.memsz = size;
    }
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

fn put_u16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], at: usize, value: u64) {
    bytes[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

fn align_up(value: u64, to: u64) -> u64 {
    value.div_ceil(to) * to
}

/// Why a file is not one this module can rewrite, or None if it is one:
/// a 64-bit little-endian x86_64 program or shared library.
pub(super) fn unsupported(bytes: &[u8]) -> Option<String> {
    if bytes.get(..4) != Some(b"\x7fELF") {
        return Some("is not an ELF file".into());
    }
    match (bytes.get(4), bytes.get(5)) {
        (Some(2), Some(1)) => {}
        (Some(1), _) => return Some("is a 32-bit ELF file".into()),
        (_, Some(2)) => return Some("is a big-endian ELF file".into()),
        _ => return Some("has an unknown ELF class".into()),
    }
    match u16_at(bytes, 18) {
        Some(62) => {}
        Some(machine) => return Some(format!("is for ELF machine {machine}, not x86_64")),
        None => return Some("has a cut ELF header".into()),
    }
    match u16_at(bytes, 16) {
        Some(2 | 3) => {}
        Some(kind) => return Some(format!("is ELF type {kind}, not a program or library")),
        None => return Some("has a cut ELF header".into()),
    }
    if u16_at(bytes, 54) != Some(PHENT as u16) {
        return Some("has program headers of an unexpected size".into());
    }
    None
}

/// Returns a copy of `bytes` whose loader is `interpreter` (a program; None
/// for a library, which has no loader of its own) and whose only library
/// folders are `folders` (a `:`-separated DT_RPATH).
pub(super) fn rewrite(
    bytes: &[u8],
    interpreter: Option<&str>,
    folders: &str,
) -> Result<Vec<u8>, String> {
    if let Some(why) = unsupported(bytes) {
        return Err(format!(
            "{why}; only 64-bit little-endian x86_64 programs and libraries can be rewritten"
        ));
    }
    if interpreter.is_some_and(|path| path.contains('\0')) || folders.contains('\0') {
        return Err("a path holds a zero byte".into());
    }
    let cut = || "the file is cut short".to_owned();
    let size = |value: u64| usize::try_from(value).map_err(|_| cut());
    let phoff = size(u64_at(bytes, 32).ok_or_else(cut)?)?;
    let phnum = usize::from(u16_at(bytes, 56).ok_or_else(cut)?);
    let mut segments = (0..phnum)
        .map(|index| Segment::read(bytes, phoff + index * PHENT).ok_or_else(cut))
        .collect::<Result<Vec<_>, _>>()?;
    let loads: Vec<Segment> = segments
        .iter()
        .filter(|segment| segment.kind == PT_LOAD)
        .copied()
        .collect();
    if loads.is_empty() {
        return Err("has no loadable part".into());
    }
    let file_offset = |vaddr: u64| {
        loads
            .iter()
            .find(|load| vaddr >= load.vaddr && vaddr < load.vaddr + load.filesz)
            .map(|load| vaddr - load.vaddr + load.offset)
    };
    let interp_index = segments
        .iter()
        .position(|segment| segment.kind == PT_INTERP);
    if interpreter.is_some() && interp_index.is_none() {
        return Err("names no loader, so it has none to replace".into());
    }

    // The dynamic table, up to its end marker, and how many entries fit.
    let dynamic_index = segments
        .iter()
        .position(|segment| segment.kind == PT_DYNAMIC)
        .ok_or("has no dynamic table (a static program needs no rewriting)")?;
    let dynamic = segments[dynamic_index];
    let capacity = size(dynamic.filesz)? / DYNENT;
    let mut entries = Vec::new();
    for index in 0..capacity {
        let at = size(dynamic.offset)? + index * DYNENT;
        let tag = u64_at(bytes, at).ok_or_else(cut)?;
        if tag == DT_NULL {
            break;
        }
        entries.push((tag, u64_at(bytes, at + 8).ok_or_else(cut)?));
    }
    let value_of = |wanted: u64| {
        entries
            .iter()
            .find(|(tag, _)| *tag == wanted)
            .map(|(_, value)| *value)
    };
    let strtab = value_of(DT_STRTAB).ok_or("its dynamic table names no string table")?;
    let strsz = value_of(DT_STRSZ).ok_or("its dynamic table gives no string table size")?;
    let strtab_offset = size(file_offset(strtab).ok_or("its string table is not loaded")?)?;
    let old_strings = bytes
        .get(strtab_offset..strtab_offset + size(strsz)?)
        .ok_or_else(cut)?;

    // The new dynamic entries: one DT_RPATH where the first library folder
    // entry was (or last), the string table moved, the end marker.
    let mut strings = old_strings.to_vec();
    let folders_at = strings.len() as u64;
    strings.extend_from_slice(folders.as_bytes());
    strings.push(0);
    let mut new_entries = Vec::new();
    let mut rpath_placed = false;
    for (tag, value) in &entries {
        match *tag {
            DT_RPATH | DT_RUNPATH if !rpath_placed => {
                new_entries.push((DT_RPATH, folders_at));
                rpath_placed = true;
            }
            DT_RPATH | DT_RUNPATH => {}
            _ => new_entries.push((*tag, *value)),
        }
    }
    if !rpath_placed {
        new_entries.push((DT_RPATH, folders_at));
    }
    let moves_dynamic = new_entries.len() + 1 > capacity;

    // The new part: program headers, loader path, strings, dynamic table.
    let adds_phdr = interpreter.is_some() && !segments.iter().any(|s| s.kind == PT_PHDR);
    let count = phnum + 1 + usize::from(adds_phdr);
    if count >= 0xffff {
        return Err("has too many program headers to add one".into());
    }
    let offset = align_up(bytes.len() as u64, PAGE);
    let vaddr = align_up(
        loads
            .iter()
            .map(|load| load.vaddr + load.memsz)
            .max()
            .unwrap_or(0),
        PAGE,
    );
    let headers_size = (count * PHENT) as u64;
    let interp_at = headers_size;
    let interp_size = interpreter.map_or(0, |path| path.len() as u64 + 1);
    let strings_at = interp_at + interp_size;
    let dynamic_at = align_up(strings_at + strings.len() as u64, 8);
    let dynamic_size = if moves_dynamic {
        ((new_entries.len() + 1) * DYNENT) as u64
    } else {
        0
    };
    let part_size = if moves_dynamic {
        dynamic_at + dynamic_size
    } else {
        strings_at + strings.len() as u64
    };
    let strings_vaddr = vaddr + strings_at;

    // The new program header table.
    let old_interp = interp_index.map(|index| segments[index].vaddr);
    if let Some(index) = interp_index {
        segments[index].place(offset + interp_at, vaddr + interp_at, interp_size);
    }
    if moves_dynamic {
        segments[dynamic_index].place(offset + dynamic_at, vaddr + dynamic_at, dynamic_size);
    }
    for segment in &mut segments {
        if segment.kind == PT_PHDR {
            segment.place(offset, vaddr, headers_size);
        }
    }
    let last_load = segments
        .iter()
        .rposition(|segment| segment.kind == PT_LOAD)
        .unwrap_or(segments.len() - 1);
    segments.insert(
        last_load + 1,
        Segment {
            kind: PT_LOAD,
            flags: if moves_dynamic { PF_R | PF_W } else { PF_R },
            offset,
            vaddr,
            paddr: vaddr,
            filesz: part_size,
            memsz: part_size,
            align: PAGE,
        },
    );
    if adds_phdr {
        let mut phdr = Segment {
            kind: PT_PHDR,
            flags: PF_R,
            offset: 0,
            vaddr: 0,
            paddr: 0,
            filesz: 0,
            memsz: 0,
            align: 8,
        };
        phdr.place(offset, vaddr, headers_size);
        segments.insert(0, phdr);
    }

    // The dynamic entries, with the string table moved.
    let mut dynamic_bytes = Vec::new();
    for (tag, value) in &new_entries {
        let value = match *tag {
            DT_STRTAB => strings_vaddr,
            DT_STRSZ => strings.len() as u64,
            _ => *value,
        };
        dynamic_bytes.extend_from_slice(&tag.to_le_bytes());
        dynamic_bytes.extend_from_slice(&value.to_le_bytes());
    }

    let mut out = bytes.to_vec();
    if !moves_dynamic {
        // Written over the old table; what is left of it becomes end markers.
        let at = size(dynamic.offset)?;
        let room = capacity * DYNENT;
        out[at..at + room].fill(0);
        out[at..at + dynamic_bytes.len()].copy_from_slice(&dynamic_bytes);
    }
    out.resize(size(offset)?, 0);
    for segment in &segments {
        segment.write(&mut out);
    }
    if let Some(path) = interpreter {
        out.extend_from_slice(path.as_bytes());
        out.push(0);
    }
    out.extend_from_slice(&strings);
    if moves_dynamic {
        out.resize(size(offset + dynamic_at)?, 0);
        out.extend_from_slice(&dynamic_bytes);
        out.extend_from_slice(&[0; DYNENT]);
    }
    debug_assert_eq!(out.len() as u64, offset + part_size);

    // The ELF header names the new table.
    put_u64(&mut out, 32, offset);
    put_u16(&mut out, 56, count as u16);

    // Section headers, where there are any, follow what moved.
    let shoff = size(u64_at(bytes, 40).ok_or_else(cut)?)?;
    let shentsize = usize::from(u16_at(bytes, 58).ok_or_else(cut)?);
    let mut shnum = usize::from(u16_at(bytes, 60).ok_or_else(cut)?);
    if shoff != 0 && shnum == 0 && shentsize == 64 {
        shnum = size(u64_at(bytes, shoff + 32).unwrap_or(0))?;
    }
    if shoff == 0 || shentsize != 64 || shoff + shnum * 64 > bytes.len() {
        shnum = 0;
    }
    for index in 0..shnum {
        let at = shoff + index * 64;
        let kind = u32_at(&out, at + 4).ok_or_else(cut)?;
        let address = u64_at(&out, at + 16).ok_or_else(cut)?;
        let mut point = |offset_in_part: u64, size_of: u64| {
            put_u64(&mut out, at + 16, vaddr + offset_in_part);
            put_u64(&mut out, at + 24, offset + offset_in_part);
            put_u64(&mut out, at + 32, size_of);
        };
        match kind {
            SHT_PROGBITS
                if interpreter.is_some()
                    && old_interp.is_some_and(|old| old == address && address != 0) =>
            {
                point(interp_at, interp_size);
            }
            SHT_STRTAB if address == strtab && address != 0 => {
                point(strings_at, strings.len() as u64);
            }
            SHT_DYNAMIC if moves_dynamic => point(dynamic_at, dynamic_size),
            _ => {}
        }
    }
    Ok(out)
}
