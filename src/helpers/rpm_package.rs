//! Native RPM reader for `target_extract_rpm`: lead, signature and main
//! header, then the payload (a cpio archive) decompressed in-process. Nothing
//! here starts a program or opens an RPM database; package authenticity is the
//! recipe's pinned SHA256 contract, so signatures are skipped.
use anyhow::{Context, Result, bail, ensure};
use std::collections::HashMap;
use std::io::{BufReader, Read};

const LEAD_MAGIC: [u8; 4] = [0xed, 0xab, 0xee, 0xdb];
const HEADER_MAGIC: [u8; 4] = [0x8e, 0xad, 0xe8, 0x01];
const TAG_PAYLOADFORMAT: u32 = 1124;
const TAG_PAYLOADCOMPRESSOR: u32 = 1125;
const TAG_DIRINDEXES: u32 = 1116;
const TAG_BASENAMES: u32 = 1117;
const TAG_DIRNAMES: u32 = 1118;
const TAG_OLDFILENAMES: u32 = 1027;
const TAG_FILEUSERNAME: u32 = 1039;
const TAG_FILEGROUPNAME: u32 = 1040;
const TAG_FILECAPS: u32 = 5010;
/// Limits that keep a corrupt header from asking for absurd allocations.
const MAX_INDEX: u32 = 1 << 20;
const MAX_HEADER_DATA: u32 = 1 << 30;

/// The header fields extraction needs, indexed like the package file list.
pub struct Header {
    /// Absolute paths (`/usr/bin/x`).
    pub files: Vec<String>,
    pub users: Vec<String>,
    pub groups: Vec<String>,
    /// Text file capabilities (`cap_net_raw=ep`), empty when none.
    pub caps: Vec<String>,
    compressor: String,
    format: String,
}

struct Entry {
    tag: u32,
    kind: u32,
    offset: usize,
    count: usize,
}

struct Raw {
    entries: Vec<Entry>,
    data: Vec<u8>,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Read one header structure. Returns it and the bytes consumed.
fn read_raw<R: Read>(reader: &mut R) -> Result<(Raw, u64)> {
    let mut intro = [0u8; 16];
    reader
        .read_exact(&mut intro)
        .context("truncated RPM header")?;
    ensure!(
        intro[..4] == HEADER_MAGIC,
        "RPM header magic missing; the file is not a valid RPM, fetch it again"
    );
    let (count, size) = (be32(&intro[8..]), be32(&intro[12..]));
    ensure!(
        count <= MAX_INDEX && size <= MAX_HEADER_DATA,
        "RPM header is implausibly large; the file is corrupt, fetch it again"
    );
    let mut index = vec![0u8; count as usize * 16];
    reader
        .read_exact(&mut index)
        .context("truncated RPM header index")?;
    let mut data = vec![0u8; size as usize];
    reader
        .read_exact(&mut data)
        .context("truncated RPM header data")?;
    let entries = index
        .as_chunks::<16>()
        .0
        .iter()
        .map(|c| Entry {
            tag: be32(c),
            kind: be32(&c[4..]),
            offset: be32(&c[8..]) as usize,
            count: be32(&c[12..]) as usize,
        })
        .collect();
    let used = 16 + index.len() as u64 + data.len() as u64;
    Ok((Raw { entries, data }, used))
}

impl Raw {
    fn find(&self, tag: u32) -> Option<&Entry> {
        self.entries.iter().find(|e| e.tag == tag)
    }

    fn strings(&self, tag: u32) -> Result<Option<Vec<String>>> {
        let Some(e) = self.find(tag) else {
            return Ok(None);
        };
        // 6 STRING, 8 STRING_ARRAY, 9 I18NSTRING
        ensure!(
            matches!(e.kind, 6 | 8 | 9),
            "RPM header tag {tag} has an unexpected type"
        );
        let mut out = Vec::with_capacity(e.count);
        let mut at = e.offset;
        for _ in 0..e.count {
            let rest = self
                .data
                .get(at..)
                .context("RPM header string out of range")?;
            let len = rest
                .iter()
                .position(|b| *b == 0)
                .context("unterminated RPM header string")?;
            out.push(
                String::from_utf8(rest[..len].to_vec()).context("non-UTF8 RPM header string")?,
            );
            at += len + 1;
        }
        Ok(Some(out))
    }

