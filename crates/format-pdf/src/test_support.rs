//! Issue #258 — the PDF content-stream / string-literal decoder issue #210
//! wrote inside `engine-wasm`'s `toc_pdf_export_tests.rs` to verify the TOC
//! → layout → `format_pdf::export_pdf` pipeline byte-for-byte. Moved here so
//! a second PDF-byte-level test (in either this crate or a downstream one)
//! never has to duplicate it. Public (hidden) only so downstream crates'
//! tests (`engine-wasm`'s TOC / tier-a corpus fixture tests) share it;
//! nothing in the export path calls these, so the linker drops them from
//! the wasm artifact — the same contract as [`crate::test_images`].
//!
//! `flate2` is already a normal (non-dev) dependency of this crate (the
//! export path itself deflates `/FlateDecode` streams), so no Cargo.toml
//! change was needed to share it across the workspace this way.

use std::collections::BTreeMap;
use std::io::Read;

/// Every top-level content-stream object embedded in `pdf`, inflated, in
/// file order. A page's content stream, an embedded `FontFile2` program and
/// a font's `/ToUnicode` CMap are ALL `/FlateDecode` streams
/// (`crates/format-pdf/src/lib.rs`); a content stream is told apart by
/// containing a `BT` (begin-text) operator once inflated — a font program
/// is opaque binary and a CMap's `beginbfchar`/`endcidrange` text never
/// contains that token.
pub fn content_streams(pdf: &[u8]) -> Vec<Vec<u8>> {
    inflated_streams(pdf)
        .into_iter()
        .filter(|decoded| find(decoded, b"BT").is_some() && find(decoded, b"begincmap").is_none())
        .collect()
}

/// Issue #360 — `pdf` as (lossy) text for marker assertions, with the
/// objects of every object stream inflated and appended: a tagged PDF 1.5+
/// export packs its dictionaries (catalog, pages, structure tree, …) into
/// one (`crate::objstm`), so a plain byte search no longer sees them.
pub fn searchable_text(pdf: &[u8]) -> String {
    let mut out = String::from_utf8_lossy(pdf).into_owned();
    let mut cursor = 0usize;
    while let Some(rel) = find(&pdf[cursor..], b"/Type /ObjStm") {
        let at = cursor + rel;
        let Some(s) = find(&pdf[at..], b"stream\n").map(|p| at + p + 7) else {
            break;
        };
        let Some(e) = find(&pdf[s..], b"\nendstream").map(|p| s + p) else {
            break;
        };
        let mut decoded = Vec::new();
        if flate2::read::ZlibDecoder::new(&pdf[s..e])
            .read_to_end(&mut decoded)
            .is_ok()
        {
            out.push('\n');
            out.push_str(&String::from_utf8_lossy(&decoded));
        }
        cursor = e;
    }
    out
}

/// Every `/FlateDecode` stream in `pdf` that inflates cleanly, in file order.
fn inflated_streams(pdf: &[u8]) -> Vec<Vec<u8>> {
    const MARKER: &[u8] = b">>\nstream\n";
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = find(&pdf[cursor..], MARKER) {
        let start = cursor + rel + MARKER.len();
        let Some(end_rel) = find(&pdf[start..], b"endstream") else {
            break;
        };
        let raw = &pdf[start..start + end_rel];
        cursor = start + end_rel + b"endstream".len();
        let mut decoded = Vec::new();
        if flate2::read::ZlibDecoder::new(raw)
            .read_to_end(&mut decoded)
            .is_ok()
        {
            out.push(decoded);
        }
    }
    out
}

/// Issue #327 — every `/ToUnicode` CMap embedded in `pdf` (one per font, in
/// file order), parsed into code → text. Subset fonts renumber glyph ids,
/// so a test can no longer compare a content stream's codes against the
/// shaper's glyph ids; it recovers the TEXT the codes stand for instead —
/// exactly what a viewer's copy / text extraction does.
pub fn to_unicode_cmaps(pdf: &[u8]) -> Vec<BTreeMap<u16, String>> {
    inflated_streams(pdf)
        .into_iter()
        .filter(|decoded| find(decoded, b"begincmap").is_some())
        .map(|decoded| parse_bfchar(&decoded))
        .collect()
}

/// The `<code> <utf16-hex>` pairs of every `beginbfchar` … `endbfchar`
/// section (the only form `pdf_writer::types::UnicodeCmap` writes).
fn parse_bfchar(cmap: &[u8]) -> BTreeMap<u16, String> {
    let text = String::from_utf8_lossy(cmap);
    let mut out = BTreeMap::new();
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.ends_with("beginbfchar") {
            in_section = true;
        } else if line == "endbfchar" {
            in_section = false;
        } else if in_section {
            let mut hex = line.split(['<', '>', ' ']).filter(|part| !part.is_empty());
            let (Some(code), Some(utf16)) = (hex.next(), hex.next()) else {
                continue;
            };
            let Ok(code) = u16::from_str_radix(code, 16) else {
                continue;
            };
            let units: Vec<u16> = (0..utf16.len() / 4)
                .filter_map(|i| u16::from_str_radix(&utf16[i * 4..i * 4 + 4], 16).ok())
                .collect();
            out.insert(code, String::from_utf16_lossy(&units));
        }
    }
    out
}

