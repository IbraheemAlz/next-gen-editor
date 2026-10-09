//! Issue #360 — the document-model side table `format_pdf::
//! export_pdf_document` consumes: what the PDF needs (headings for the
//! outline) that the layout box tree does not carry.
//!
//! The table is indexed like `do_export_pdf`'s `para_texts` — by
//! `ParagraphBox::source_paragraph_id` — so every walk here mirrors the
//! `walk_block_texts` call it sits beside, entry for entry.

use format_pdf::ParagraphSemantics;

/// One entry per paragraph of `block`, in `walk_block_texts` order (table
/// rows × cells × cell blocks, `VMergeRole::Continue` cells skipped).
pub(crate) fn walk_block_semantics(
    doc: &engine::DocumentTree,
    block: &engine::Block,
    out: &mut Vec<ParagraphSemantics>,
) {
    match block {
        engine::Block::Paragraph(p) => out.push(paragraph_semantics(doc, p)),
        engine::Block::Table(t) => {
            for row in &t.rows {
                for cell in &row.cells {
                    if cell.props.v_merge == engine::VMergeRole::Continue {
                        continue;
                    }
                    for b in &cell.blocks {
                        walk_block_semantics(doc, b, out);
                    }
                }
            }
        }
    }
}

/// The semantics of one body paragraph.
fn paragraph_semantics(doc: &engine::DocumentTree, p: &engine::Paragraph) -> ParagraphSemantics {
    let heading = doc.outline_heading_level(p);
    ParagraphSemantics {
        heading,
        title: heading
            .map(|_| engine::toc::entry_text(p))
            .unwrap_or_default(),
    }
}
