//! Issue #360 — the tagged export's compact file structure: every plain
//! (non-stream) object packed into one compressed **object stream** (ISO
//! 32000-1 §7.5.7) and the cross-reference table replaced by a compressed
//! **cross-reference stream** (§7.5.8). Both are PDF 1.5+ and both are
//! allowed by PDF/A-2 and PDF/UA-1 (PDF/A-1 and PDF/X-3 — PDF 1.4 —
//! forbid them; the caller never packs a 1.4 file).
//!
//! Tagging is mostly small, repetitive dictionaries — one structure element
//! per paragraph, list item, table cell, link and figure — plus a fixed,
//! uncompressible XMP block (PDF/A keeps the metadata stream unfiltered).
//! Written as plain indirect objects with a classic table, tagging costs
//! 15–28 % on the tier-a corpus' one-page documents; packed, the whole
//! tagged file stays within its size budget of the untagged PDF/A-2u
//! export (`tools/pdf-validate --profile ua1`). Streams (content, fonts,
//! images, the ICC profile, the XMP) are copied byte for byte.
//!
//! This is a post-pass over `pdf_writer::Pdf::finish` output, whose layout
//! it relies on: header, `N 0 obj … endobj` objects, a classic `xref`
//! table with one 20-byte entry per object number, `trailer`, `startxref`.

use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::collections::HashMap;
use std::io::Write;

/// One classic cross-reference entry.
#[derive(Clone, Copy)]
enum Entry {
    /// In use at this byte offset.
    Used(usize),
    /// Free: next free object number, generation.
    Free(u64, u64),
}

