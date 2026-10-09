//! Issues #423 / #420 — the per-slot font read-back on
//! `Event::SelectionChanged`: `resolved_font_latin` / `resolved_font_cs`
//! (the family each script slot resolves to after the full cascade —
//! direct name → theme binding → style chain → docDefaults → the layout's
//! default face), `font_source` (which of those answered), `slot_formats`
//! (size / weight / slant / family id per slot, the Font dialog's seed)
//! and `caret_font_slot` (the slot `attrs_at_caret` reports).

use super::*;
use bridge::{BridgeFontSources, FontSlot, FontSource};
use format_docx::test_fixtures::{
    CS_SIZE_MIXED_TEXT, THEME_FIXTURE_TEXTS, complex_script_size_docx, theme_word_default_docx,
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

/// What a `SelectionChanged` reports about fonts.
#[derive(Debug, Clone)]
struct FontReadBack {
    latin: String,
    cs: String,
    sources: BridgeFontSources,
    formats: bridge::BridgeSlotFormats,
    caret_slot: FontSlot,
    attrs: TextAttrs,
}

fn read_back(evt: Event) -> FontReadBack {
    let Event::SelectionChanged {
        resolved_font_latin,
        resolved_font_cs,
        font_source,
        slot_formats,
        caret_font_slot,
        attrs_at_caret,
        ..
    } = evt
    else {
        panic!("expected SelectionChanged, got {evt:?}");
    };
    FontReadBack {
        latin: resolved_font_latin,
        cs: resolved_font_cs,
        sources: font_source,
        formats: slot_formats,
        caret_slot: caret_font_slot,
        attrs: attrs_at_caret,
    }
}

/// Collapse the caret at `(para, offset)` and read the reply.
fn caret_at(e: &mut Engine, para: u32, offset: usize) -> FontReadBack {
    let at = bpos_top(para, offset as u32);
    read_back(apply(
        e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: at.clone(),
                end: at.clone(),
            },
            caret: at,
        },
    ))
}

fn theme_engine() -> Engine {
    let doc = format_docx::read_docx(&theme_word_default_docx())
        .expect("theme fixture")
        .document;
    tests::test_engine_with_doc(doc)
}

fn sources(latin: FontSource, complex_script: FontSource) -> BridgeFontSources {
    BridgeFontSources {
        latin,
        complex_script,
    }
}

/// The acceptance case of #423: Word's default template binds body text to
/// the theme through docDefaults, so the run names no font at all — the
/// read-back is the theme's face (Calibri; Arial for Arabic through the
/// theme's `Arab` row), marked `Theme`, where `attrs_at_caret.font_family`
/// can only report the layout default id.
#[test]
fn theme_bound_body_text_reads_back_its_theme_fonts() {
    let mut e = theme_engine();
    let body = caret_at(&mut e, 1, 3);
    assert_eq!(
        (body.latin.as_str(), body.cs.as_str()),
        ("Calibri", "Arial")
    );
    assert_eq!(body.sources, sources(FontSource::Theme, FontSource::Theme));
    assert_eq!(body.formats.latin.font_family, "calibri");
    assert_eq!(body.formats.complex_script.font_family, "arial");
    assert_eq!(body.caret_slot, FontSlot::Latin);
    assert_eq!(
        body.attrs.font_family, "test-latin",
        "the flat read-back keeps its old meaning (layout default id)"
    );
    /* docDefaults: 11 pt on both slots. */
    assert_eq!(body.formats.latin.font_size, 11.0);
    assert_eq!(body.formats.complex_script.font_size, 11.0);

    /* The heading rebinds every slot to the major font; bold from the
    style, on both twins. */
    let heading = caret_at(&mut e, 0, 3);
    assert_eq!(
        (heading.latin.as_str(), heading.cs.as_str()),
        ("Calibri Light", "Times New Roman")
    );
    assert_eq!(
        heading.sources,
        sources(FontSource::Theme, FontSource::Theme)
    );
    assert!(heading.formats.latin.bold && heading.formats.complex_script.bold);
    assert_eq!(heading.formats.latin.font_size, 16.0);
}

/// A run naming its family explicitly reads `Explicit` on both slots
/// (the fixture's "explicit Amiri" run names `w:ascii` / `w:hAnsi` /
/// `w:cs`).
#[test]
fn an_explicit_run_family_reads_explicit() {
    let mut e = theme_engine();
    let at = THEME_FIXTURE_TEXTS[1].find("explicit Amiri").unwrap() + 4;
    let r = caret_at(&mut e, 1, at);
    assert_eq!((r.latin.as_str(), r.cs.as_str()), ("Amiri", "Amiri"));
    assert_eq!(
        r.sources,
        sources(FontSource::Explicit, FontSource::Explicit)
    );
    assert_eq!(r.formats.latin.font_family, "amiri");
}

