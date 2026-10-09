//! Issue #345 — a bounded, read-only OLE Compound File (MS-CFB) reader.
//!
//! An encrypted Office document is not a ZIP: MS-OFFCRYPTO (ECMA-376
//! Part 2 §"Encryption") wraps the whole OOXML package in a compound file
//! whose root storage holds an `EncryptionInfo` stream (the key-derivation
//! parameters) and an `EncryptedPackage` stream (the encrypted ZIP). Before
//! this module such a file failed as "invalid Zip archive"; now
//! [`sniff_compound_file`] recognises the signature and tells an encrypted
//! package apart from any other compound file (a Word 97–2003 `.doc`, an
//! `.xls`, …), and [`CompoundFile::stream`] hands the decryptor
//! (`opc::offcrypto`) the two streams.
//!
//! The input is attacker-shaped, and the wasm worker aborts on a panic, so
//! the reader never trusts a size or a sector number: every sector index is
//! range-checked against the file, every chain walk is capped at the number
//! of sectors the file can hold (a cycle ends the walk with an error), and
//! a stream's declared size never allocates more than the bytes its chain
//! actually covers. Only what the decryptor needs is implemented: top-level
//! streams of version-3 (512-byte sectors) and version-4 (4096-byte
//! sectors) files, regular and mini streams. Writing is out of scope.

/// The eight signature bytes every compound file starts with.
pub const CFB_SIGNATURE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

const HEADER_LEN: usize = 512;
const DIR_ENTRY_LEN: usize = 128;
/// Sector "pointers" at or above this value are markers, never sectors
/// (`MAXREGSECT` = `0xFFFFFFFA`; free / end-of-chain / FAT / DIFAT).
const MAX_REG_SECT: u32 = 0xFFFF_FFFA;
const END_OF_CHAIN: u32 = 0xFFFF_FFFE;
const NO_STREAM: u32 = 0xFFFF_FFFF;
const TYPE_STREAM: u8 = 2;
const TYPE_ROOT: u8 = 5;

/// What a byte blob that starts with [`CFB_SIGNATURE`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompoundFileKind {
    /// An MS-OFFCRYPTO encrypted OOXML package: the root storage carries
    /// both an `EncryptionInfo` and an `EncryptedPackage` stream.
    EncryptedPackage,
    /// Any other compound file — a legacy binary Office document (`.doc`,
    /// `.xls`, `.ppt`), or one too damaged to read its directory.
    Other,
}

/// Issue #345 — classify `bytes` when they are an OLE compound file;
/// `None` for anything else (a ZIP `.docx`, plain text, …). A file whose
/// directory cannot be read is still a compound file ([`CompoundFileKind::
/// Other`]), never "not a zip".
pub fn sniff_compound_file(bytes: &[u8]) -> Option<CompoundFileKind> {
    if !bytes.starts_with(&CFB_SIGNATURE) {
        return None;
    }
    let kind = match CompoundFile::parse(bytes) {
        Ok(cf) if cf.has_stream("EncryptionInfo") && cf.has_stream("EncryptedPackage") => {
            CompoundFileKind::EncryptedPackage
        }
        _ => CompoundFileKind::Other,
    };
    Some(kind)
}

/// Why a compound file could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfbError(pub &'static str);

impl std::fmt::Display for CfbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "malformed compound file: {}", self.0)
    }
}

/// One directory entry the reader keeps (top-level streams only need
/// these fields).
#[derive(Debug, Clone)]
struct DirEntry {
    name: String,
    kind: u8,
    left: u32,
    right: u32,
    child: u32,
    start: u32,
    size: u64,
}

/// A parsed compound file: the sector allocation tables and the directory,
/// borrowing the file bytes.
#[derive(Debug)]
pub struct CompoundFile<'a> {
    bytes: &'a [u8],
    sector_len: usize,
    mini_sector_len: usize,
    mini_cutoff: u64,
    fat: Vec<u32>,
    mini_fat: Vec<u32>,
    dir: Vec<DirEntry>,
    /// The root entry's stream: the container of every mini stream.
    mini_stream: Vec<u8>,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