    fn int32s(&self, tag: u32) -> Result<Option<Vec<u32>>> {
        let Some(e) = self.find(tag) else {
            return Ok(None);
        };
        ensure!(e.kind == 4, "RPM header tag {tag} has an unexpected type");
        let bytes = self
            .data
            .get(e.offset..e.offset + e.count * 4)
            .context("RPM header integers out of range")?;
        Ok(Some(
            bytes.as_chunks::<4>().0.iter().map(|c| be32(c)).collect(),
        ))
    }
}

/// Parse the lead and both headers from `reader`, leaving it at the payload.
pub fn read_header<R: Read>(reader: &mut R) -> Result<Header> {
    let mut lead = [0u8; 96];
    reader
        .read_exact(&mut lead)
        .context("truncated RPM lead; the file is not a complete RPM, fetch it again")?;
    ensure!(
        lead[..4] == LEAD_MAGIC,
        "not an RPM file (lead magic missing); fetch the archive again"
    );
    let (_, used) = read_raw(reader).context("RPM signature header")?;
    let pad = (8 - used % 8) % 8;
    let mut skip = [0u8; 8];
    reader.read_exact(&mut skip[..pad as usize])?;
    let (raw, _) = read_raw(reader).context("RPM main header")?;

    let basenames = raw.strings(TAG_BASENAMES)?;
    let files = if let Some(basenames) = basenames {
        let dirs = raw
            .strings(TAG_DIRNAMES)?
            .context("RPM has basenames but no dirnames")?;
        let indexes = raw
            .int32s(TAG_DIRINDEXES)?
            .context("RPM has basenames but no dirindexes")?;
        ensure!(
            indexes.len() == basenames.len(),
            "RPM file list is inconsistent"
        );
        let mut files = Vec::with_capacity(basenames.len());
        for (base, dir) in basenames.iter().zip(indexes) {
            let dir = dirs
                .get(dir as usize)
                .context("RPM directory index out of range")?;
            files.push(format!("{dir}{base}"));
        }
        files
    } else {
        raw.strings(TAG_OLDFILENAMES)?.unwrap_or_default()
    };
    let per_file = |tag| -> Result<Vec<String>> {
        let values = raw.strings(tag)?.unwrap_or_default();
        ensure!(
            values.is_empty() || values.len() == files.len(),
            "RPM file metadata is inconsistent"
        );
        Ok(values)
    };
    let (users, groups, caps) = (
        per_file(TAG_FILEUSERNAME)?,
        per_file(TAG_FILEGROUPNAME)?,
        per_file(TAG_FILECAPS)?,
    );
    let one = |tag, default: &str| -> Result<String> {
        Ok(raw
            .strings(tag)?
            .and_then(|v| v.into_iter().next())
            .unwrap_or_else(|| default.to_owned()))
    };
    Ok(Header {
        compressor: one(TAG_PAYLOADCOMPRESSOR, "gzip")?,
        format: one(TAG_PAYLOADFORMAT, "cpio")?,
        files,
        users,
        groups,
        caps,
    })
}

/// A decompressing reader over the payload bytes that follow the headers.
pub fn payload<R: Read + 'static>(header: &Header, reader: R) -> Result<Box<dyn Read>> {
    ensure!(
        header.format == "cpio",
        "RPM payload format `{}` is not cpio; Recipe reads only cpio payloads, use a plain RPM",
        header.format
    );
    Ok(match header.compressor.as_str() {
        "zstd" => Box::new(zstd::Decoder::new(reader)?),
        "xz" => Box::new(xz2::read::XzDecoder::new(reader)),
        "lzma" => Box::new(xz2::read::XzDecoder::new_stream(
            reader,
            xz2::stream::Stream::new_lzma_decoder(u64::MAX)?,
        )),
        "gzip" => Box::new(flate2::read::MultiGzDecoder::new(reader)),
        "bzip2" => Box::new(bzip2::read::BzDecoder::new(reader)),
        "" | "identity" => Box::new(reader),
        other => bail!(
            "RPM payload compressor `{other}` is not supported; Recipe reads zstd, xz, lzma, gzip, bzip2 and uncompressed payloads"
        ),
    })
}

