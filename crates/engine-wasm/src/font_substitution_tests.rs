//! Issue #329 — the substitutions the open document's layout makes:
//! reported on `Event::DocumentLoaded` / `Event::FontLoaded`
//! (`substituted`), read back as `FontSource::Substituted` on the slot
//! they serve, and actually shaped with.

use super::*;
use bridge::{FontSlot, FontSource, FontSubstitution};
use format_docx::test_fixtures::{THEME_FIXTURE_TEXTS, theme_word_default_docx};

const NOTO_NASKH: &[u8] = include_bytes!("../../../ts/fonts/NotoNaskhArabic-Regular.ttf");

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

fn sub(
    family: &str,
    slot: FontSlot,
    substitute: &str,
    substitute_id: &str,
    metric_compatible: bool,
) -> FontSubstitution {
    FontSubstitution {
        family: family.into(),
        slot,
        substitute: substitute.into(),
        substitute_id: substitute_id.into(),
        metric_compatible,
    }
}

/// The Arabic rows Word's stock theme needs: `+Body CS` is Arial and
/// `+Headings CS` Times New Roman, neither of which the editor ships.
fn arabic_theme_substitutions() -> Vec<FontSubstitution> {
    vec![
        sub(
            "Arial",
            FontSlot::ComplexScript,
            "Noto Naskh Arabic",
            "noto-naskh",
            false,
        ),
        sub(
            "Times New Roman",
            FontSlot::ComplexScript,
            "Noto Naskh Arabic",
            "noto-naskh",
            false,
        ),
    ]
}

/// The theme fixture names Calibri / Calibri Light for Latin text and
/// Arial / Times New Roman for Arabic. With only Liberation Sans loaded
/// nothing substitutes (no Carlito; no face with Arabic glyphs); loading
/// Noto Naskh Arabic starts serving both Arabic families, and the
/// `FontLoaded` reply says so.
#[test]
fn loading_a_substitute_reports_the_substitutions_it_starts_serving() {
    let doc = format_docx::read_docx(&theme_word_default_docx())
        .expect("theme fixture")
        .document;
    let mut e = tests::test_engine_with_doc(doc);
    assert_eq!(e.font_substitutions(), []);
    let evt = apply(
        &mut e,
        Command::LoadFont {
            id: "noto-naskh".into(),
            bytes: NOTO_NASKH.to_vec(),
        },
    );
    let Event::FontLoaded { substituted, .. } = evt else {
        panic!("expected FontLoaded, got {evt:?}");
    };
    assert_eq!(substituted, arabic_theme_substitutions());
}

/// Opening the document with the substitute already loaded reports the
/// same list on `DocumentLoaded` (the wire shape is pinned in `bridge`).
#[test]
fn opening_a_document_reports_its_substitutions() {
    let mut e = tests::test_engine_with_doc(DocumentTree::from_text(""));
    let evt = apply(
        &mut e,
        Command::LoadFont {
            id: "noto-naskh".into(),
            bytes: NOTO_NASKH.to_vec(),
        },
    );
    let Event::FontLoaded { substituted, .. } = &evt else {
        panic!("expected FontLoaded, got {evt:?}");
    };
    assert!(
        substituted.is_empty(),
        "an empty document substitutes nothing"
    );

    let evt = apply(
        &mut e,
        Command::OpenDocument {
            bytes: theme_word_default_docx(),
            format: bridge::DocFormat::Docx,
            name: None,
            defaults: None,
            limits: None,
            password: None,
        },
    );
    let Event::DocumentLoaded { substituted, .. } = &evt else {
        panic!("expected DocumentLoaded, got {evt:?}");
    };
    assert_eq!(*substituted, arabic_theme_substitutions());
}

/// The read-back marks a slot whose named family is served by a
/// substitute `Substituted` (the resolved name stays the document's);
/// a slot whose family has no loaded substitute keeps its cascade level.
#[test]
fn a_substituted_slot_reads_back_substituted() {
    let doc = format_docx::read_docx(&theme_word_default_docx())
        .expect("theme fixture")
        .document;
    let mut e = tests::test_engine_with_doc(doc);
    let face = LoadedFont::parse("noto-naskh".into(), NOTO_NASKH.to_vec()).expect("parse");
    e.fonts.insert("noto-naskh".into(), Arc::new(face));
    let arabic = THEME_FIXTURE_TEXTS[2];
    let at = bpos_top(2, (arabic.find("عربي").unwrap() + 2) as u32);
    let evt = apply(
        &mut e,
        Command::SetSelection {
            range: BridgeLogicalRange {
                start: at.clone(),
                end: at.clone(),
            },
            caret: at,
        },
    );
    let Event::SelectionChanged {
        resolved_font_latin,
        resolved_font_cs,
        font_source,
        ..
    } = evt
    else {
        panic!("expected SelectionChanged, got {evt:?}");
    };
    assert_eq!(
        (resolved_font_latin.as_str(), resolved_font_cs.as_str()),
        ("Calibri", "Arial")
    );
    assert_eq!(font_source.latin, FontSource::Theme, "no Carlito loaded");
    assert_eq!(font_source.complex_script, FontSource::Substituted);
}

/// A Latin row: a document naming Arial lays its Latin text out in
/// Arial's metric clone (Liberation Sans — the test engine's only face,
/// registered as `test-latin` and matched by its `name`-table family),
/// reported as a metric-compatible Latin substitution and read back as
/// `Substituted`. Its Arabic text has no face with Arabic glyphs to
/// substitute with, so the complex-script slot keeps `Style`.
#[test]
fn a_latin_family_substitutes_its_metric_clone() {
    let mut doc = DocumentTree::from_text("plain words");
    doc.style_run_defaults = SpanStyle {
        font_family: EngineFontFamily::from_display_name("Arial"),
        font_family_cs: EngineFontFamily::from_display_name("Arial"),
        ..Default::default()
    };
    let e = tests::test_engine_with_doc(doc);
    assert_eq!(
        e.font_substitutions(),
        [sub(
            "Arial",
            FontSlot::Latin,
            "Liberation Sans",
            "test-latin",
            true
        )]
    );
    let evt = e.selection_changed();
    let Event::SelectionChanged { font_source, .. } = evt else {
        panic!("expected SelectionChanged, got {evt:?}");
    };
    assert_eq!(font_source.latin, FontSource::Substituted);
    assert_eq!(font_source.complex_script, FontSource::Style);
}