/// #423 acceptance, second half: a caret in Arabic text reports the
/// complex-script slot as the active one, and the run that rebinds only
/// its `cs` slot (`w:cstheme="majorBidi"`) reads Times New Roman there
/// while its Latin slot stays on the body theme font.
#[test]
fn a_caret_in_arabic_reads_the_complex_script_slot() {
    let mut e = theme_engine();
    let arabic = THEME_FIXTURE_TEXTS[2];
    let plain = caret_at(&mut e, 2, arabic.find("عربي").unwrap() + 2);
    assert_eq!(plain.caret_slot, FontSlot::ComplexScript);
    assert_eq!(plain.cs, "Arial");
    assert_eq!(plain.sources.complex_script, FontSource::Theme);

    let rebound = caret_at(&mut e, 2, arabic.find("العناوين").unwrap() + 4);
    assert_eq!(rebound.caret_slot, FontSlot::ComplexScript);
    assert_eq!(
        (rebound.latin.as_str(), rebound.cs.as_str()),
        ("Calibri", "Times New Roman")
    );
    assert_eq!(
        rebound.sources,
        sources(FontSource::Theme, FontSource::Theme)
    );
}

/// No theme, no names anywhere: both slots read `Default` and name the
/// face the font stack shapes the script with; a family the docDefaults
/// name reads `Style`; a toolbar pick reads `Explicit` — on the slot it
/// targeted only (issues #420 / #249).
#[test]
fn default_style_and_explicit_sources_follow_the_cascade_level() {
    let mut doc = DocumentTree::from_text("plain words نص");
    let mut e = tests::test_engine_with_doc(doc.clone());
    let r = caret_at(&mut e, 0, 2);
    assert_eq!(r.sources, sources(FontSource::Default, FontSource::Default));
    assert_eq!(r.formats.latin.font_family, "test-latin");
    assert_eq!(r.latin, "Test Latin");
    assert_eq!(r.formats.latin.font_size, 16.0, "the layout default size");

    doc.style_run_defaults = SpanStyle {
        font_family: Some(EngineFontFamily::Amiri),
        ..Default::default()
    };
    let mut e = tests::test_engine_with_doc(doc);
    let r = caret_at(&mut e, 0, 2);
    assert_eq!(r.latin, "Amiri");
    assert_eq!(
        r.sources,
        sources(FontSource::Style, FontSource::Default),
        "docDefaults name only the Latin slot"
    );

    /* A complex-script-only pick over the whole paragraph. */
    let len = e.undo.current().nth_paragraph(0).unwrap().text.len() as u32;
    apply(
        &mut e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: bpos_top(0, 0),
                end: bpos_top(0, len),
            },
            caret: bpos_top(0, len),
        },
    );
    apply(
        &mut e,
        Command::ApplyFormatting {
            range: None,
            attrs: TextAttrsPatch {
                bold: Some(true),
                italic: None,
                underline: None,
                strike: None,
                font_family: Some("noto-naskh".into()),
                font_size: Some(20.0),
                color: None,
                bg_color: None,
                script: None,
                language: None,
                caps: None,
                small_caps: None,
                font_slot: Some(FontSlot::ComplexScript),
            },
        },
    );
    let r = caret_at(&mut e, 0, 2);
    assert_eq!(
        (r.latin.as_str(), r.cs.as_str()),
        ("Amiri", "Noto Naskh Arabic")
    );
    assert_eq!(r.sources, sources(FontSource::Style, FontSource::Explicit));
    assert_eq!(
        (r.formats.latin.font_size, r.formats.latin.bold),
        (16.0, false),
        "the Latin slot is untouched"
    );
    assert_eq!(
        (
            r.formats.complex_script.font_size,
            r.formats.complex_script.bold,
            r.formats.complex_script.font_family.as_str()
        ),
        (20.0, true, "noto-naskh")
    );
}

/// An armed pending (sticky) style is part of what the next keystroke
/// produces, so it reads `Explicit` on the slot it targets — and only
/// there.
#[test]
fn a_pending_style_reads_explicit_on_its_slot() {
    let mut e = theme_engine();
    let _ = caret_at(&mut e, 1, 3);
    apply(
        &mut e,
        Command::ApplyFormatting {
            range: None,
            attrs: TextAttrsPatch {
                bold: None,
                italic: None,
                underline: None,
                strike: None,
                font_family: Some("amiri".into()),
                font_size: None,
                color: None,
                bg_color: None,
                script: None,
                language: None,
                caps: None,
                small_caps: None,
                font_slot: Some(FontSlot::ComplexScript),
            },
        },
    );
    let evt = e.selection_changed();
    let r = read_back(evt);
    assert_eq!((r.latin.as_str(), r.cs.as_str()), ("Calibri", "Amiri"));
    assert_eq!(r.sources, sources(FontSource::Theme, FontSource::Explicit));
}

/// The per-slot sizes are the twins: in the complex-script size fixture's
/// mixed run (`w:sz="22" w:szCs="28"`) both slots report their own size
/// wherever the caret sits, while `attrs_at_caret` follows the caret's
/// script.
#[test]
fn slot_formats_report_both_twins_wherever_the_caret_sits() {
    let doc = format_docx::read_docx(&complex_script_size_docx())
        .expect("fixture")
        .document;
    let mut e = tests::test_engine_with_doc(doc);
    let arabic_at = CS_SIZE_MIXED_TEXT.find('ا').unwrap() + 2;
    for (at, slot, flat) in [
        (3, FontSlot::Latin, 11.0),
        (arabic_at, FontSlot::ComplexScript, 14.0),
    ] {
        let r = caret_at(&mut e, 0, at);
        assert_eq!(r.caret_slot, slot);
        assert_eq!(r.attrs.font_size, flat);
        assert_eq!(r.formats.latin.font_size, 11.0);
        assert_eq!(r.formats.complex_script.font_size, 14.0);
    }
}
