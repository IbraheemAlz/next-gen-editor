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

/// Issue #329 — every face the shell's boot sequence loads (`fonts.json`
/// `defaults` + `substitutes`), under its manifest id: what an opened
/// document lays out against in the editor.
pub(crate) fn engine_with_editor_faces(doc: DocumentTree) -> Engine {
    let mut e = tests::test_engine_with_doc(doc);
    for (id, bytes) in EDITOR_FACES {
        let face = LoadedFont::parse(id.to_string(), bytes.to_vec()).expect("parse face");
        e.fonts.insert(id.to_string(), Arc::new(face));
    }
    e
}

/// `fonts.json`'s boot faces (`defaults` then `substitutes`).
pub(crate) const EDITOR_FACES: &[(&str, &[u8])] = &[
    (
        "amiri",
        include_bytes!("../../../ts/public/fonts/Amiri-Regular.ttf"),
    ),
    (
        "liberation",
        include_bytes!("../../../ts/public/fonts/LiberationSans-Regular.ttf"),
    ),
    (
        "noto-naskh",
        include_bytes!("../../../ts/public/fonts/NotoNaskhArabic-Regular.ttf"),
    ),
    (
        "carlito",
        include_bytes!("../../../ts/public/fonts/Carlito-Regular.ttf"),
    ),
    (
        "caladea",
        include_bytes!("../../../ts/public/fonts/Caladea-Regular.ttf"),
    ),
    (
        "liberation-serif",
        include_bytes!("../../../ts/public/fonts/LiberationSerif-Regular.ttf"),
    ),
    (
        "liberation-mono",
        include_bytes!("../../../ts/public/fonts/LiberationMono-Regular.ttf"),
    ),
    (
        "gelasio",
        include_bytes!("../../../ts/public/fonts/Gelasio-Regular.ttf"),
    ),
    (
        "selawik",
        include_bytes!("../../../ts/public/fonts/Selawik-Regular.ttf"),
    ),
];

/// With the editor's boot faces, Word's stock theme substitutes on both
/// slots: Calibri and Calibri Light take Carlito (Calibri's metric clone;
/// Calibri Light only by family), Arial and Times New Roman Arabic take
/// Noto Naskh Arabic — and layout shapes the body Latin with Carlito.
#[test]
fn the_editor_faces_substitute_the_word_default_theme() {
    let doc = format_docx::read_docx(&theme_word_default_docx())
        .expect("theme fixture")
        .document;
    let e = engine_with_editor_faces(doc);
    let mut want = vec![
        sub("Calibri", FontSlot::Latin, "Carlito", "carlito", true),
        sub(
            "Calibri Light",
            FontSlot::Latin,
            "Carlito",
            "carlito",
            false,
        ),
    ];
    want.extend(arabic_theme_substitutions());
    assert_eq!(e.font_substitutions(), want);

    let (pages, _, _, _) = e.build_pages(1.0, false, None).expect("layout");
    let body = pages[0].blocks[1].as_paragraph().expect("paragraph");
    let text = THEME_FIXTURE_TEXTS[1];
    let calibri_run = body
        .lines
        .iter()
        .flat_map(|l| &l.runs)
        .find(|r| text[r.source_range.start as usize..].starts_with("Body"))
        .expect("the body's first run");
    assert_eq!(calibri_run.font, "carlito");
}

/* ================================================================
Issue #329 — Word's font-derived line pitch for documents read from a
Word package.
================================================================ */

/// Paragraph heights of page 1 at scale 1.
fn paragraph_heights(e: &Engine) -> Vec<f32> {
    let (pages, _, _, info) = e.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    pages[0]
        .blocks
        .iter()
        .map(|b| b.as_paragraph().expect("paragraph").size.height)
        .collect()
}

/// Single / `auto` 480 / `atLeast` 30 pt / `exact` 20 pt / an empty
/// paragraph, in the test face (Liberation Sans at the 16 px layout
/// default): read from a Word package (`document_envelope` captured) the
/// pitch is Word's — 1.149 em per single line (win extent + external
/// leading), doubled, floored at 30, exactly 20, and the empty line sized
/// by its mark's face; an engine-authored document keeps the configured
/// 26 px pitch.
#[test]
fn a_word_document_takes_its_line_pitch_from_the_faces() {
    let para = |text: &str, line_height| {
        engine::Block::Paragraph(engine::Paragraph {
            text: text.into(),
            props: engine::ParaProperties {
                line_height,
                ..Default::default()
            },
            ..Default::default()
        })
    };
    let blocks = vec![
        para("single", None),
        para("double", Some(engine::LineHeight::Auto { twips: 480 })),
        para("at least", Some(engine::LineHeight::AtLeast { twips: 600 })),
        para("exact", Some(engine::LineHeight::Exact { twips: 400 })),
        para("", None),
    ];
    let mut doc = DocumentTree::from_blocks(blocks);
    let configured = paragraph_heights(&tests::test_engine_with_doc(doc.clone()));
    assert_eq!(configured, [26.0, 52.0, 30.0, 20.0, 26.0]);

    doc.document_envelope = engine::DocumentEnvelope {
        root_tag: b"<w:document>".to_vec(),
        body_tag: b"<w:body>".to_vec(),
        tail: b"</w:body></w:document>".to_vec(),
        ..Default::default()
    };
    let word = paragraph_heights(&tests::test_engine_with_doc(doc));
    let single = 16.0 * 2355.0 / 2048.0;
    let want = [single, 2.0 * single, 30.0, 20.0, single];
    for (got, want) in word.iter().zip(want) {
        assert!((got - want).abs() < 1e-3, "{word:?} vs {want}");
    }
}