impl<'a> CompoundFile<'a> {
    /// Parse the header, the FAT (through the DIFAT), the directory and
    /// the mini FAT / mini stream. Every structure is bounded by the file.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, CfbError> {
        let bad = CfbError;
        if bytes.len() < HEADER_LEN || !bytes.starts_with(&CFB_SIGNATURE) {
            return Err(bad("no compound-file header"));
        }
        let major = u16_at(bytes, 0x1A).ok_or(bad("header"))?;
        let sector_shift = u16_at(bytes, 0x1E).ok_or(bad("header"))?;
        let mini_shift = u16_at(bytes, 0x20).ok_or(bad("header"))?;
        let sector_len = match (major, sector_shift) {
            (3, 9) => 512,
            (4, 12) => 4096,
            _ => return Err(bad("unsupported version / sector size")),
        };
        if mini_shift != 6 {
            return Err(bad("unsupported mini sector size"));
        }
        let mini_sector_len = 64;
        let num_fat_sectors = u32_at(bytes, 0x2C).ok_or(bad("header"))? as usize;
        let first_dir = u32_at(bytes, 0x30).ok_or(bad("header"))?;
        let mini_cutoff = u64::from(u32_at(bytes, 0x38).ok_or(bad("header"))?);
        let first_mini_fat = u32_at(bytes, 0x3C).ok_or(bad("header"))?;
        let first_difat = u32_at(bytes, 0x44).ok_or(bad("header"))?;

        /* Sector n lives at (n + 1) * sector_len; a v4 header still
        occupies a whole first sector. */
        let sector_count = bytes.len().saturating_sub(sector_len) / sector_len;
        let mut cf = CompoundFile {
            bytes,
            sector_len,
            mini_sector_len,
            mini_cutoff,
            fat: Vec::new(),
            mini_fat: Vec::new(),
            dir: Vec::new(),
            mini_stream: Vec::new(),
        };
        if num_fat_sectors > sector_count {
            return Err(bad("FAT larger than the file"));
        }

        /* The DIFAT: 109 FAT sector numbers in the header, the rest in a
        chain of DIFAT sectors (each ends with the next one's number). */
        let mut fat_sectors: Vec<u32> = Vec::with_capacity(num_fat_sectors.min(109));
        for i in 0..109 {
            if fat_sectors.len() == num_fat_sectors {
                break;
            }
            let s = u32_at(bytes, 0x4C + 4 * i).ok_or(bad("header"))?;
            if s < MAX_REG_SECT {
                fat_sectors.push(s);
            }
        }
        let per_difat = sector_len / 4 - 1;
        let mut difat = first_difat;
        let mut hops = 0usize;
        while fat_sectors.len() < num_fat_sectors && difat < MAX_REG_SECT {
            hops += 1;
            if hops > sector_count {
                return Err(bad("DIFAT chain loops"));
            }
            let sec = cf.sector(difat).ok_or(bad("DIFAT sector out of range"))?;
            for i in 0..per_difat {
                if fat_sectors.len() == num_fat_sectors {
                    break;
                }
                let s = u32_at(sec, 4 * i).ok_or(bad("DIFAT"))?;
                if s < MAX_REG_SECT {
                    fat_sectors.push(s);
                }
            }
            difat = u32_at(sec, sector_len - 4).ok_or(bad("DIFAT"))?;
        }