/// One cpio (newc) member header.
pub struct CpioEntry {
    pub name: String,
    pub ino: u32,
    pub mode: u32,
    pub nlink: u32,
    pub size: u64,
    pub dev: (u32, u32),
}

/// Sequential reader of an RPM payload (cpio "070701"/"070702").
pub struct Cpio<R: Read> {
    reader: BufReader<R>,
    /// Bytes of the current member's data not yet consumed, plus its padding.
    remaining: u64,
    padding: u64,
}

impl<R: Read> Cpio<R> {
    pub fn new(reader: R) -> Self {
        Cpio {
            reader: BufReader::with_capacity(1 << 20, reader),
            remaining: 0,
            padding: 0,
        }
    }

    /// Advance to the next member; `None` at the archive trailer. Any unread
    /// data of the previous member is skipped.
    pub fn next(&mut self) -> Result<Option<CpioEntry>> {
        std::io::copy(
            &mut (&mut self.reader).take(self.remaining + self.padding),
            &mut std::io::sink(),
        )?;
        self.remaining = 0;
        self.padding = 0;
        let mut head = [0u8; 110];
        self.reader
            .read_exact(&mut head)
            .context("truncated RPM payload")?;
        if &head[..6] == b"07070X" {
            bail!(
                "RPM payload uses the stripped cpio format (07070X, files of 4 GiB or more); \
                 Recipe cannot read it natively yet, report this package to the Recipe maintainers"
            );
        }
        ensure!(
            &head[..6] == b"070701" || &head[..6] == b"070702",
            "RPM payload is not a cpio archive; fetch the package again"
        );
        let field = |n: usize| -> Result<u32> {
            let text = std::str::from_utf8(&head[6 + n * 8..14 + n * 8])?;
            u32::from_str_radix(text, 16).context("bad cpio header field")
        };
        let (ino, mode, nlink, size) = (field(0)?, field(1)?, field(4)?, field(6)?);
        let dev = (field(7)?, field(8)?);
        let namesize = field(11)? as u64;
        ensure!((1..=4096 + 2).contains(&namesize), "bad cpio name length");
        let mut name = vec![0u8; namesize as usize];
        self.reader
            .read_exact(&mut name)
            .context("truncated RPM payload")?;
        let pad = (4 - (110 + namesize) % 4) % 4;
        std::io::copy(&mut (&mut self.reader).take(pad), &mut std::io::sink())?;
        ensure!(name.pop() == Some(0), "cpio name is not terminated");
        let name = String::from_utf8(name).context("non-UTF8 package path")?;
        if name == "TRAILER!!!" {
            return Ok(None);
        }
        self.remaining = size as u64;
        self.padding = (4 - size as u64 % 4) % 4;
        Ok(Some(CpioEntry {
            name,
            ino,
            mode,
            nlink,
            size: size as u64,
            dev,
        }))
    }

    /// The current member's data.
    pub fn data(&mut self) -> impl Read + '_ {
        let n = self.remaining;
        self.remaining = 0;
        (&mut self.reader).take(n)
    }
}

/// Index of every non-ghost header file by its path without leading `/`.
pub fn index(header: &Header) -> HashMap<&str, usize> {
    header
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| (f.trim_start_matches('/'), i))
        .collect()
}
