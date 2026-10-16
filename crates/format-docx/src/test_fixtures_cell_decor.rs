//! Issue #455 — paragraph decorations inside table cells: `<w:pBdr>`,
//! `<w:shd>` and a tracked paragraph mark on paragraphs that live in
//! `<w:tc>` cells. Hand-written OOXML in Word's own shape (A4, 1-inch
//! margins, no styles part); the source of `tools/roundtrip`'s
//! `cell_paragraph_decor.docx`, the `cell-paragraph-decor` visual-diff
//! golden and the engine-wasm layout / PDF pins.

use super::docx_with_body;

/// The two top-level paragraphs around the table (the manifest asserts
/// them; the cell paragraphs are not top-level).
pub const CELL_DECOR_TEXTS: [&str; 2] = [
    "Paragraph decorations inside table cells",
    "A closing paragraph after the table.",
];

/// Cell (0,0): yellow shading and a red bottom rule.
pub const CELL_DECOR_SHADED_TEXT: &str = "Yellow shading with a red bottom rule, \
    inside a table cell, long enough to wrap onto a second line.";
/// Cell (0,1), first paragraph: a blue box on all four edges, pale blue fill.
pub const CELL_DECOR_BOXED_TEXT: &str = "A boxed paragraph with a pale blue fill.";
/// Cell (0,1), second paragraph: no decoration at all.
pub const CELL_DECOR_PLAIN_TEXT: &str = "A plain neighbour without decoration.";
/// Cell (1,0): a tracked INSERTED paragraph mark (and text).
pub const CELL_DECOR_INSERTED_TEXT: &str = "Inserted text and paragraph mark.";
/// Cell (1,1): a tracked DELETED paragraph mark (and text).
pub const CELL_DECOR_DELETED_TEXT: &str = "Deleted text and paragraph mark.";

/// Issue #455 — a 2 x 2 table (light grey grid) whose cell paragraphs
/// carry the decorations the body and story layout paths already paint:
/// row 0 a yellow-shaded paragraph with a red bottom rule and, beside it,
/// a blue-boxed pale-blue paragraph followed by a plain one; row 1 a
/// tracked inserted and a tracked deleted paragraph mark (the review
/// pilcrow colours). An intro paragraph precedes the table, a closing
/// one follows it.
pub fn cell_paragraph_decor_docx() -> Vec<u8> {
    const REV: &str = "w:author=\"Reviewer\" w:date=\"2026-01-01T00:00:00Z\"";
    let [intro, closing] = CELL_DECOR_TEXTS;
    let cell = |inner: &str| {
        format!("<w:tc><w:tcPr><w:tcW w:w=\"4513\" w:type=\"dxa\"/></w:tcPr>{inner}</w:tc>")
    };
    let grey = "w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"BFBFBF\"";
    let shaded = format!(
        "<w:p><w:pPr><w:pBdr><w:bottom w:val=\"single\" w:sz=\"12\" w:space=\"1\" \
         w:color=\"C00000\"/></w:pBdr><w:shd w:val=\"clear\" w:color=\"auto\" \
         w:fill=\"FFFF00\"/></w:pPr><w:r><w:t>{CELL_DECOR_SHADED_TEXT}</w:t></w:r></w:p>"
    );
    let edge =
        |e: &str| format!("<w:{e} w:val=\"single\" w:sz=\"8\" w:space=\"1\" w:color=\"1F3864\"/>");
    let boxed = format!(
        "<w:p><w:pPr><w:pBdr>{}{}{}{}</w:pBdr><w:shd w:val=\"clear\" w:color=\"auto\" \
         w:fill=\"DDEBF7\"/></w:pPr><w:r><w:t>{CELL_DECOR_BOXED_TEXT}</w:t></w:r></w:p>",
        edge("top"),
        edge("left"),
        edge("bottom"),
        edge("right"),
    );
    let plain = format!("<w:p><w:r><w:t>{CELL_DECOR_PLAIN_TEXT}</w:t></w:r></w:p>");
    let inserted = format!(
        "<w:p><w:pPr><w:rPr><w:ins w:id=\"1\" {REV}/></w:rPr></w:pPr>\
         <w:ins w:id=\"2\" {REV}><w:r><w:t>{CELL_DECOR_INSERTED_TEXT}</w:t></w:r></w:ins></w:p>"
    );
    let deleted = format!(
        "<w:p><w:pPr><w:rPr><w:del w:id=\"3\" {REV}/></w:rPr></w:pPr>\
         <w:del w:id=\"4\" {REV}><w:r><w:delText>{CELL_DECOR_DELETED_TEXT}</w:delText></w:r></w:del></w:p>"
    );
    let body = format!(
        "<w:p><w:r><w:t>{intro}</w:t></w:r></w:p>\
         <w:tbl><w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblBorders>\
         <w:top {grey}/><w:left {grey}/><w:bottom {grey}/><w:right {grey}/>\
         <w:insideH {grey}/><w:insideV {grey}/></w:tblBorders>\
         <w:tblLook w:val=\"04A0\"/></w:tblPr>\
         <w:tblGrid><w:gridCol w:w=\"4513\"/><w:gridCol w:w=\"4513\"/></w:tblGrid>\
         <w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>\
         <w:p><w:r><w:t>{closing}</w:t></w:r></w:p>",
        cell(&shaded),
        cell(&format!("{boxed}{plain}")),
        cell(&inserted),
        cell(&deleted),
    );
    docx_with_body(&body)
}
