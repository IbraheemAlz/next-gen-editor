//! Issue #355 — theme fonts and colours end to end: a Word default-template
//! document (`format_docx::test_fixtures::theme_fonts_docx`) read through
//! the real reader, laid out by the real paginator, each glyph run shaped
//! against the face its `<w:rFonts>` slot resolves to through the theme.

use super::*;
use format_docx::test_fixtures::{theme_loaded_faces_docx, theme_word_default_docx};

/// The faces the editor ships, under their `FontFamily` ids — the ids a
/// theme typeface resolves to (`"Liberation Sans"` → `liberation`, …).
fn engine_with_shipped_faces(doc: DocumentTree) -> Engine {
    let mut engine = crate::tests::test_engine_with_doc(doc);
    for (id, bytes) in [
        (
            "liberation",
            include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").as_slice(),
        ),
        (
            "amiri",
            include_bytes!("../../../ts/fonts/Amiri-Regular.ttf").as_slice(),
        ),
        (
            "noto-naskh",
            include_bytes!("../../../ts/fonts/NotoNaskhArabic-Regular.ttf").as_slice(),
        ),
    ] {
        let face = LoadedFont::parse(id.to_string(), bytes.to_vec()).expect("parse face");
        engine.fonts.insert(id.to_string(), Arc::new(face));
    }
    engine
}

fn read(bytes: &[u8]) -> DocumentTree {
    format_docx::read_docx(bytes)
        .expect("read fixture")
        .document
}

/// Per body paragraph: `(covered text, font id)` of every glyph run, in
/// visual order, consecutive runs of one face merged.
fn faces_by_paragraph(engine: &Engine) -> (Vec<Vec<(String, String)>>, u64) {
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    assert_eq!(pages.len(), 1);
    let doc = engine.undo.current();
    let mut out = Vec::new();
    for (i, block) in pages[0].blocks.iter().enumerate() {
        let para = block.as_paragraph().expect("paragraph");
        let text = doc.paragraph_text(i as u32).expect("text");
        let mut runs: Vec<(String, String)> = Vec::new();
        for line in &para.lines {
            for run in &line.runs {
                let r = &run.source_range;
                let piece = text[r.start as usize..r.end as usize].to_string();
                match runs.last_mut() {
                    Some((t, f)) if *f == run.font => t.push_str(&piece),
                    _ => runs.push((piece, run.font.clone())),
                }
            }
        }
        out.push(runs);
    }
    (out, layout::geometry_fingerprint(&pages))
}

/// The face every glyph run of `para` covering `needle` was shaped with.
fn face_of<'a>(runs: &'a [(String, String)], needle: &str) -> Vec<&'a str> {
    runs.iter()
        .filter(|(t, _)| t.contains(needle) || needle.contains(t.trim()))
        .filter(|(t, _)| !t.trim().is_empty())
        .map(|(_, f)| f.as_str())
        .collect()
}

/// Theme faces the editor ships: body Latin in Liberation Sans, body
/// Arabic in Noto Naskh Arabic (`+Body CS` through the `Arab` row),
/// headings in Amiri — and a run naming Amiri explicitly keeps Amiri, a
/// run rebinding only its complex-script slot to `majorBidi` switches only
/// its Arabic. Without the theme the same document lays out entirely in
/// the font stack's per-script default (Amiri, the first covering id), as
/// every theme-bound document did before issue #355.
#[test]
fn theme_bound_runs_shape_against_the_theme_faces() {
    let doc = read(&theme_loaded_faces_docx());
    let engine = engine_with_shipped_faces(doc.clone());
    let (paras, fp) = faces_by_paragraph(&engine);
    assert_eq!(face_of(&paras[0], "Theme heading"), ["amiri"]);
    assert_eq!(face_of(&paras[0], "عنوان"), ["amiri"]);
    assert_eq!(
        face_of(&paras[1], "Body text in the minor font,"),
        ["liberation"]
    );
    assert_eq!(face_of(&paras[1], "explicit Amiri"), ["amiri"]);
    assert_eq!(face_of(&paras[1], "then the theme again."), ["liberation"]);
    assert_eq!(face_of(&paras[2], "نص عربي بخط السمة"), ["noto-naskh"]);
    assert_eq!(face_of(&paras[2], "بخط العناوين"), ["amiri"]);
    assert_eq!(face_of(&paras[3], "Accent two"), ["liberation"]);

    let mut themeless = doc;
    themeless.theme = None;
    let (bare, bare_fp) = faces_by_paragraph(&engine_with_shipped_faces(themeless));
    for (i, runs) in bare.iter().enumerate() {
        let faces: Vec<&str> = runs
            .iter()
            .filter(|(t, _)| !t.trim().is_empty())
            .map(|(_, f)| f.as_str())
            .collect();
        let expect: &[&str] = &["amiri"];
        assert!(
            faces.iter().all(|f| expect.contains(f)),
            "paragraph {i} without a theme: {runs:?}"
        );
    }
    eprintln!("THEME LOADED FACES FINGERPRINT theme = {fp:#x} no-theme = {bare_fp:#x}");
    assert_ne!(fp, bare_fp, "the theme moves this fixture's geometry");
    assert_eq!(fp, PINNED_THEME_LOADED_FACES, "themed geometry changed");
    assert_eq!(
        bare_fp, PINNED_THEME_LOADED_FACES_NO_THEME,
        "pre-#355 (theme-less) geometry changed"
    );
}

