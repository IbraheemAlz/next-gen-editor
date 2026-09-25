//! Issue #210 — nothing exercised the REAL path `DocumentTree::regenerate_tocs`
//! (#81) → layout → `format_pdf::export_pdf` end to end. #144's acceptance
//! test simulated a TOC-entry-shaped paragraph straight into
//! `layout_paragraph`; this module drives the actual bridge commands
//! (`Command::InsertToc`, `Command::UpdateFields`) against a real
//! `DocumentTree`, runs the real pagination, and inspects the BYTES a
//! `SaveAs → PDF` / `Print` would actually produce — not just the box tree.
//!
//! This lives beside `mutation_signal_tests.rs` / `wire_validation_tests`
//! rather than in `crates/format-pdf` because the page-number post-pass
//! (`Engine::regenerate_tocs_converged`, which paginates a probe layout to
//! learn each heading's page before re-synthesizing the TOC's result
//! paragraphs) and the `para_texts` / source-paragraph-id plumbing
//! `Engine::do_export_pdf` builds live only in `engine-wasm` — `format-pdf`
//! has no document-tree-to-`PageBox` pipeline of its own, so a
//! `format-pdf`-only test could not exercise the real page-number
//! resolution the issue is about (it could only hand-feed
//! `regenerate_tocs` a page number it already knows to be correct, which
//! begs the question). `Engine::do_insert_toc` / `do_update_fields` /
//! `do_export_pdf` are the same private methods `Engine::apply` dispatches
//! `Command::InsertToc` / `UpdateFields` / `ExportPdf` to — this test calls
//! them directly (as `mod tests` and `mutation_signal_tests` already do)
//! rather than through the `JsValue` / `serde_wasm_bindgen` `dispatch()`
//! surface, which only functions inside a browser's wasm32 runtime (see the
//! `fuzz-native` block's doc comment, `src/lib.rs`).

use super::*;
use std::io::Read;

/// Shared scaffold: a native `Engine` over `doc` with a real Latin font and
/// a cached layout config — mirrors `mod tests::test_engine_with_doc` /
/// `mutation_signal_tests::engine_with`, duplicated here because both are
/// private to their own module (Rust privacy is per-module, not per-crate,
/// for non-`pub(crate)` items) and this module is a *sibling*, not a
/// descendant, of either.
fn build_engine(doc: DocumentTree) -> Engine {
    let mut e = assemble_engine(None, None);
    let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
    let font = LoadedFont::parse("test-latin".to_string(), bytes).expect("parse test font");
    e.fonts.insert("test-latin".to_string(), Arc::new(font));
    e.layout_cfg = Some(RenderConfig {
        font_id: "test-latin".to_string(),
        base_direction: ShapingDirection::Ltr,
        px_size: 16.0,
        line_height: 26.0,
        alignment: Alignment::Start,
        scale: 1.0,
        base_scale: 1.0,
        zoom: 1.0,
    });
    e.undo = UndoStack::new(doc, 100);
    e.selection = Some(SelectionState {
        anchor: bpos_top(0, 0),
        caret: bpos_top(0, 0),
        ideal_x: None,
        kind: SelectionKind::Linear,
    });
    e
}

fn heading(text: &str, level: u8) -> engine::Block {
    engine::Block::Paragraph(engine::Paragraph {
        text: text.into(),
        style_id: Some(format!("Heading{level}")),
        ..Default::default()
    })
}

fn body_para(text: &str) -> engine::Block {
    engine::Block::Paragraph(engine::Paragraph {
        text: text.into(),
        ..Default::default()
    })
}

/// Heading 1 / Heading 2 / Heading 1 over two pages: "Introduction" (p1)
/// carries a FORM FEED so "Background" and "Conclusion" land on p2 — the
/// established pagination-fixture idiom (`crates/engine/src/toc.rs`'s own
/// `five_heading_doc`, and `mod tests::five_heading_doc` above), not
/// `set_page_break_before` (that property forces a break on the paragraph
/// it is set on, one indirection further from "this exact byte offset
/// starts a new page" than a literal FORM FEED in the text).
fn three_heading_toc_doc() -> DocumentTree {
    let mut d = DocumentTree::from_text("");
    d.blocks = vec![
        heading("Introduction", 1),
        body_para("Intro body.\u{000C}"),
        heading("Background", 2),
        heading("Conclusion", 1),
    ]
    .into_iter()
    .collect();
    d
}

/// `"Heading\tPage"` for every paragraph in the document's one TOC region,
/// in document order — mirrors `mod tests::toc_entry_texts`.
fn entry_texts(engine: &Engine) -> Vec<String> {
    let doc = engine.undo.current();
    let regions = doc.toc_regions();
    assert_eq!(regions.len(), 1, "exactly one TOC");
    (regions[0].first..=regions[0].last)
        .map(|b| {
            doc.paragraph_at_path(&EngineBlockPath::top(b))
                .expect("TOC region block is a paragraph")
                .text
                .clone()
        })
        .collect()
}