        let mut fat = Vec::with_capacity(fat_sectors.len() * (sector_len / 4));
        for s in fat_sectors {
            let sec = cf.sector(s).ok_or(bad("FAT sector out of range"))?;
            fat.extend(
                sec.chunks_exact(4)
                    .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])),
            );
        }
        cf.fat = fat;

        /* The directory: a regular chain of 128-byte entries. */
        let dir_bytes = cf.read_chain(first_dir, None)?;
        cf.dir = dir_bytes
            .chunks_exact(DIR_ENTRY_LEN)
            .map(|e| {
                let name_len = usize::from(u16_at(e, 0x40).unwrap_or(0)).min(64);
                let units: Vec<u16> = e[..name_len.saturating_sub(2)]
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                let size = u64_at(e, 0x78).unwrap_or(0);
                DirEntry {
                    name: String::from_utf16_lossy(&units),
                    kind: e[0x42],
                    left: u32_at(e, 0x44).unwrap_or(NO_STREAM),
                    right: u32_at(e, 0x48).unwrap_or(NO_STREAM),
                    child: u32_at(e, 0x4C).unwrap_or(NO_STREAM),
                    start: u32_at(e, 0x74).unwrap_or(END_OF_CHAIN),
                    /* A version-3 file only defines the low 32 bits
                    (some writers leave garbage in the high half). */
                    size: if major == 3 { size & 0xFFFF_FFFF } else { size },
                }
            })
            .collect();
        let root = cf.dir.first().ok_or(bad("empty directory"))?.clone();
        if root.kind != TYPE_ROOT {
            return Err(bad("first directory entry is not the root"));
        }

        /* The mini FAT (regular chain) and the mini stream (the root
        entry's own regular-sector stream). */
        if first_mini_fat < MAX_REG_SECT {
            let mf = cf.read_chain(first_mini_fat, None)?;
            cf.mini_fat = mf
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
        }
        if root.start < MAX_REG_SECT {
            cf.mini_stream = cf.read_chain(root.start, Some(root.size))?;
        }
        Ok(cf)
    }

    /// Sector `n`'s bytes, `None` when it lies outside the file.
    fn sector(&self, n: u32) -> Option<&'a [u8]> {
        let start = (n as usize).checked_add(1)?.checked_mul(self.sector_len)?;
        self.bytes.get(start..start.checked_add(self.sector_len)?)
    }

    /// Follow a regular-sector chain from `start`, concatenating sectors,
    /// truncated to `size` when given. Capped at the file's sector count
    /// (a cycle is an error, not a hang) and never allocating past the
    /// bytes the chain actually covers.
    fn read_chain(&self, start: u32, size: Option<u64>) -> Result<Vec<u8>, CfbError> {
        if size.is_some_and(|s| s > self.bytes.len() as u64) {
            return Err(CfbError("stream larger than the file"));
        }
        let cap = size.map(|s| s.min(self.bytes.len() as u64) as usize);
        let mut out = Vec::new();
        let mut cur = start;
        let mut hops = 0usize;
        let limit = self.bytes.len() / self.sector_len + 1;
        while cur < MAX_REG_SECT {
            if cap.is_some_and(|c| out.len() >= c) {
                break;
            }
            hops += 1;
            if hops > limit {
                return Err(CfbError("sector chain loops"));
            }
            let sec = self
                .sector(cur)
                .ok_or(CfbError("sector chain leaves the file"))?;
            out.extend_from_slice(sec);
            cur = *self
                .fat
                .get(cur as usize)
                .ok_or(CfbError("sector outside the FAT"))?;
        }
        if let Some(c) = cap {
            if out.len() < c {
                return Err(CfbError("stream shorter than its declared size"));
            }
            out.truncate(c);
        }
        Ok(out)
    }

    /// Follow a mini-sector chain inside the mini stream.
    fn read_mini_chain(&self, start: u32, size: u64) -> Result<Vec<u8>, CfbError> {
        let cap = size.min(self.mini_stream.len() as u64) as usize;
        if (size as usize) > cap {
            return Err(CfbError("mini stream shorter than its declared size"));
        }
        let mut out = Vec::with_capacity(cap);
        let mut cur = start;
        let mut hops = 0usize;
        let limit = self.mini_stream.len() / self.mini_sector_len + 1;
        while cur < MAX_REG_SECT && out.len() < cap {
            hops += 1;
            if hops > limit {
                return Err(CfbError("mini sector chain loops"));
            }
            let at = (cur as usize)
                .checked_mul(self.mini_sector_len)
                .ok_or(CfbError("mini sector out of range"))?;
            let sec = self
                .mini_stream
                .get(at..at + self.mini_sector_len)
                .ok_or(CfbError("mini sector out of range"))?;
            out.extend_from_slice(sec);
            cur = *self
                .mini_fat
                .get(cur as usize)
                .ok_or(CfbError("mini sector outside the mini FAT"))?;
        }
        if out.len() < cap {
            return Err(CfbError("mini stream shorter than its declared size"));
        }
        out.truncate(cap);
        Ok(out)
    }

    /// The root storage's direct children (the red-black tree under the
    /// root entry), in tree order. Bounded by the directory size.
    fn root_children(&self) -> Vec<&DirEntry> {
        let mut out = Vec::new();
        let Some(root) = self.dir.first() else {
            return out;
        };
        let mut stack = vec![root.child];
        let mut seen = vec![false; self.dir.len()];
        while let Some(i) = stack.pop() {
            let Some(e) = self.dir.get(i as usize) else {
                continue;
            };
            if std::mem::replace(&mut seen[i as usize], true) {
                continue;
            }
            out.push(e);
            stack.push(e.left);
            stack.push(e.right);
        }
        out
    }

    fn find_stream(&self, name: &str) -> Option<&DirEntry> {
        self.root_children()
            .into_iter()
            .find(|e| e.kind == TYPE_STREAM && e.name.eq_ignore_ascii_case(name))
    }

    /// `true` when the root storage holds a stream called `name`
    /// (case-insensitive, as MS-CFB compares names).
    pub fn has_stream(&self, name: &str) -> bool {
        self.find_stream(name).is_some()
    }

    /// The bytes of the top-level stream `name`, or `None` when it is
    /// absent; an error when its sectors cannot be read.
    pub fn stream(&self, name: &str) -> Option<Result<Vec<u8>, CfbError>> {
        let e = self.find_stream(name)?;
        Some(if e.size < self.mini_cutoff {
            self.read_mini_chain(e.start, e.size)
        } else {
            self.read_chain(e.start, Some(e.size))
        })
    }
}