/// Word's stock theme: the spans name Calibri / Calibri Light for Latin
/// and Arial / Times New Roman for Arabic (what `+Body` / `+Headings` /
/// `+Body CS` / `+Headings CS` are in Word) — none of which the editor
/// ships, so every run still falls to the font stack and the geometry is
/// exactly the theme-less geometry. Substitution (#329) is what makes
/// these names render.
#[test]
fn word_default_theme_names_reach_layout_and_fall_back() {
    let doc = read(&theme_word_default_docx());
    let sctx = StyleContext::of(&doc);
    let ids = |i: u32| -> Vec<(Option<String>, Option<String>)> {
        let p = doc.nth_paragraph(i).expect("paragraph");
        build_style_spans(p, sctx, 11.0, [0, 0, 0, 255], 1.0)
            .into_iter()
            .map(|s| (s.font_family, s.font_family_cs))
            .collect()
    };
    let pair = |l: &str, c: &str| (Some(l.to_string()), Some(c.to_string()));
    assert_eq!(ids(0), [pair("calibri-light", "times-new-roman")]);
    assert_eq!(
        ids(1),
        [
            pair("calibri", "arial"),
            pair("amiri", "amiri"),
            pair("calibri", "arial")
        ]
    );
    assert_eq!(
        ids(2),
        [
            pair("calibri", "arial"),
            pair("calibri", "times-new-roman"),
            pair("calibri", "arial")
        ]
    );
    /* Theme colours: heading accent1 shaded BF, accent2, text1 tinted A6. */
    let colours = |i: u32| -> Vec<[u8; 4]> {
        let p = doc.nth_paragraph(i).expect("paragraph");
        build_style_spans(p, sctx, 11.0, [0, 0, 0, 255], 1.0)
            .into_iter()
            .map(|s| s.color)
            .collect()
    };
    assert_eq!(colours(0), [[0x2F, 0x54, 0x96, 255]]);
    assert_eq!(
        colours(3),
        [
            [0xED, 0x7D, 0x31, 255],
            [0, 0, 0, 255],
            [0x59, 0x59, 0x59, 255],
            [0, 0, 0, 255]
        ]
    );

    let (_, fp) = faces_by_paragraph(&engine_with_shipped_faces(doc.clone()));
    let mut themeless = doc;
    themeless.theme = None;
    let (_, bare_fp) = faces_by_paragraph(&engine_with_shipped_faces(themeless));
    eprintln!("THEME WORD DEFAULT FINGERPRINT = {fp:#x}");
    assert_eq!(
        fp, bare_fp,
        "unshipped theme faces fall back exactly as before"
    );
    assert_eq!(
        fp, PINNED_THEME_WORD_DEFAULT,
        "word-default fixture geometry changed"
    );
}

/// Recorded on this change via `--nocapture` (issue #355).
const PINNED_THEME_LOADED_FACES: u64 = 0x6d2d0b138e4dc1a5;
const PINNED_THEME_LOADED_FACES_NO_THEME: u64 = 0x076c0f0f592c8182;
const PINNED_THEME_WORD_DEFAULT: u64 = 0x076c0f0f592c8182;
