//! Issue #335 — `<w:softHyphen/>` / `<w:noBreakHyphen/>` end to end: read
//! from `.docx`, laid out (a line that breaks at a soft hyphen draws a
//! synthetic hyphen; a non-breaking hyphen never breaks), caret geometry
//! (the hyphen is not a caret stop) and PDF text extraction (the drawn
//! hyphen maps to `-`, a hidden soft hyphen never claims the space glyph).

use super::*;
use format_docx::test_fixtures::{NB_HYPHEN_TEXT, SOFT_HYPHEN_TEXT, soft_hyphen_docx};
use format_pdf::test_support::{content_streams, decode_codes, text_blocks, to_unicode_cmaps};
use layout::LineHyphen;

fn fixture_engine() -> Engine {
    let archive = format_docx::read_docx(&soft_hyphen_docx()).expect("fixture");
    tests::test_engine_with_doc(archive.document)
}

/// The byte where line `l` ends: the largest run end (the line's logical
/// end in every direction).
fn line_end(l: &LineBox) -> usize {
    l.runs
        .iter()
        .map(|r| r.source_range.end as usize)
        .max()
        .unwrap_or(l.source_start as usize)
}

fn paragraph_lines(pages: &[PageBox], para: u32) -> Vec<&LineBox> {
    pages
        .iter()
        .flat_map(|p| p.blocks.iter())
        .filter_map(|b| b.as_paragraph())
        .filter(|p| p.source_paragraph_id == para)
        .flat_map(|p| p.lines.iter())
        .collect()
}

/// Acceptance (#335): the justified paragraph wraps at author-placed soft
/// hyphens, each such line ending with exactly ONE synthetic hyphen (and
/// stretched to the full measure like any justified line); no other line
/// draws one; no line of the second paragraph ends at a non-breaking
/// hyphen. Geometry pinned.
#[test]
fn soft_hyphens_break_with_a_drawn_hyphen_and_non_breaking_ones_never_break() {
    let engine = fixture_engine();
    let doc = engine.undo.current().clone();
    assert_eq!(doc.nth_paragraph(0).unwrap().text, SOFT_HYPHEN_TEXT);
    assert_eq!(doc.nth_paragraph(1).unwrap().text, NB_HYPHEN_TEXT);
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);

    let lines = paragraph_lines(&pages, 0);
    let soft: Vec<&&LineBox> = lines
        .iter()
        .filter(|l| l.hyphen == LineHyphen::Soft)
        .collect();
    assert!(
        !soft.is_empty(),
        "some line breaks at a soft hyphen: {:?}",
        lines
            .iter()
            .map(|l| &SOFT_HYPHEN_TEXT[l.source_start as usize..line_end(l)])
            .collect::<Vec<_>>()
    );
    for l in &lines {
        eprintln!(
            "{:?} {:?}",
            l.hyphen,
            &SOFT_HYPHEN_TEXT[l.source_start as usize..line_end(l)]
        );
    }
    let full = lines.iter().map(|l| l.width).fold(0.0_f32, f32::max);
    for l in &lines {
        let synth = l
            .runs
            .iter()
            .flat_map(|r| &r.glyphs)
            .filter(|g| g.synthetic)
            .count();
        let at_shy = SOFT_HYPHEN_TEXT[..line_end(l)].ends_with('\u{AD}');
        assert_eq!(l.hyphen == LineHyphen::Soft, at_shy);
        assert_eq!(synth, usize::from(at_shy), "one drawn hyphen per break");
        if at_shy {
            assert!(
                (l.width - full).abs() < 0.5,
                "a hyphenated line is justified"
            );
        }
    }
    for l in paragraph_lines(&pages, 1) {
        assert!(!NB_HYPHEN_TEXT[..line_end(l)].ends_with('\u{2011}'));
        assert!(l.hyphen.is_none());
    }

    let fp = layout::geometry_fingerprint(&pages);
    eprintln!("SOFT HYPHEN FINGERPRINT = {fp:#x}");
    assert_eq!(
        fp, PINNED_SOFT_HYPHEN,
        "soft-hyphen fixture geometry changed"
    );
}

