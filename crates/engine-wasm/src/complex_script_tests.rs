//! Issues #359 / #104 / #249 — complex-script run properties. OOXML formats
//! Arabic / Hebrew / Thai / … characters (and every character of a
//! `<w:rtl/>` / `<w:cs/>` run) with the complex-script twins of the run
//! properties: `<w:szCs>` instead of `<w:sz>`, `<w:bCs>` / `<w:iCs>`
//! instead of `<w:b>` / `<w:i>`, `<w:rFonts w:cs>` instead of
//! `w:ascii` / `w:hAnsi`. The engine keeps the twins apart from read to
//! layout to write.

use super::*;
use bridge::{FontSlot, FormattingToggle};
use format_docx::test_fixtures::{
    CS_SIZE_CASCADE_TEXT, CS_SIZE_MIXED_TEXT, CS_SIZE_RTL_TEXT, complex_script_size_docx,
    docx_with_body,
};

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = Box::pin(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("Engine::apply suspended in a native test"),
    }
}

fn apply(e: &mut Engine, cmd: Command) -> Event {
    let evt = block_on(e.apply(cmd));
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    evt
}

/// `doc` in an engine with a Latin AND an Arabic face loaded (the Arabic
/// face covers U+0628, so the stack routes Arabic script runs to it).
fn engine_with(doc: DocumentTree) -> Engine {
    let mut e = tests::test_engine_with_doc(doc);
    let bytes = include_bytes!("../../../ts/fonts/NotoNaskhArabic-Regular.ttf").to_vec();
    let font = LoadedFont::parse("test-arabic".to_string(), bytes).expect("arabic font");
    e.fonts.insert("test-arabic".to_string(), Arc::new(font));
    e
}

fn fixture_engine() -> Engine {
    let archive = format_docx::read_docx(&complex_script_size_docx()).expect("fixture");
    engine_with(archive.document)
}

fn select(e: &mut Engine, para: u32, start: u32, end: u32) {
    apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: bpos_top(para, start),
                end: bpos_top(para, end),
            },
            caret: bpos_top(para, end),
        },
    );
}

fn patch(font_size: f32, font_slot: Option<FontSlot>) -> TextAttrsPatch {
    TextAttrsPatch {
        bold: None,
        italic: None,
        underline: None,
        strike: None,
        font_family: None,
        font_size: Some(font_size),
        color: None,
        bg_color: None,
        script: None,
        language: None,
        caps: None,
        small_caps: None,
        font_slot,
    }
}

/// Every shaped run of top-level paragraph `para` (all pages), as
/// (source text, px size).
fn runs_of(pages: &[PageBox], doc: &DocumentTree, para: u32) -> Vec<(String, f32)> {
    let text = doc.nth_paragraph(para).expect("paragraph").text.clone();
    pages
        .iter()
        .flat_map(|p| p.blocks.iter())
        .filter_map(|b| b.as_paragraph())
        .filter(|p| p.source_paragraph_id == para)
        .flat_map(|p| p.lines.iter())
        .flat_map(|l| l.runs.iter())
        .map(|r| {
            let s = r.source_range.start as usize;
            let e = r.source_range.end as usize;
            (text[s..e].to_string(), r.attrs.px_size)
        })
        .collect()
}

fn is_arabic(s: &str) -> bool {
    s.chars().any(text_pipeline::is_complex_script)
}