/// Under Word's pitch an empty paragraph is as tall as its MARK's face at
/// the mark's size (issue #370's rule, here for the font pitch): a 24 pt
/// mark gives a 24 pt line.
#[test]
fn an_empty_word_paragraph_is_sized_by_its_mark() {
    let mut doc = DocumentTree::from_blocks(vec![engine::Block::Paragraph(engine::Paragraph {
        mark_style: Some(Box::new(SpanStyle {
            font_size: Some(24.0),
            ..Default::default()
        })),
        ..Default::default()
    })]);
    doc.document_envelope = engine::DocumentEnvelope {
        root_tag: b"<w:document>".to_vec(),
        body_tag: b"<w:body>".to_vec(),
        tail: b"</w:body></w:document>".to_vec(),
        ..Default::default()
    };
    let h = paragraph_heights(&tests::test_engine_with_doc(doc));
    assert!((h[0] - 24.0 * 2355.0 / 2048.0).abs() < 1e-3, "{h:?}");
}

/// Issue #329 acceptance fixture — a document naming Calibri and
/// Simplified Arabic directly (`format_docx::test_fixtures::
/// substitution_fonts_docx`, committed as `substitution_fonts.docx`), laid
/// out with the editor's boot faces: the Latin text in Carlito, the Arabic
/// in Noto Naskh Arabic, at Word's font-derived pitch — Word 2013's Normal
/// `w:line="259"` multiple of Carlito's 1.221 em at 11 pt for a Latin
/// line, of Noto Naskh's 1.703 em at the 14 pt `w:szCs` for an Arabic
/// one. One page; geometry pinned.
#[test]
fn the_substitution_fixture_lays_out_in_its_substitutes() {
    let doc = format_docx::read_docx(&format_docx::test_fixtures::substitution_fonts_docx())
        .expect("fixture")
        .document;
    let e = engine_with_editor_faces(doc);
    assert_eq!(
        e.font_substitutions(),
        [
            sub("Calibri", FontSlot::Latin, "Carlito", "carlito", true),
            sub(
                "Simplified Arabic",
                FontSlot::ComplexScript,
                "Noto Naskh Arabic",
                "noto-naskh",
                false
            ),
        ]
    );
    let (pages, _, _, info) = e.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    assert_eq!(pages.len(), 1);
    let texts = format_docx::test_fixtures::SUBSTITUTION_FIXTURE_TEXTS;
    let multiple = 259.0 / 240.0;
    let latin_line = 11.0 * 2500.0 / 2048.0 * multiple;
    let arabic_line = 14.0 * 1703.0 / 1000.0 * multiple;
    for (i, block) in pages[0].blocks.iter().enumerate() {
        let para = block.as_paragraph().expect("paragraph");
        for line in &para.lines {
            for run in &line.runs {
                let piece =
                    &texts[i][run.source_range.start as usize..run.source_range.end as usize];
                let arabic = piece
                    .chars()
                    .any(|c| ('\u{0600}'..='\u{06FF}').contains(&c));
                let latin = piece.chars().any(|c| c.is_ascii_alphabetic());
                if arabic {
                    assert_eq!(run.font, "noto-naskh", "{piece:?}");
                } else if latin {
                    assert_eq!(run.font, "carlito", "{piece:?}");
                }
            }
            let has_arabic = line.runs.iter().any(|r| r.font == "noto-naskh");
            if i == 1 {
                assert!((line.height - latin_line).abs() < 1e-3, "{}", line.height);
            } else if i == 2 && has_arabic {
                assert!((line.height - arabic_line).abs() < 1e-2, "{}", line.height);
            }
        }
    }
    let fp = layout::geometry_fingerprint(&pages);
    eprintln!("SUBSTITUTION FIXTURE FINGERPRINT = {fp:#x}");
    assert_eq!(
        fp, PINNED_SUBSTITUTION_FIXTURE,
        "substitution fixture geometry changed"
    );
}

/// Recorded on this change via `--nocapture` (issue #329).
const PINNED_SUBSTITUTION_FIXTURE: u64 = 0x2b50961476972aa6;