/// Issue #327 — the distinct faces `pdf` embeds, by `/BaseFont` name with
/// any six-letter `ABCDEF+` subset tag stripped (the `Type0` and `CIDFont`
/// dictionaries both carry the name — deduplicated), in file order.
pub fn embedded_font_names(pdf: &[u8]) -> Vec<String> {
    const KEY: &[u8] = b"/BaseFont /";
    let mut out: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = find(&pdf[cursor..], KEY) {
        let start = cursor + rel + KEY.len();
        let len = pdf[start..]
            .iter()
            .position(|b| b.is_ascii_whitespace() || b"/<>[]()%".contains(b))
            .unwrap_or(pdf.len() - start);
        cursor = start + len;
        let name = String::from_utf8_lossy(&pdf[start..start + len]).into_owned();
        let bytes = name.as_bytes();
        let untagged =
            if bytes.len() > 7 && bytes[6] == b'+' && bytes[..6].iter().all(u8::is_ascii_uppercase)
            {
                name[7..].to_string()
            } else {
                name
            };
        if !out.contains(&untagged) {
            out.push(untagged);
        }
    }
    out
}

/// Decode one text block's codes through a font's `/ToUnicode` map; an
/// unmapped code becomes U+FFFD so a missing mapping fails loudly.
pub fn decode_codes(codes: &[u16], cmap: &BTreeMap<u16, String>) -> String {
    codes
        .iter()
        .map(|code| cmap.get(code).map_or("\u{fffd}", String::as_str))
        .collect()
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Parse one inflated content stream into its `BT`..`ET` text objects, each
/// as the sequence of 2-byte (Identity-H CID) glyph ids its `Tj` operators
/// show. `show_run` / `emit_tab_leader_glyphs`
/// (`crates/format-pdf/src/lib.rs`) show exactly one glyph per `Tj`, and a
/// leader-glyph pass opens its OWN nested `BT`/`ET` outside the paragraph's
/// main text object — so an entry paragraph with a leader tab contributes
/// exactly two adjacent blocks in the returned list.
/// Decodes `pdf_writer::object::Str`'s own encoding (`Primitive for Str`):
/// a literal `(...)` with PDF's backslash / octal escapes, or a hex
/// `<...>` when any byte is non-ASCII.
pub fn text_blocks(stream: &[u8]) -> Vec<Vec<u16>> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<u16>> = None;
    let mut i = 0usize;
    while i < stream.len() {
        let rest = &stream[i..];
        if rest.starts_with(b"BT") {
            current = Some(Vec::new());
            i += 2;
        } else if rest.starts_with(b"ET") {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            i += 2;
        } else if stream[i] == b'(' {
            let (raw, next) = decode_literal_string(stream, i + 1);
            if let Some(cur) = current.as_mut() {
                push_codes(cur, &raw);
            }
            i = next;
        } else if stream[i] == b'<' {
            let (raw, next) = decode_hex_string(stream, i + 1);
            if let Some(cur) = current.as_mut() {
                push_codes(cur, &raw);
            }
            i = next;
        } else {
            i += 1;
        }
    }
    blocks
}

fn push_codes(out: &mut Vec<u16>, raw: &[u8]) {
    for pair in raw.chunks_exact(2) {
        out.push(u16::from_be_bytes([pair[0], pair[1]]));
    }
}