/// Rewrite `pdf` (complete pdf-writer output) so every in-use object that
/// is not a stream lives in one compressed object stream, with a
/// compressed cross-reference stream in place of the table. Object numbers
/// are kept; the object stream and the cross-reference stream take the
/// next two numbers. The trailer's `/Root`, `/Info` and `/ID` move into
/// the cross-reference stream's dictionary verbatim.
pub(crate) fn pack(pdf: &[u8]) -> Result<Vec<u8>, String> {
    let startxref = rfind(pdf, b"\nstartxref\n").ok_or("pack: no startxref")?;
    let xref_at: usize = ascii_number(&pdf[startxref + 11..]).ok_or("pack: bad startxref")?;
    let table = pdf
        .get(xref_at..startxref)
        .ok_or("pack: startxref out of range")?;
    let rest = table
        .strip_prefix(b"xref\n0 ")
        .ok_or("pack: no classic xref")?;
    let size: usize = ascii_number(rest).ok_or("pack: bad xref size")?;
    let first_entry = rest
        .iter()
        .position(|&b| b == b'\n')
        .ok_or("pack: bad xref header")?
        + 1;
    let entries_bytes = &rest[first_entry..];
    if entries_bytes.len() < size * 20 {
        return Err("pack: truncated xref".into());
    }
    let mut entries: Vec<Entry> = Vec::with_capacity(size);
    for i in 0..size {
        let e = &entries_bytes[i * 20..i * 20 + 20];
        let a: u64 = ascii_number(&e[0..10]).ok_or("pack: bad xref entry")?;
        let b: u64 = ascii_number(&e[11..16]).ok_or("pack: bad xref entry")?;
        entries.push(match e[17] {
            b'n' => Entry::Used(a as usize),
            _ => Entry::Free(a, b),
        });
    }
    let trailer = &entries_bytes[size * 20..];
    let dict_open = find(trailer, b"<<").ok_or("pack: no trailer dict")? + 2;
    let dict_close = rfind(trailer, b">>").ok_or("pack: no trailer dict end")?;
    let trailer_entries: Vec<&[u8]> = trailer[dict_open..dict_close]
        .split(|&b| b == b'\n')
        .map(trim)
        .filter(|l| !l.is_empty() && !l.starts_with(b"/Size "))
        .collect();

    /* Every in-use object, in file order; each extends to the next one
    (pdf-writer writes them back to back) or to the table. */
    let mut used: Vec<(usize, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(n, e)| match *e {
            Entry::Used(off) => Some((off, n)),
            Entry::Free(..) => None,
        })
        .collect();
    used.sort_unstable();
    if used.iter().any(|&(off, _)| off >= xref_at) {
        return Err("pack: object offset past the table".into());
    }
    let header_end = used.first().map_or(xref_at, |&(o, _)| o);

    let mut out: Vec<u8> = Vec::with_capacity(pdf.len());
    out.extend_from_slice(&pdf[..header_end]);
    let mut offsets: HashMap<usize, usize> = HashMap::new();
    /* Packed objects: number → slot in the object stream. */
    let mut slots: HashMap<usize, usize> = HashMap::new();
    let mut index = String::new();
    let mut data: Vec<u8> = Vec::new();
    for (i, &(start, n)) in used.iter().enumerate() {
        let end = used.get(i + 1).map_or(xref_at, |&(o, _)| o);
        let obj = &pdf[start..end];
        let head = format!("{n} 0 obj\n");
        let body = obj
            .strip_prefix(head.as_bytes())
            .ok_or_else(|| format!("pack: object {n} header"))?;
        let body_end = rfind(body, b"\nendobj").ok_or_else(|| format!("pack: object {n} end"))?;
        let body = &body[..body_end];
        if find(body, b"\nstream\n").is_some() {
            offsets.insert(n, out.len());
            out.extend_from_slice(obj);
            continue;
        }
        slots.insert(n, slots.len());
        if !index.is_empty() {
            index.push(' ');
        }
        index.push_str(&format!("{n} {}", data.len()));
        data.extend_from_slice(body);
        data.push(b'\n');
    }

    /* The object stream. */
    let objstm = size;
    let xref = size + 1;
    let new_size = size + 2;
    index.push('\n');
    let mut raw = index.clone().into_bytes();
    raw.extend_from_slice(&data);
    let z = deflate(&raw);
    offsets.insert(objstm, out.len());
    out.extend_from_slice(
        format!(
            "{objstm} 0 obj\n<<\n  /Type /ObjStm\n  /N {}\n  /First {}\n  /Filter /FlateDecode\n  /Length {}\n>>\nstream\n",
            slots.len(),
            index.len(),
            z.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&z);
    out.extend_from_slice(b"\nendstream\nendobj\n\n");

    /* The cross-reference stream: type 0 free / 1 at an offset / 2 in the
    object stream. */
    let xref_offset = out.len();
    offsets.insert(xref, xref_offset);
    let rows: Vec<(u8, u64, u64)> = (0..new_size)
        .map(|n| {
            if let Some(&slot) = slots.get(&n) {
                (2, objstm as u64, slot as u64)
            } else if let Some(&off) = offsets.get(&n) {
                (1, off as u64, 0)
            } else {
                match entries.get(n) {
                    Some(Entry::Free(next, generation)) => (0, *next, *generation),
                    _ => (0, 0, 0),
                }
            }
        })
        .collect();
    let w2 = bytes_for(rows.iter().map(|r| r.1).max().unwrap_or(0));
    let w3 = bytes_for(rows.iter().map(|r| r.2).max().unwrap_or(0));
    let mut stream: Vec<u8> = Vec::with_capacity(rows.len() * (1 + w2 + w3));
    for (t, a, b) in rows {
        stream.push(t);
        stream.extend_from_slice(&a.to_be_bytes()[8 - w2..]);
        stream.extend_from_slice(&b.to_be_bytes()[8 - w3..]);
    }
    let z = deflate(&stream);
    let mut dict =
        format!("{xref} 0 obj\n<<\n  /Type /XRef\n  /Size {new_size}\n  /W [1 {w2} {w3}]\n")
            .into_bytes();
    for line in trailer_entries {
        dict.extend_from_slice(b"  ");
        dict.extend_from_slice(line);
        dict.push(b'\n');
    }
    dict.extend_from_slice(
        format!(
            "  /Filter /FlateDecode\n  /Length {}\n>>\nstream\n",
            z.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&dict);
    out.extend_from_slice(&z);
    out.extend_from_slice(b"\nendstream\nendobj\n\n");
    out.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF").as_bytes());
    Ok(out)
}

/// Bytes needed to store `v` big-endian (at least 1).
fn bytes_for(v: u64) -> usize {
    ((64 - v.leading_zeros()) as usize).div_ceil(8).max(1)
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data)
        .expect("zlib encode into a Vec cannot fail");
    enc.finish().expect("zlib finish into a Vec cannot fail")
}

/// The leading ASCII decimal number of `b`.
fn ascii_number<T: std::str::FromStr>(b: &[u8]) -> Option<T> {
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    std::str::from_utf8(&b[..digits]).ok()?.parse().ok()
}

fn trim(b: &[u8]) -> &[u8] {
    let start = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    let end = b
        .iter()
        .rposition(|c| !c.is_ascii_whitespace())
        .map_or(start, |e| e + 1);
    &b[start..end]
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdf_writer::{Name, Pdf, Ref, Str};
    use std::io::Read;

    /// Catalog (1), a plain dictionary (2), a free number (3), a stream (4).
    fn sample() -> Vec<u8> {
        let mut pdf = Pdf::new();
        pdf.catalog(Ref::new(1)).pair(Name(b"Extra"), Ref::new(2));
        pdf.indirect(Ref::new(2))
            .dict()
            .pair(Name(b"Note"), Str(b"packed (me)"));
        pdf.stream(Ref::new(4), b"BT ET");
        pdf.set_file_id((b"abc".to_vec(), b"def".to_vec()));
        pdf.finish()
    }

    /// The inflated body of the first stream after `marker`.
    fn stream_after(out: &[u8], marker: &[u8]) -> Vec<u8> {
        let at = find(out, marker).unwrap();
        let s = at + find(&out[at..], b"stream\n").unwrap() + 7;
        let e = s + find(&out[s..], b"\nendstream").unwrap();
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(&out[s..e])
            .read_to_end(&mut raw)
            .unwrap();
        raw
    }

    #[test]
    fn packs_dictionaries_into_an_object_stream_with_an_xref_stream() {
        let out = pack(&sample()).expect("pack");
        let text = String::from_utf8_lossy(&out);
        assert!(!text.contains("\nxref\n"), "the classic table is gone");
        assert!(!text.contains("\ntrailer\n"));
        assert!(text.contains("/Type /ObjStm\n  /N 2\n"), "catalog + dict");
        assert!(text.contains("/Type /XRef"));
        assert!(text.contains("/Root 1 0 R"));
        assert!(text.contains("/ID [(abc) (def)]"));
        assert!(
            !text.contains("2 0 obj"),
            "object 2 lives in the object stream"
        );
        assert!(!text.contains("1 0 obj"), "so does the catalog");
        assert!(text.contains("4 0 obj\n"), "streams stay plain objects");
        assert!(out.ends_with(b"%%EOF"));
        /* startxref points at the cross-reference stream object (byte
        offsets: `text` is lossy over the compressed streams). */
        let sx = rfind(&out, b"startxref\n").unwrap();
        let at: usize = ascii_number(&out[sx + 10..]).unwrap();
        assert!(out[at..].starts_with(b"6 0 obj\n"));
    }

    #[test]
    fn the_xref_stream_locates_every_object() {
        let out = pack(&sample()).expect("pack");
        let at = find(&out, b"/Type /XRef").unwrap();
        let w_at = at + find(&out[at..], b"/W [").unwrap() + 4;
        let w_end = w_at + find(&out[w_at..], b"]").unwrap();
        let w: Vec<usize> = std::str::from_utf8(&out[w_at..w_end])
            .unwrap()
            .split(' ')
            .map(|v| v.parse().unwrap())
            .collect();
        let rows = stream_after(&out, b"/Type /XRef");
        let width: usize = w.iter().sum();
        assert_eq!(rows.len(), 7 * width, "objects 0..=6");
        let field = |row: &[u8], from: usize, n: usize| {
            row[from..from + n]
                .iter()
                .fold(0usize, |acc, &b| (acc << 8) | b as usize)
        };
        for (n, row) in rows.chunks(width).enumerate() {
            let (t, a, b) = (row[0], field(row, 1, w[1]), field(row, 1 + w[1], w[2]));
            match n {
                0 | 3 => assert_eq!(t, 0, "object {n} is free"),
                1 | 2 => assert_eq!((t, a, b), (2, 5, n - 1), "object {n} packed"),
                _ => {
                    assert_eq!(t, 1, "object {n} in use");
                    assert!(
                        out[a..].starts_with(format!("{n} 0 obj\n").as_bytes()),
                        "object {n}"
                    );
                }
            }
        }
        /* The object stream's index names both packed objects, and each
        offset (relative to `/First`) lands on its dictionary. */
        let raw = String::from_utf8(stream_after(&out, b"/Type /ObjStm")).unwrap();
        let (index, objects) = raw.split_once('\n').unwrap();
        let nums: Vec<usize> = index.split(' ').map(|v| v.parse().unwrap()).collect();
        assert_eq!((nums[0], nums[2]), (1, 2));
        assert!(objects[nums[1]..].starts_with("<<\n  /Type /Catalog"));
        assert!(objects[nums[3]..].starts_with("<<\n  /Note (packed (me))"));
    }

    #[test]
    fn refuses_input_that_is_not_pdf_writer_output() {
        assert!(pack(b"%PDF-1.7\n").is_err());
        let mut broken = sample();
        let x = rfind(&broken, b"xref\n0 ").unwrap();
        broken.truncate(x + 30);
        broken.extend_from_slice(b"\nstartxref\n0\n%%EOF");
        assert!(pack(&broken).is_err());
    }
}
