//! Issue #352 — `<w:pBdr>` `<w:start>` / `<w:end>`: the RTL fixture's
//! border strokes land on the physical side the paragraph's direction
//! names. The display list is the golden here: the committed
//! `(side, x, y)` table below is what the canvas and Vello backends paint
//! (both walk this list), so a drift is a visible regression.

/// Red (`FF0000`) vertical border strokes of page 1, top to bottom, as
/// `(x_center, y_top, height)` rounded to whole px.
fn red_vertical_strokes(scene: &render::scene::DisplayList) -> Vec<(i32, i32, i32)> {
    let mut out: Vec<(i32, i32, i32)> = scene
        .cmds
        .iter()
        .filter_map(|c| match c {
            render::scene::DisplayCmd::FillRect { rect, paint }
                if (rect.x1 - rect.x0) < 8.0
                    && (rect.y1 - rect.y0) > (rect.x1 - rect.x0)
                    && paint.solid_rgba8() == Some([255, 0, 0, 255]) =>
            {
                Some((
                    ((rect.x0 + rect.x1) / 2.0).round() as i32,
                    rect.y0.round() as i32,
                    (rect.y1 - rect.y0).round() as i32,
                ))
            }
            _ => None,
        })
        .collect();
    out.sort_by_key(|s| s.1);
    out
}

#[test]
fn rtl_start_border_paints_on_the_right_and_end_on_the_left() {
    let bytes = format_docx::test_fixtures::paragraph_start_end_borders_docx();
    let doc = format_docx::read_docx(&bytes).expect("read").document;
    let engine = crate::tests::test_engine_with_doc(doc);
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    let scene = render::scene::build_document_scene(&pages, 0.0);
    let strokes = red_vertical_strokes(&scene);
    assert_eq!(
        strokes.len(),
        4,
        "one vertical border per paragraph: {strokes:?}"
    );

    /* The page's left content edge and right content edge in px. */
    let left_edge = strokes[0].0;
    let right_edge = strokes[1].0;
    assert!(
        right_edge > left_edge + 300,
        "RTL start is on the far side: {strokes:?}"
    );
    /* LTR start, RTL end and the legacy physical `w:left` all paint left. */
    assert_eq!(strokes[2].0, left_edge, "RTL end → left: {strokes:?}");
    assert_eq!(
        strokes[3].0, left_edge,
        "RTL physical left → left: {strokes:?}"
    );

    /* Golden table (A4, 72 pt margins, 3 pt stroke at scale 1). */
    assert_eq!(
        strokes.iter().map(|s| s.0).collect::<Vec<_>>(),
        GOLDEN_X,
        "stroke x positions: {strokes:?}"
    );
    assert_eq!(
        strokes.iter().map(|s| (s.1, s.2)).collect::<Vec<_>>(),
        GOLDEN_Y,
        "stroke y/height: {strokes:?}"
    );
}

const GOLDEN_X: [i32; 4] = [72, 523, 72, 72];
const GOLDEN_Y: [(i32, i32); 4] = [(72, 26), (98, 26), (124, 26), (150, 26)];