/// Every top-level content-stream object embedded in `pdf`, inflated, in
/// file order. A page's content stream, an embedded `FontFile2` program and
/// a font's `/ToUnicode` CMap are ALL `/FlateDecode` streams
/// (`crates/format-pdf/src/lib.rs`); a content stream is told apart by
/// containing a `BT` (begin-text) operator once inflated — a font program
/// is opaque binary and a CMap's `beginbfchar`/`endcidrange` text never
/// contains that token.
fn content_streams(pdf: &[u8]) -> Vec<Vec<u8>> {
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
            && find(&decoded, b"BT").is_some()
        {
            out.push(decoded);
        }
    }
    out
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
/// main text object — so a TOC entry paragraph contributes exactly two
/// adjacent blocks in the returned list: its heading-text-plus-page-number
/// main block, then a leader block whose codes are all the same glyph id.
/// Decodes `pdf_writer::object::Str`'s own encoding (`Primitive for Str`):
/// a literal `(...)` with PDF's backslash / octal escapes, or a hex
/// `<...>` when any byte is non-ASCII.
fn text_blocks(stream: &[u8]) -> Vec<Vec<u16>> {
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

/// Issue #210's acceptance shape: 3 headings (Heading 1/2/1) over two
/// pages, `InsertToc`, `UpdateFields` (F9), export PDF plain AND PDF/A-1b
/// — the real `DocumentTree::regenerate_tocs` (#81) → layout →
/// `format_pdf::export_pdf` pipeline, asserting entry text, page numbers
/// and the dotted-leader glyph counts (#144) reached the actual PDF bytes.
#[test]
fn toc_survives_regenerate_layout_and_pdf_export() {
    let mut engine = build_engine(three_heading_toc_doc());

    /* `Command::InsertToc` — no `\h` (keeps the entry a plain run of text:
    no PAGEREF sub-field / hyperlink span to account for when decoding the
    PDF's glyph shows below). */
    let switches = bridge::TocSwitches {
        outline_min: 1,
        outline_max: 3,
        hyperlinks: false,
        hide_in_web: true,
        use_outline_levels: true,
        page_numbers: true,
    };
    let evt = engine.do_insert_toc(bpos_top(0, 0), switches);
    assert!(!matches!(evt, Event::Error { .. }), "InsertToc: {evt:?}");

    let expected_entries = [
        ("Introduction", "1"),
        ("Background", "2"),
        ("Conclusion", "2"),
    ];
    let want_texts: Vec<String> = expected_entries
        .iter()
        .map(|(h, p)| format!("{h}\t{p}"))
        .collect();
    assert_eq!(entry_texts(&engine), want_texts, "InsertToc result");

    /* `Command::UpdateFields` (F9). A freshly inserted TOC is already
    current, so this is the "nothing to do" path — the acceptance scope
    calls for it regardless, and it must leave a correct TOC correct. */
    let evt = engine.do_update_fields();
    assert!(!matches!(evt, Event::Error { .. }), "UpdateFields: {evt:?}");
    assert_eq!(entry_texts(&engine), want_texts, "UpdateFields must not disturb a current TOC");

    /* Real layout: every entry line carries exactly one dot-leader tab
    glyph, and its tiled dot count is derived from the SAME geometry
    `emit_tab_leader_glyphs` (crates/format-pdf/src/lib.rs) uses — pen
    position + `page.margins` + `para.origin` + `line.origin`
    (`.claude/rules/render.md`'s accumulation invariant), the run's own
    `px_size`, and the face's own period advance. */
    let (pages, font_stack, _paths, info) =
        engine.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    assert!(pages.len() >= 2, "fixture must span at least two pages");

    let face = font_stack.face("test-latin").expect("test-latin face loaded");
    let dot_gid = face.glyph_id('.').expect("liberation shapes '.'");

    let mut expected_dot_counts = Vec::with_capacity(expected_entries.len());
    {
        let page = &pages[0];
        let margin_left = page.margins.left;
        for block in page.blocks.iter().take(expected_entries.len()) {
            let p = block.as_paragraph().expect("TOC entry is a paragraph");
            let line = p.lines.first().expect("entry paragraph has one line");
            let mut pen = 0.0_f32;
            let mut found: Option<(f32, f32, f32)> = None;
            for run in &line.runs {
                for g in &run.glyphs {
                    if g.leader == Some(layout::TabLeaderKind::Dot) {
                        let x0 = margin_left + p.origin.x + line.origin.x + pen;
                        found = Some((x0, x0 + g.x_advance, run.attrs.px_size));
                    }
                    pen += g.x_advance;
                }
            }
            let (x0, x1, px) = found.expect("entry has exactly one dot-leader tab");
            let step = face
                .glyph_metrics('.', px)
                .expect("period metrics")
                .advance_width;
            let pad = px * 0.15;
            let hi = x1 - pad;
            let lo_raw = x0 + pad;
            let count = if hi <= lo_raw {
                0u32
            } else {
                let mut x = (lo_raw / step).ceil() * step;
                if x + step > hi {
                    0
                } else {
                    let mut n = 0u32;
                    while x + step <= hi {
                        n += 1;
                        x += step;
                    }
                    n
                }
            };
            assert!(count > 0, "leader tab must tile at least one dot");
            expected_dot_counts.push(count);
        }
    }

    /* `Command::ExportPdf` — plain output has no `PdfConformance` wire
    variant (issue #210's "plain" leg is reachable only by calling
    `do_export_pdf` directly with `format_pdf::PdfProfile::Plain`, never
    over the wire — see that method's doc comment), so both legs are
    driven directly here. */
    for profile in [format_pdf::PdfProfile::Plain, format_pdf::PdfProfile::A1b] {
        let Event::PdfExported {
            bytes,
            pages: page_count,
        } = engine.do_export_pdf(profile)
        else {
            panic!("ExportPdf must succeed for {profile:?}");
        };
        assert!(bytes.starts_with(b"%PDF"), "{profile:?}");
        assert_eq!(page_count as usize, pages.len(), "{profile:?} page count");

        let streams = content_streams(&bytes);
        assert!(!streams.is_empty(), "{profile:?}: no content stream found");
        let all_blocks: Vec<Vec<u16>> = streams.iter().flat_map(|s| text_blocks(s)).collect();

        /* Pair up (main, leader) blocks: a leader block is a nonempty run
        of the SAME glyph id (the dot) — `emit_tab_leader_glyphs` shows
        nothing else. Its immediate predecessor is the entry's own text. */
        let mut pairs: Vec<(&[u16], &[u16])> = Vec::new();
        for w in all_blocks.windows(2) {
            let leader = &w[1];
            if !leader.is_empty() && leader.iter().all(|&g| g == dot_gid) {
                pairs.push((&w[0], leader));
            }
        }
        assert_eq!(
            pairs.len(),
            expected_entries.len(),
            "{profile:?}: TOC entry (main, leader) block count"
        );

        for (i, (main, leader)) in pairs.iter().enumerate() {
            let (heading, page_num) = expected_entries[i];
            let expected_codes: Vec<u16> = heading
                .chars()
                .chain(page_num.chars())
                .map(|c| {
                    face.glyph_id(c)
                        .unwrap_or_else(|| panic!("no glyph for {c:?}"))
                })
                .collect();
            assert_eq!(
                main.to_vec(),
                expected_codes,
                "{profile:?} entry {i} (\"{heading}\") text + page number glyphs"
            );
            assert_eq!(
                leader.len() as u32,
                expected_dot_counts[i],
                "{profile:?} entry {i} (\"{heading}\") leader dot count"
            );
        }
    }
}

/// Issue #210 (part 2) — one-off regenerator for
/// `tests/corpus/tier-a/toc-leaders.docx`, the `tools/pdf-validate` fixture
/// covering the same TOC-with-leaders shape this module's PDF test drives
/// natively. `tools/pdf-validate` only ever *opens* a `.docx` and exports it
/// (`LOAD_DOCX` then `EXPORT_PDF` — it never dispatches `InsertToc` /
/// `UpdateFields` itself), so the fixture must carry an already-regenerated
/// TOC result (real dot leaders + real page numbers) baked in at save time —
/// exactly what this produces: build the fixture, `InsertToc`,
/// `UpdateFields`, then `SaveDocument` to `.docx` bytes.
///
/// `#[ignore]`d — it writes to the working tree rather than asserting
/// anything, so it must never run under `cargo test --workspace`. Re-run it
/// by hand (`cargo test -p engine-wasm --lib -- --ignored
/// generate_toc_leaders_pdf_validate_fixture`) whenever `regenerate_tocs` /
/// the TOC leader shape changes and the committed fixture needs updating —
/// mirrors `tools/visual-diff`'s `UPDATE=1` goldens and `tools/perf-fixtures`
/// (a whole crate) for the same "committed, regeneratable fixture" idiom.
#[test]
#[ignore = "regenerates tests/corpus/tier-a/toc-leaders.docx; run explicitly, not part of the workspace test gate"]
fn generate_toc_leaders_pdf_validate_fixture() {
    let mut engine = build_engine(three_heading_toc_doc());
    let switches = bridge::TocSwitches {
        outline_min: 1,
        outline_max: 3,
        hyperlinks: false,
        hide_in_web: true,
        use_outline_levels: true,
        page_numbers: true,
    };
    let evt = engine.do_insert_toc(bpos_top(0, 0), switches);
    assert!(!matches!(evt, Event::Error { .. }), "InsertToc: {evt:?}");
    let evt = engine.do_update_fields();
    assert!(!matches!(evt, Event::Error { .. }), "UpdateFields: {evt:?}");

    let Event::DocumentSaved { bytes, .. } = engine.save_docx_bytes("toc-leaders fixture generator")
    else {
        panic!("SaveDocument must succeed");
    };

    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/tier-a/toc-leaders.docx");
    std::fs::write(&out, &bytes).unwrap_or_else(|e| panic!("write {}: {e}", out.display()));
    eprintln!("wrote {} ({} bytes)", out.display(), bytes.len());
}
