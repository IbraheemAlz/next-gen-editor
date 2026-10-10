//! Issue #395 — paragraph borders defined on STYLES reach the canvas: the
//! corpus' Word `Title` style paints its accent-1 bottom rule, a
//! `basedOn` chain cascades per edge, a direct `nil` removes one edge and
//! a style's `<w:start>` lands on the side the PARAGRAPH's direction
//! names (`format_docx::test_fixtures::styled_paragraph_borders_docx`).
//! The display list is the golden (both backends walk it), like
//! `pbdr_start_end_tests`; the layout geometry is pinned too — borders
//! paint, they never move a box.

/// Border colours of the fixture: the Title rule, red, blue.
const COLOURS: [([u8; 4], char); 3] = [
    ([0x4F, 0x81, 0xBD, 255], 'A'),
    ([255, 0, 0, 255], 'R'),
    ([0, 0, 255, 255], 'B'),
];

/// Every border stroke of page 1, top to bottom then left to right:
/// `(colour tag, 'H'orizontal / 'V'ertical, x0, y0, length)` in whole px.
fn border_strokes(scene: &render::scene::DisplayList) -> Vec<(char, char, i32, i32, i32)> {
    let mut out: Vec<(char, char, i32, i32, i32)> = scene
        .cmds
        .iter()
        .filter_map(|c| match c {
            render::scene::DisplayCmd::FillRect { rect, paint } => {
                let rgba = paint.solid_rgba8()?;
                let tag = COLOURS.iter().find(|(c, _)| *c == rgba)?.1;
                let (w, h) = (rect.x1 - rect.x0, rect.y1 - rect.y0);
                let (dir, len) = if w >= h { ('H', w) } else { ('V', h) };
                Some((
                    tag,
                    dir,
                    rect.x0.round() as i32,
                    rect.y0.round() as i32,
                    len.round() as i32,
                ))
            }
            _ => None,
        })
        .collect();
    out.sort_by_key(|s| (s.3, s.2, s.0, s.1));
    out
}

#[test]
fn style_borders_paint_per_edge_by_the_paragraph_direction() {
    let bytes = format_docx::test_fixtures::styled_paragraph_borders_docx();
    let doc = format_docx::read_docx(&bytes).expect("read").document;
    let engine = crate::tests::test_engine_with_doc(doc);
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    let scene = render::scene::build_document_scene(&pages, 0.0);
    let strokes = border_strokes(&scene);
    assert_eq!(strokes, GOLDEN, "border strokes: {strokes:#?}");
    assert_eq!(
        format!("{:#018x}", layout::geometry_fingerprint(&pages)),
        GOLDEN_FINGERPRINT,
        "style borders paint, they never move a box"
    );
}

/// Eyeballed against the fixture's intent (A4, 72 pt margins → content
/// x 72..523; 3 pt red / blue strokes centred on the edge, the Title's
/// 1 pt accent rule): Title — bottom only; boxed — top, left, bottom
/// (`basedOn` per edge); no top — left, bottom (direct nil); LTR start —
/// blue left; RTL start (direct bidi) — blue right; RTL style start —
/// blue right; LTR override (direct `bidi="0"`) — blue left. Issue #329 —
/// line heights are Word's font-derived pitch (18 px for the 16 px test
/// face; the configured 26 px before), which also moves the Title rule.
const GOLDEN: [(char, char, i32, i32, i32); 10] = [
    ('A', 'H', 72, 101, 451),
    ('R', 'H', 72, 115, 451),
    ('R', 'V', 70, 117, 18),
    ('R', 'H', 72, 133, 451),
    ('R', 'V', 70, 135, 18),
    ('R', 'H', 72, 152, 451),
    ('B', 'V', 70, 154, 18),
    ('B', 'V', 521, 172, 18),
    ('B', 'V', 521, 190, 18),
    ('B', 'V', 70, 209, 18),
];

/// Issue #329 — was `0x1b3b06bbca34b0ac` under the configured pitch.
const GOLDEN_FINGERPRINT: &str = "0x4da969f4646ef429";