/// Acceptance (#359): one run with `w:sz="22" w:szCs="28"` lays its Latin
/// words out at 11 pt and its Arabic words at 14 pt; a `<w:rtl/>` run puts
/// EVERY character (digits, the Latin word) at its 14 pt `szCs`; a run with
/// only `w:sz` takes the docDefaults `szCs` (16 pt) for its Arabic, never
/// the Latin 11 pt. Geometry pinned.
#[test]
fn mixed_run_lays_latin_and_arabic_out_at_their_own_sizes() {
    let engine = fixture_engine();
    let doc = engine.undo.current().clone();
    assert_eq!(doc.nth_paragraph(0).unwrap().text, CS_SIZE_MIXED_TEXT);
    assert_eq!(doc.nth_paragraph(1).unwrap().text, CS_SIZE_RTL_TEXT);
    assert_eq!(doc.nth_paragraph(2).unwrap().text, CS_SIZE_CASCADE_TEXT);
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);

    let mixed = runs_of(&pages, &doc, 0);
    assert!(mixed.iter().any(|(t, _)| is_arabic(t)));
    assert!(mixed.iter().any(|(t, _)| !is_arabic(t)));
    for (t, px) in &mixed {
        let want = if is_arabic(t) { 14.0 } else { 11.0 };
        assert_eq!(*px, want, "{t:?}");
    }
    let lines_mixed = pages[0].blocks[0].as_paragraph().unwrap().lines.len();
    assert!(lines_mixed >= 2, "the mixed paragraph wraps");

    for (t, px) in runs_of(&pages, &doc, 1) {
        assert_eq!(px, 14.0, "<w:rtl/> run: {t:?} takes w:szCs");
    }

    for (t, px) in runs_of(&pages, &doc, 2) {
        let want = if is_arabic(&t) { 16.0 } else { 11.0 };
        assert_eq!(px, want, "{t:?}");
    }

    let fp = layout::geometry_fingerprint(&pages);
    eprintln!("COMPLEX SCRIPT SIZE FINGERPRINT = {fp:#x}");
    assert_eq!(
        fp, PINNED_CS_SIZE,
        "complex-script size fixture geometry changed"
    );
}

/// Recorded on this change via `--nocapture` (issue #359).
const PINNED_CS_SIZE: u64 = 0x77225ebeb9c6b3ba;

/// The old single slot let `w:szCs` win: the same paragraph with BOTH
/// sizes at 14 pt needs more room — the twin split is what moves the
/// line breaks.
#[test]
fn folding_szcs_into_sz_would_lay_the_latin_out_larger() {
    let engine = fixture_engine();
    let (split, _, _, _) = engine.build_pages(1.0, false, None).expect("layout");
    let mut folded = engine.undo.current().clone();
    if let Some(engine::Block::Paragraph(p)) = folded.blocks.get_mut(0) {
        for run in &mut p.spans {
            run.style.font_size = run.style.font_size_cs;
        }
    }
    let folded_engine = engine_with(folded);
    let (folded, _, _, _) = folded_engine.build_pages(1.0, false, None).expect("layout");
    let width = |pages: &[PageBox]| {
        pages[0].blocks[0]
            .as_paragraph()
            .unwrap()
            .lines
            .iter()
            .map(|l| l.width)
            .sum::<f32>()
    };
    assert!(
        width(&split) < width(&folded) - 10.0,
        "11 pt Latin is narrower than 14 pt Latin ({} vs {})",
        width(&split),
        width(&folded)
    );
}

/// `ApplyFormatting { font_size }` sets both slots by default (Word's
/// ribbon); `font_slot: Latin` / `ComplexScript` (the `cs_only` flag) set
/// one and leave the other alone — and the relayout sees a twin-only
/// change (the paragraph layout key hashes the twins).
#[test]
fn apply_formatting_routes_the_size_by_font_slot() {
    let mut e = fixture_engine();
    let len = CS_SIZE_MIXED_TEXT.len() as u32;
    let style = |e: &Engine| e.undo.current().nth_paragraph(0).unwrap().style_at(0);

    select(&mut e, 0, 0, len);
    apply(
        &mut e,
        Command::ApplyFormatting {
            range: None,
            attrs: patch(20.0, Some(FontSlot::ComplexScript)),
        },
    );
    assert_eq!(style(&e).font_size, Some(11.0), "Latin slot untouched");
    assert_eq!(style(&e).font_size_cs, Some(20.0));
    let doc = e.undo.current().clone();
    let (pages, _, _, _) = e.build_pages(1.0, false, None).expect("layout");
    for (t, px) in runs_of(&pages, &doc, 0) {
        let want = if is_arabic(&t) { 20.0 } else { 11.0 };
        assert_eq!(px, want, "cs-only relayout: {t:?}");
    }

    apply(
        &mut e,
        Command::ApplyFormatting {
            range: None,
            attrs: patch(9.0, Some(FontSlot::Latin)),
        },
    );
    assert_eq!(style(&e).font_size, Some(9.0));
    assert_eq!(style(&e).font_size_cs, Some(20.0), "complex slot untouched");

    apply(
        &mut e,
        Command::ApplyFormatting {
            range: None,
            attrs: patch(12.0, None),
        },
    );
    assert_eq!(style(&e).font_size, Some(12.0));
    assert_eq!(style(&e).font_size_cs, Some(12.0), "no slot = both");
}