/// Recorded on this change via `--nocapture` (issue #335).
const PINNED_SOFT_HYPHEN: u64 = 0x56ec09b23de0751b;

/// The end-of-line caret of a hyphenated line stops at the last character,
/// BEFORE the drawn hyphen (it is ink, not text); the hyphen itself is no
/// caret stop.
#[test]
fn the_drawn_hyphen_is_not_a_caret_stop() {
    let engine = fixture_engine();
    let (pages, _, _, _) = engine.build_pages(1.0, false, None).expect("layout");
    let line = paragraph_lines(&pages, 0)
        .into_iter()
        .find(|l| l.hyphen == LineHyphen::Soft)
        .expect("a hyphenated line");
    let run = line.runs.last().expect("run");
    let hyphen = run.glyphs.last().expect("glyph");
    assert!(hyphen.synthetic);
    let geom = build_line_run_geom(line, 0.0);
    let last = geom.last().expect("run geometry");
    let end_slot = last.slots.last().expect("end slot");
    let pen: f32 = line
        .runs
        .iter()
        .flat_map(|r| &r.glyphs)
        .map(|g| g.x_advance)
        .sum();
    assert_eq!(end_slot.byte, run.source_range.end);
    assert!(
        (end_slot.x - (pen - hyphen.x_advance)).abs() < 0.01,
        "end caret at {} — expected before the hyphen at {}",
        end_slot.x,
        pen - hyphen.x_advance
    );
    let stops = last.slots.len();
    let real = run.glyphs.iter().filter(|g| !g.synthetic).count() + 1;
    assert_eq!(stops, real, "one stop per real glyph + the run end");
}

/// PDF text extraction (issue #258 discipline): the drawn hyphen decodes
/// to `-` through the subset font's `/ToUnicode`, a hidden soft hyphen
/// never takes over the space glyph's decode, and the extracted text is
/// the paragraph's own words with a hyphen at each soft break.
#[test]
fn pdf_text_extraction_shows_the_break_hyphen_and_real_spaces() {
    let engine = fixture_engine();
    for profile in [format_pdf::PdfProfile::Plain, format_pdf::PdfProfile::A2u] {
        let Event::PdfExported { bytes, .. } = engine.do_export_pdf(profile) else {
            panic!("ExportPdf must succeed for {profile:?}");
        };
        let cmaps = to_unicode_cmaps(&bytes);
        assert_eq!(cmaps.len(), 1, "{profile:?}: one font");
        let cmap = &cmaps[0];
        assert!(
            cmap.values().any(|v| v == "-"),
            "{profile:?}: hyphen mapped"
        );
        assert!(cmap.values().any(|v| v == " "), "{profile:?}: space mapped");
        assert!(
            !cmap.values().any(|v| v.contains('\u{AD}')),
            "{profile:?}: no glyph decodes to a soft hyphen: {cmap:?}"
        );
        let text: String = content_streams(&bytes)
            .iter()
            .flat_map(|s| text_blocks(s))
            .map(|b| decode_codes(&b, cmap))
            .collect();
        assert!(text.contains("Typesetters mark"), "{profile:?}: {text}");
        assert!(text.contains('-'), "{profile:?}: a drawn hyphen: {text}");
    }
}

/// The accessibility mirror reads the TEXT: the paragraph's words with
/// their (invisible) soft hyphens, never the drawn break hyphen.
#[test]
fn the_accessibility_mirror_never_reads_the_drawn_hyphen() {
    let engine = fixture_engine();
    let text: String = engine
        .build_a11y_nodes()
        .iter()
        .filter_map(|n| match n {
            A11yNode::Paragraph(p) => Some(p),
            _ => None,
        })
        .next()
        .expect("first paragraph")
        .runs
        .iter()
        .map(|r| r.text.as_str())
        .collect();
    assert_eq!(text, SOFT_HYPHEN_TEXT);
    assert!(!text.contains('-'), "no drawn hyphen in the mirror");
}