/// Test support: a minimal version-3 compound-file WRITER (regular
/// sectors only — every stream is padded past the 4096-byte mini-stream
/// cutoff unless `mini` is set). Used by this crate's tests and the
/// encrypted-package fixtures; never by the reader.
#[doc(hidden)]
pub mod test_writer {
    use super::*;

    /// Build a version-3 compound file whose root storage holds `streams`
    /// (`(name, bytes)`). Streams shorter than 4096 bytes go to the mini
    /// stream, exactly as Office writes them.
    pub fn build(streams: &[(&str, &[u8])]) -> Vec<u8> {
        const SEC: usize = 512;
        let n = streams.len();
        /* Mini stream: concatenated small streams, 64-byte aligned. */
        let mut mini_stream: Vec<u8> = Vec::new();
        let mut mini_fat: Vec<u32> = Vec::new();
        let mut placement: Vec<(u32, u64, bool)> = Vec::new(); // (start, size, mini)
        let mut regular: Vec<(usize, &[u8])> = Vec::new();
        for (i, (_, data)) in streams.iter().enumerate() {
            if data.len() < 4096 {
                let start = (mini_stream.len() / 64) as u32;
                let secs = data.len().div_ceil(64).max(1);
                for k in 0..secs {
                    let idx = start + k as u32;
                    mini_fat.push(if k + 1 == secs { END_OF_CHAIN } else { idx + 1 });
                }
                mini_stream.extend_from_slice(data);
                mini_stream.resize((start as usize + secs) * 64, 0);
                placement.push((start, data.len() as u64, true));
            } else {
                regular.push((i, data));
                placement.push((0, data.len() as u64, false));
            }
        }
        /* Regular sectors, in order: directory, mini FAT, mini stream,
        regular streams, then the FAT itself. */
        let dir_entries = n + 1;
        let dir_secs = (dir_entries * DIR_ENTRY_LEN).div_ceil(SEC);
        let mini_fat_secs = (mini_fat.len() * 4).div_ceil(SEC);
        let mut sectors: Vec<Vec<u8>> = Vec::new();
        let mut fat: Vec<u32> = Vec::new();
        let push_chain = |sectors: &mut Vec<Vec<u8>>, fat: &mut Vec<u32>, data: &[u8]| -> u32 {
            let start = sectors.len() as u32;
            let count = data.len().div_ceil(SEC).max(1);
            for k in 0..count {
                let mut sec = data
                    .get(k * SEC..((k + 1) * SEC).min(data.len()))
                    .unwrap_or(&[])
                    .to_vec();
                sec.resize(SEC, 0);
                sectors.push(sec);
                fat.push(if k + 1 == count {
                    END_OF_CHAIN
                } else {
                    start + k as u32 + 1
                });
            }
            start
        };
        /* Placeholder directory; patched once stream starts are known. */
        let dir_start = push_chain(&mut sectors, &mut fat, &vec![0u8; dir_secs * SEC]);
        let mini_fat_bytes: Vec<u8> = mini_fat
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .chain(std::iter::repeat_n(
                0xFFu8,
                mini_fat_secs * SEC - mini_fat.len() * 4,
            ))
            .collect();
        let mini_fat_start = if mini_fat.is_empty() {
            END_OF_CHAIN
        } else {
            push_chain(&mut sectors, &mut fat, &mini_fat_bytes)
        };
        let mini_stream_start = if mini_stream.is_empty() {
            END_OF_CHAIN
        } else {
            push_chain(&mut sectors, &mut fat, &mini_stream)
        };
        for (i, data) in regular {
            let start = push_chain(&mut sectors, &mut fat, data);
            placement[i].0 = start;
        }
        /* The FAT covers every sector including its own. */
        let mut fat_secs = 1;
        while (sectors.len() + fat_secs) * 4 > fat_secs * SEC {
            fat_secs += 1;
        }
        let fat_start = sectors.len() as u32;
        /* FATSECT marks the FAT's own sectors. */
        fat.extend(std::iter::repeat_n(0xFFFF_FFFD, fat_secs));
        fat.resize(fat_secs * SEC / 4, 0xFFFF_FFFF);
        let fat_bytes: Vec<u8> = fat.iter().flat_map(|v| v.to_le_bytes()).collect();
        for k in 0..fat_secs {
            sectors.push(fat_bytes[k * SEC..(k + 1) * SEC].to_vec());
        }
        /* Directory: root + one stream entry each, chained as a
        right-leaning list (a valid, if unbalanced, red-black tree with
        every node black). */
        let mut dir = vec![0u8; dir_secs * SEC];
        let entry = |dir: &mut Vec<u8>,
                     idx: usize,
                     name: &str,
                     kind: u8,
                     right: u32,
                     child: u32,
                     start: u32,
                     size: u64| {
            let e = &mut dir[idx * DIR_ENTRY_LEN..(idx + 1) * DIR_ENTRY_LEN];
            let units: Vec<u16> = name.encode_utf16().collect();
            for (k, u) in units.iter().enumerate() {
                e[2 * k..2 * k + 2].copy_from_slice(&u.to_le_bytes());
            }
            e[0x40..0x42].copy_from_slice(&(((units.len() + 1) * 2) as u16).to_le_bytes());
            e[0x42] = kind;
            e[0x43] = 1; // black
            e[0x44..0x48].copy_from_slice(&NO_STREAM.to_le_bytes());
            e[0x48..0x4C].copy_from_slice(&right.to_le_bytes());
            e[0x4C..0x50].copy_from_slice(&child.to_le_bytes());
            e[0x74..0x78].copy_from_slice(&start.to_le_bytes());
            e[0x78..0x80].copy_from_slice(&size.to_le_bytes());
        };
        entry(
            &mut dir,
            0,
            "Root Entry",
            TYPE_ROOT,
            NO_STREAM,
            if n > 0 { 1 } else { NO_STREAM },
            mini_stream_start,
            mini_stream.len() as u64,
        );
        for (i, (name, _)) in streams.iter().enumerate() {
            let (start, size, _) = placement[i];
            let right = if i + 1 < n { (i + 2) as u32 } else { NO_STREAM };
            entry(
                &mut dir,
                i + 1,
                name,
                TYPE_STREAM,
                right,
                NO_STREAM,
                start,
                size,
            );
        }
        /* Unused directory slots stay zeroed except for their sibling /
        child pointers (MS-CFB: NOSTREAM). */
        for idx in dir_entries..dir_secs * SEC / DIR_ENTRY_LEN {
            let e = &mut dir[idx * DIR_ENTRY_LEN..(idx + 1) * DIR_ENTRY_LEN];
            e[0x44..0x50].fill(0xFF);
        }
        for k in 0..dir_secs {
            sectors[dir_start as usize + k] = dir[k * SEC..(k + 1) * SEC].to_vec();
        }
        /* Header. */
        let mut h = vec![0u8; HEADER_LEN];
        h[..8].copy_from_slice(&CFB_SIGNATURE);
        h[0x18..0x1A].copy_from_slice(&0x003Eu16.to_le_bytes());
        h[0x1A..0x1C].copy_from_slice(&3u16.to_le_bytes());
        h[0x1C..0x1E].copy_from_slice(&0xFFFEu16.to_le_bytes());
        h[0x1E..0x20].copy_from_slice(&9u16.to_le_bytes());
        h[0x20..0x22].copy_from_slice(&6u16.to_le_bytes());
        h[0x2C..0x30].copy_from_slice(&(fat_secs as u32).to_le_bytes());
        h[0x30..0x34].copy_from_slice(&dir_start.to_le_bytes());
        h[0x38..0x3C].copy_from_slice(&4096u32.to_le_bytes());
        h[0x3C..0x40].copy_from_slice(&mini_fat_start.to_le_bytes());
        h[0x40..0x44].copy_from_slice(&(mini_fat_secs as u32).to_le_bytes());
        h[0x44..0x48].copy_from_slice(&END_OF_CHAIN.to_le_bytes());
        for i in 0..109 {
            let v = if i < fat_secs {
                fat_start + i as u32
            } else {
                0xFFFF_FFFF
            };
            h[0x4C + 4 * i..0x50 + 4 * i].copy_from_slice(&v.to_le_bytes());
        }
        let mut out = h;
        for s in sectors {
            out.extend_from_slice(&s);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_small_and_large_streams_back() {
        let small = b"EncryptionInfo bytes".to_vec();
        let large: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let cfb = test_writer::build(&[("EncryptionInfo", &small), ("EncryptedPackage", &large)]);
        assert_eq!(
            sniff_compound_file(&cfb),
            Some(CompoundFileKind::EncryptedPackage)
        );
        let cf = CompoundFile::parse(&cfb).expect("parse");
        assert_eq!(cf.stream("EncryptionInfo").unwrap().unwrap(), small);
        assert_eq!(cf.stream("encryptedpackage").unwrap().unwrap(), large);
        assert!(cf.stream("WordDocument").is_none());
    }

    #[test]
    fn a_legacy_compound_file_is_not_an_encrypted_package() {
        let cfb = test_writer::build(&[("WordDocument", b"binary .doc"), ("1Table", b"t")]);
        assert_eq!(sniff_compound_file(&cfb), Some(CompoundFileKind::Other));
        assert_eq!(sniff_compound_file(b"PK\x03\x04 a zip"), None);
        assert_eq!(sniff_compound_file(b""), None);
    }

    /// The Apache POI password fixtures (Office-written compound files,
    /// local corpus only — skipped when absent): both are encrypted
    /// packages whose `EncryptionInfo` reads back with a version header.
    #[test]
    fn office_written_encrypted_packages_parse() {
        for name in [
            "bug53475-password-is-pass.docx",
            "bug53475-password-is-solrcell.docx",
        ] {
            let path = format!("/data/corpus/files/apache-poi-test-data-document/{name}");
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            assert_eq!(
                sniff_compound_file(&bytes),
                Some(CompoundFileKind::EncryptedPackage),
                "{name}"
            );
            let cf = CompoundFile::parse(&bytes).expect(name);
            let info = cf.stream("EncryptionInfo").unwrap().expect(name);
            let (major, minor) = (
                u16::from_le_bytes([info[0], info[1]]),
                u16::from_le_bytes([info[2], info[3]]),
            );
            assert!(
                matches!((major, minor), (4, 4) | (3 | 4, 2)),
                "{name}: {major}.{minor}"
            );
            let pkg = cf.stream("EncryptedPackage").unwrap().expect(name);
            let declared = u64::from_le_bytes(pkg[..8].try_into().unwrap());
            assert!(declared > 0 && declared <= pkg.len() as u64, "{name}");
        }
    }

    /// Hostile shapes end in an error (or `Other`), never a panic, a hang
    /// or an allocation from a declared size.
    #[test]
    fn hostile_headers_and_chains_are_refused() {
        /* A bare signature (truncated header). */
        assert_eq!(
            sniff_compound_file(&CFB_SIGNATURE),
            Some(CompoundFileKind::Other)
        );
        let good =
            test_writer::build(&[("EncryptionInfo", b"x"), ("EncryptedPackage", &[7u8; 5000])]);
        /* A FAT whose every entry points at itself: chains loop. */
        let mut looping = good.clone();
        let fat_sector = u32::from_le_bytes(looping[0x4C..0x50].try_into().unwrap()) as usize;
        let at = (fat_sector + 1) * 512;
        for k in 0..128u32 {
            looping[at + 4 * k as usize..at + 4 * k as usize + 4].copy_from_slice(&k.to_le_bytes());
        }
        assert!(CompoundFile::parse(&looping).is_err());
        /* A FAT count larger than the file. */
        let mut huge = good.clone();
        huge[0x2C..0x30].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(CompoundFile::parse(&huge).is_err());
        /* A stream whose declared size dwarfs the file: refused without
        allocating it. */
        let mut lying = good.clone();
        let dir_sector = u32::from_le_bytes(lying[0x30..0x34].try_into().unwrap()) as usize;
        let e2 = (dir_sector + 1) * 512 + 2 * DIR_ENTRY_LEN;
        lying[e2 + 0x78..e2 + 0x80].copy_from_slice(&0xFFFF_FFF0u64.to_le_bytes());
        let cf = CompoundFile::parse(&lying).expect("directory still reads");
        assert!(cf.stream("EncryptedPackage").unwrap().is_err());
        /* Every truncation of a valid file stays panic-free. */
        for cut in (0..good.len()).step_by(97) {
            let _ = sniff_compound_file(&good[..cut]);
            if let Ok(cf) = CompoundFile::parse(&good[..cut]) {
                let _ = cf.stream("EncryptedPackage");
                let _ = cf.stream("EncryptionInfo");
            }
        }
    }
}