/// The toolbar read-back reports the size the caret's text is laid out
/// with: 14 pt inside the Arabic words of the mixed run, 11 pt inside
/// its Latin words, 14 pt anywhere in the `<w:rtl/>` run.
#[test]
fn attrs_at_caret_report_the_complex_script_size_in_arabic_text() {
    let e = fixture_engine();
    let arabic_at = CS_SIZE_MIXED_TEXT.find('ا').unwrap() as u32 + 2;
    let latin_at = 3;
    assert_eq!(e.attrs_at(bpos_top(0, arabic_at), true).font_size, 14.0);
    assert_eq!(e.attrs_at(bpos_top(0, latin_at), true).font_size, 11.0);
    let digits_at = CS_SIZE_RTL_TEXT.find("2026").unwrap() as u32 + 2;
    assert_eq!(
        e.attrs_at(bpos_top(1, digits_at), true).font_size,
        14.0,
        "a <w:rtl/> run reports its complex-script size everywhere"
    );
    let cascade_arabic = CS_SIZE_CASCADE_TEXT.find('ا').unwrap() as u32 + 2;
    assert_eq!(
        e.attrs_at(bpos_top(2, cascade_arabic), true).font_size,
        16.0,
        "an unset szCs reads the cascade's"
    );
    /* A range reads its first character. */
    assert_eq!(
        e.attrs_at(bpos_top(0, arabic_at - 2), false).font_size,
        14.0
    );
}

/// The UI save path keeps the slots apart: a size the user set on part of
/// the run comes back on BOTH slots, the rest keeps its source pair (the
/// byte-level checks live in `tools/roundtrip`'s complex-script step).
#[test]
fn ui_save_keeps_each_slot() {
    let mut e = fixture_engine();
    select(&mut e, 0, 0, 5);
    apply(
        &mut e,
        Command::ApplyFormatting {
            range: None,
            attrs: patch(18.0, None),
        },
    );
    let saved = format_docx::save_docx(e.undo.current()).expect("save");
    let back = format_docx::read_docx(&saved).expect("reread").document;
    let p = back.nth_paragraph(0).unwrap();
    assert_eq!(p.style_at(0).font_size, Some(18.0));
    assert_eq!(p.style_at(0).font_size_cs, Some(18.0));
    assert_eq!(p.style_at(10).font_size, Some(11.0));
    assert_eq!(p.style_at(10).font_size_cs, Some(14.0));
    let cascade = back.nth_paragraph(2).unwrap().style_at(0);
    assert_eq!(cascade.font_size, Some(11.0));
    assert_eq!(cascade.font_size_cs, None, "no szCs synthesized");
}

/* ================================================================
Issue #104 — `<w:bCs>` / `<w:iCs>`.
================================================================ */

/// "Latin " then two Arabic words, in ONE run.
const BCS_TEXT: &str = "Latin \u{0646}\u{0635} \u{0639}\u{0631}\u{0628}\u{064A}";

/// A one-paragraph document whose single run carries `rpr`.
fn engine_for_run(rpr: &str) -> Engine {
    let body = format!(
        r#"<w:p><w:r><w:rPr>{rpr}</w:rPr><w:t xml:space="preserve">{BCS_TEXT}</w:t></w:r></w:p>"#
    );
    let archive = format_docx::read_docx(&docx_with_body(&body)).expect("fixture");
    engine_with(archive.document)
}

fn toggle_bold(e: &mut Engine) {
    apply(
        e,
        Command::ToggleFormatting {
            attr: FormattingToggle::Bold,
            underline_style: None,
        },
    );
}

