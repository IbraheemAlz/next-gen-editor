//! Issue #112 — `word/document.xml` drift bucketing.
//!
//! A zero-edit resave is supposed to reproduce `document.xml` byte for
//! byte. When it does not, the *size* delta the pipeline used to report
//! says nothing about which construct the writer failed to preserve — a
//! 3-byte delta could be a dropped BOM or an `<w:sdt>` envelope swapped
//! for an equally long regenerated paragraph. This module finds the FIRST
//! differing byte between the original part and the resaved one and names
//! the element of the ORIGINAL that byte falls in, so `main.rs` can
//! histogram the whole corpus by construct and the fixes land bucket by
//! bucket (largest first) until the drift is zero.
//!
//! The bucket key is `parent/element` (`w:body/w:sdt`, `w:body/w:sectPr`,
//! `/w:document` for the root start tag, `w:body/#text` for whitespace
//! between blocks). Bytes before the root element — BOM, XML declaration,
//! the newline after it — bucket as `<prolog>`; a resave that is a strict
//! prefix of the original buckets at the element the missing tail starts
//! in, and one that is longer than the original as `<eof>`.

use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// Where the first differing byte of a resaved `document.xml` lands in
/// the original part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftPoint {
    /// Byte offset (in the raw original part, BOM included) of the first
    /// byte that differs.
    pub offset: usize,
    /// Qualified name of the innermost element the byte belongs to
    /// (`w:sectPr`), `#text` for character data, `<prolog>` / `<eof>` for
    /// the two out-of-tree cases.
    pub element: String,
    /// `parent/element` bucket key — what `main.rs` histograms.
    pub context: String,
}

/// First index at which `a` and `b` differ; `None` when equal.
pub fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    let common = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
    (common < a.len() || common < b.len()).then_some(common)
}

/// Locate `offset` (raw byte offset into `original`) in the original's
/// element tree.
pub fn locate(original: &[u8], offset: usize) -> DriftPoint {
    if offset >= original.len() {
        return DriftPoint {
            offset,
            element: "<eof>".into(),
            context: "<eof>".into(),
        };
    }
    /* quick-xml drops a leading BOM from its input without counting it in
    `buffer_position()` (issue #110); keep every offset in the raw byte
    space by parsing the stripped stream and shifting by the BOM length. */
    let bom = if original.starts_with(b"\xEF\xBB\xBF") {
        3
    } else {
        0
    };
    if offset < bom {
        return prolog(offset);
    }
    let xml = &original[bom..];
    let target = offset - bom;

    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut prev: usize = 0;
    loop {
        let ev = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            Err(_) => break,
        };
        let pos = reader.buffer_position() as usize;
        let inside = prev <= target && target < pos;
        match ev {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if inside {
                    return point(offset, &stack, name);
                }
                stack.push(name);
            }
            Event::Empty(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                if inside {
                    return point(offset, &stack, name);
                }
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                stack.pop();
                if inside {
                    return point(offset, &stack, name);
                }
            }
            Event::Text(_) | Event::CData(_) => {
                if inside {
                    if stack.is_empty() {
                        return prolog(offset);
                    }
                    return point(offset, &stack, "#text".into());
                }
            }
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) | Event::DocType(_) => {
                if inside {
                    if stack.is_empty() {
                        return prolog(offset);
                    }
                    return point(offset, &stack, "#markup".into());
                }
            }
            Event::Eof => break,
        }
        prev = pos;
        buf.clear();
    }
    DriftPoint {
        offset,
        element: "<eof>".into(),
        context: "<eof>".into(),
    }
}

fn prolog(offset: usize) -> DriftPoint {
    DriftPoint {
        offset,
        element: "<prolog>".into(),
        context: "<prolog>".into(),
    }
}

fn point(offset: usize, stack: &[String], element: String) -> DriftPoint {
    let parent = stack.last().map(String::as_str).unwrap_or("");
    let context = format!("{parent}/{element}");
    DriftPoint {
        offset,
        element,
        context,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="urn:w"><w:body><w:p><w:r><w:t>a</w:t></w:r></w:p><w:sdt><w:sdtPr/><w:sdtContent><w:p/></w:sdtContent></w:sdt> <w:sectPr w:rsidR="1"><w:pgSz/></w:sectPr></w:body></w:document>
"#;

    fn ctx_at(needle: &str) -> String {
        let off = DOC
            .windows(needle.len())
            .position(|w| w == needle.as_bytes())
            .expect("needle");
        locate(DOC, off).context
    }

    #[test]
    fn first_difference_finds_the_first_byte_or_length_change() {
        assert_eq!(first_difference(b"abc", b"abc"), None);
        assert_eq!(first_difference(b"abc", b"abd"), Some(2));
        assert_eq!(first_difference(b"abc", b"ab"), Some(2));
        assert_eq!(first_difference(b"ab", b"abc"), Some(2));
    }

    #[test]
    fn buckets_by_enclosing_element() {
        assert_eq!(ctx_at("\n<w:document"), "<prolog>");
        assert_eq!(locate(DOC, 3).context, "<prolog>");
        assert_eq!(ctx_at("<w:document"), "/w:document");
        assert_eq!(ctx_at("<w:sdt>"), "w:body/w:sdt");
        assert_eq!(ctx_at("<w:sdtPr/>"), "w:sdt/w:sdtPr");
        assert_eq!(ctx_at("<w:p/>"), "w:sdtContent/w:p");
        assert_eq!(ctx_at(" <w:sectPr"), "w:body/#text");
        assert_eq!(ctx_at("<w:sectPr w:rsidR"), "w:body/w:sectPr");
        assert_eq!(ctx_at("w:rsidR"), "w:body/w:sectPr");
        assert_eq!(ctx_at("</w:sectPr>"), "w:body/w:sectPr");
        assert_eq!(ctx_at("</w:document>"), "/w:document");
        assert_eq!(locate(DOC, DOC.len() + 5).context, "<eof>");
    }

    #[test]
    fn bom_offsets_stay_in_the_raw_byte_space() {
        let mut bom = b"\xEF\xBB\xBF".to_vec();
        bom.extend_from_slice(DOC);
        let off = bom
            .windows(7)
            .position(|w| w == b"<w:sdt>")
            .expect("needle");
        assert_eq!(locate(&bom, off).context, "w:body/w:sdt");
        assert_eq!(locate(&bom, 1).context, "<prolog>");
    }
}