/// `s[start..]` begins right after the opening `(`. Returns the decoded
/// bytes and the index right after the closing `)`.
fn decode_literal_string(s: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut i = start;
    while i < s.len() {
        match s[i] {
            b')' => {
                i += 1;
                break;
            }
            b'\\' => {
                i += 1;
                match s.get(i) {
                    Some(b'n') => {
                        out.push(b'\n');
                        i += 1;
                    }
                    Some(b'r') => {
                        out.push(b'\r');
                        i += 1;
                    }
                    Some(b't') => {
                        out.push(b'\t');
                        i += 1;
                    }
                    Some(b'\x08') | Some(b'b') => {
                        out.push(0x08);
                        i += 1;
                    }
                    Some(b'f') => {
                        out.push(0x0c);
                        i += 1;
                    }
                    Some(b'(') => {
                        out.push(b'(');
                        i += 1;
                    }
                    Some(b')') => {
                        out.push(b')');
                        i += 1;
                    }
                    Some(b'\\') => {
                        out.push(b'\\');
                        i += 1;
                    }
                    Some(d) if (b'0'..=b'7').contains(d) => {
                        let mut val: u32 = 0;
                        let mut n = 0;
                        while n < 3 && s.get(i).is_some_and(|c| (b'0'..=b'7').contains(c)) {
                            val = val * 8 + u32::from(s[i] - b'0');
                            i += 1;
                            n += 1;
                        }
                        out.push(val as u8);
                    }
                    _ => {}
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    (out, i)
}

/// `s[start..]` begins right after the opening `<`. Returns the decoded
/// bytes and the index right after the closing `>`.
fn decode_hex_string(s: &[u8], start: usize) -> (Vec<u8>, usize) {
    let mut hex = Vec::new();
    let mut i = start;
    while i < s.len() && s[i] != b'>' {
        if s[i].is_ascii_hexdigit() {
            hex.push(s[i]);
        }
        i += 1;
    }
    if i < s.len() {
        i += 1;
    }
    let out = hex
        .chunks(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16).unwrap_or(0) as u8;
            let lo = pair
                .get(1)
                .and_then(|&b| (b as char).to_digit(16))
                .unwrap_or(0) as u8;
            (hi << 4) | lo
        })
        .collect();
    (out, i)
}

/// Issue #258 — a format-pdf-side consumer of this module, proving it is
/// genuinely SHARED (not just relocated for `engine-wasm`'s sole benefit):
/// export a one-paragraph page and recover its glyph ids straight from the
/// PDF bytes via [`content_streams`] / [`text_blocks`], the same technique
/// `engine-wasm`'s TOC fixture test now imports from here.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::PdfProfile;
    use layout::{
        HeaderRole, LayoutBlock, Margins, NoteBand, PageBox, ParagraphConfig, Size, StyleSpan,
        layout_paragraph,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use text_pipeline::{Alignment, FontStack, LoadedFont, ShapingDirection};

    fn liberation_stack() -> FontStack {
        let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
        let face = LoadedFont::parse("liberation".into(), bytes).expect("parse font");
        let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
        faces.insert("liberation".to_string(), Arc::new(face));
        FontStack::from_faces(faces, "liberation")
    }

    #[test]
    fn content_streams_and_text_blocks_decode_a_real_export() {
        let stack = liberation_stack();
        let mut para = layout_paragraph(ParagraphConfig {
            text: "Hi",
            fonts: &stack,
            spans: &[StyleSpan {
                start: 0,
                end: 2,
                px_size: 24.0,
                color: [0, 0, 0, 255],
                bold: false,
                italic: false,
                underline: engine::UnderlineStyle::None,
                strike: false,
                bg_color: None,
                font_family: None,
                caps_transform: false,
                baseline_shift_px: 0.0,
            }],
            base_direction: ShapingDirection::Ltr,
            max_width: 400.0,
            line_height: 30.0,
            line_height_exact: false,
            alignment: Alignment::Start,
            indent_start_px: 0.0,
            indent_end_px: 0.0,
            first_line_indent_px: 0.0,
            hanging_indent_px: 0.0,
            marker_text: None,
            px_size_for_marker: 12.0,
            inline_objects: &[],
            tab_stops_px: &[],
        });
        para.source_paragraph_id = 0;
        let page = PageBox {
            size: Size {
                width: 400.0,
                height: 200.0,
            },
            margins: Margins::uniform(36.0),
            blocks: vec![LayoutBlock::Paragraph(para)],
            header: None,
            footer: None,
            header_offset: 18.0,
            footer_offset: 18.0,
            footnotes: NoteBand::default(),
            endnotes: NoteBand::default(),
            hf_role: HeaderRole::Default,
            page_number: 1,
            floats: Vec::new(),
        };
        let mut out = Vec::new();
        crate::export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &["Hi"],
            PdfProfile::Plain,
            &mut out,
        )
        .expect("export");

        let streams = content_streams(&out);
        assert!(!streams.is_empty(), "no content stream decoded");
        let blocks: Vec<Vec<u16>> = streams.iter().flat_map(|s| text_blocks(s)).collect();
        assert!(!blocks.is_empty(), "no BT/ET text block decoded");
        /* Issue #327 — the font is a subset, so the codes are its
        renumbered ids (first use: 'H' → 1, 'i' → 2), and they decode
        back to the source text through the font's `/ToUnicode`. */
        assert!(
            blocks.contains(&vec![1, 2]),
            "expected the subset codes [1, 2] among decoded blocks {blocks:?}"
        );
        let cmaps = to_unicode_cmaps(&out);
        assert_eq!(cmaps.len(), 1, "one font, one /ToUnicode CMap");
        let texts: Vec<String> = blocks.iter().map(|b| decode_codes(b, &cmaps[0])).collect();
        assert!(texts.iter().any(|t| t == "Hi"), "decoded blocks {texts:?}");
    }
}