/// Every shaped run of paragraph 0 as (is complex script, faux bold,
/// faux italic). No bold / italic face is loaded, so weight and slant
/// show up as synthesis flags.
fn faces(e: &Engine) -> Vec<(bool, bool, bool)> {
    let doc = e.undo.current().clone();
    let text = doc.nth_paragraph(0).unwrap().text.clone();
    let (pages, _, _, _) = e.build_pages(1.0, false, None).expect("layout");
    pages[0].blocks[0]
        .as_paragraph()
        .unwrap()
        .lines
        .iter()
        .flat_map(|l| l.runs.iter())
        .map(|r| {
            let piece = &text[r.source_range.start as usize..r.source_range.end as usize];
            (is_arabic(piece), r.attrs.faux_bold, r.attrs.faux_italic)
        })
        .collect()
}

/// Word bolds Arabic text by `<w:bCs>` and Latin text by `<w:b>`: a run
/// with only `<w:bCs/>` (rtl.docx's shape) shows bold Arabic and regular
/// Latin; one with only `<w:b/>` the opposite. Same for `<w:iCs>`.
#[test]
fn bcs_and_ics_style_only_the_complex_script_text() {
    for (rpr, latin, arabic) in [
        ("<w:bCs/>", (false, false), (true, false)),
        ("<w:b/>", (true, false), (false, false)),
        ("<w:b/><w:bCs/><w:iCs/>", (true, false), (true, true)),
        ("<w:i/>", (false, true), (false, false)),
    ] {
        let runs = faces(&engine_for_run(rpr));
        assert!(
            runs.iter().any(|r| r.0) && runs.iter().any(|r| !r.0),
            "{rpr}"
        );
        for (cs, b, i) in runs {
            let want = if cs { arabic } else { latin };
            assert_eq!((b, i), want, "{rpr}: complex={cs}");
        }
    }
}

/// Issue #104 — Ctrl+B on Arabic text reads the weight Word shows there
/// (`bCs`) and writes BOTH slots: un-bolding a `<w:bCs/>` Arabic run turns
/// `bCs` off (it used to go stale in the grab bag and stay bold in Word).
#[test]
fn toggle_bold_on_arabic_text_reads_and_writes_both_twins() {
    let mut e = engine_for_run("<w:bCs/>");
    let arabic_start = BCS_TEXT.find('\u{0646}').unwrap() as u32;
    let len = BCS_TEXT.len() as u32;
    assert!(
        e.attrs_at(bpos_top(0, arabic_start + 2), true).bold,
        "Arabic reads bCs"
    );
    assert!(!e.attrs_at(bpos_top(0, 2), true).bold, "Latin reads b");

    select(&mut e, 0, arabic_start, len);
    toggle_bold(&mut e);
    let s = e
        .undo
        .current()
        .nth_paragraph(0)
        .unwrap()
        .style_at(arabic_start);
    assert_eq!(
        (s.bold, s.bold_cs),
        (Some(false), Some(false)),
        "bold Arabic turns off"
    );
    assert!(faces(&e).iter().all(|f| !f.1), "nothing bold any more");

    toggle_bold(&mut e);
    let s = e
        .undo
        .current()
        .nth_paragraph(0)
        .unwrap()
        .style_at(arabic_start);
    assert_eq!((s.bold, s.bold_cs), (Some(true), Some(true)));

    /* Saved: Word sees both. */
    let saved = format_docx::save_docx(e.undo.current()).expect("save");
    let back = format_docx::read_docx(&saved).expect("reread").document;
    let s = back.nth_paragraph(0).unwrap().style_at(arabic_start);
    assert_eq!((s.bold, s.bold_cs), (Some(true), Some(true)));
}

/// A range whose Latin part is regular and whose Arabic part is bold by
/// `bCs` is MIXED for bold (the toolbar shows the indeterminate state and
/// Ctrl+B turns everything on, Word's rule).
#[test]
fn mixed_detection_reads_each_script_class_with_its_twin() {
    let mut e = engine_for_run("<w:bCs/>");
    let len = BCS_TEXT.len() as u32;
    let mixed = e.attrs_mixed_over(&bpos_top(0, 0), &bpos_top(0, len));
    assert!(mixed.bold, "regular Latin + bCs Arabic is mixed");
    assert!(!mixed.italic);
    select(&mut e, 0, 0, len);
    toggle_bold(&mut e);
    assert!(
        faces(&e).iter().all(|f| f.1),
        "mixed turns ON for both classes"
    );
}
