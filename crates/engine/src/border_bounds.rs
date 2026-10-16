//! Issue #503 (residue) - Word's bound for a border stroke's width.
//!
//! `BorderStroke::size_eighth_pt` is a `u16`: `SetCellBorders` /
//! `SetParagraphBorders` took up to 8191 pt, the `.docx` reader any `w:sz`
//! that fits a `u16` (and silently read a digit string past it as the
//! default 4), HTML paste's `css_px_to_eighth_pt` anything a CSS number can
//! spell. Word draws a line from 1/4 pt to 6 pt (`w:sz` 2..=48 eighths of a
//! point), and a `none` / `nil` edge paints nothing at any size. This is the
//! model's side of the bound, ONE module shared by every door that sets a
//! stroke width:
//!
//! - the command path (engine-wasm `border_guard`) REFUSES a width outside
//!   the range with `ErrorKind::OutOfRange`, naming the command and field;
//! - the `.docx` reader (`w:sz` on the paragraph, cell, table and page
//!   border edges) and HTML paste CLAMP into it, the reader reporting
//!   `DocxWarning::MeasureClamped`;
//! - a restored snapshot is repaired by `DocumentTree::repair_bounds`
//!   ([`repair_cell_borders`] / [`repair_page_borders`]).
//!
//! [`first_border_beyond_bound`] is the executable form of the rule (folded
//! into `first_point_field_beyond_bound`, so the `rpc_command` and
//! `snapshot_decode` fuzz invariants cover it).

use crate::{
    Block, BorderStroke, BorderStyle, CellBorders, DocumentTree, PageBorders, ParaProperties,
    SectionProps,
};

/// Word's widest line: 6 pt = 48 eighths of a point.
pub const MAX_BORDER_EIGHTH_PT: u16 = 48;

/// Word's thinnest drawn line: 1/4 pt = 2 eighths of a point.
pub const MIN_BORDER_EIGHTH_PT: u16 = 2;

/// The width range, in eighths of a point, a stroke of `style` may hold: a
/// drawn line `MIN..=MAX`; a `none` stroke paints nothing, so it may also
/// be `0`.
pub fn border_size_range(style: &BorderStyle) -> (u16, u16) {
    if matches!(style, BorderStyle::None) {
        (0, MAX_BORDER_EIGHTH_PT)
    } else {
        (MIN_BORDER_EIGHTH_PT, MAX_BORDER_EIGHTH_PT)
    }
}

/// `true` when `size` (eighths of a point) is a width Word can hold for a
/// stroke of `style`.
pub fn border_size_in_bound(style: &BorderStyle, size: u16) -> bool {
    let (lo, hi) = border_size_range(style);
    (lo..=hi).contains(&size)
}

/// `size` pulled into the range of [`border_size_range`].
pub fn clamp_border_eighth_pt(style: &BorderStyle, size: u16) -> u16 {
    let (lo, hi) = border_size_range(style);
    size.clamp(lo, hi)
}

/// The six edges of a [`CellBorders`], named as the `.docx` spells them.
fn edges_mut(b: &mut CellBorders) -> [&mut Option<BorderStroke>; 6] {
    [
        &mut b.top,
        &mut b.left,
        &mut b.bottom,
        &mut b.right,
        &mut b.inside_h,
        &mut b.inside_v,
    ]
}

fn edges(b: &CellBorders) -> [(&'static str, &Option<BorderStroke>); 6] {
    [
        ("top", &b.top),
        ("left", &b.left),
        ("bottom", &b.bottom),
        ("right", &b.right),
        ("insideH", &b.inside_h),
        ("insideV", &b.inside_v),
    ]
}

fn repair_stroke(s: &mut Option<BorderStroke>) -> u32 {
    let Some(stroke) = s.as_mut() else { return 0 };
    let c = clamp_border_eighth_pt(&stroke.style, stroke.size_eighth_pt);
    u32::from(std::mem::replace(&mut stroke.size_eighth_pt, c) != c)
}

/// Pull every edge of `b` into Word's width range; the number of strokes
/// moved. Restore repair.
pub fn repair_cell_borders(b: &mut Option<CellBorders>) -> u32 {
    let Some(b) = b.as_mut() else { return 0 };
    edges_mut(b).into_iter().map(repair_stroke).sum()
}

/// [`repair_cell_borders`] for a section's `<w:pgBorders>`.
pub fn repair_page_borders(b: &mut Option<PageBorders>) -> u32 {
    let Some(b) = b.as_mut() else { return 0 };
    [&mut b.top, &mut b.left, &mut b.bottom, &mut b.right]
        .into_iter()
        .map(repair_stroke)
        .sum()
}

fn scan_stroke(name: &str, s: &Option<BorderStroke>) -> Option<String> {
    let s = s.as_ref()?;
    let (lo, hi) = border_size_range(&s.style);
    (!border_size_in_bound(&s.style, s.size_eighth_pt)).then(|| {
        format!(
            "border {name} width {} eighths of a point is outside {lo}..={hi}",
            s.size_eighth_pt
        )
    })
}

fn scan_cell_borders(b: &Option<CellBorders>) -> Option<String> {
    let b = b.as_ref()?;
    edges(b)
        .into_iter()
        .find_map(|(name, s)| scan_stroke(name, s))
}

fn scan_page_borders(b: &Option<PageBorders>) -> Option<String> {
    let b = b.as_ref()?;
    [
        ("pgBorders top", &b.top),
        ("pgBorders left", &b.left),
        ("pgBorders bottom", &b.bottom),
        ("pgBorders right", &b.right),
    ]
    .into_iter()
    .find_map(|(name, s)| scan_stroke(name, s))
}

fn scan_props(p: &ParaProperties) -> Option<String> {
    scan_cell_borders(&p.borders)
}

fn scan_section(s: &SectionProps) -> Option<String> {
    scan_page_borders(&s.page_borders)
}

/// Every block reachable from `roots` - table cells and text-box stories at
/// any depth - walked with an explicit worklist (a hostile tree's depth
/// cannot run it off the stack).
fn scan_blocks<'a>(roots: impl IntoIterator<Item = &'a Block>) -> Option<String> {
    let mut stack: Vec<&Block> = roots.into_iter().collect();
    while let Some(block) = stack.pop() {
        match block {
            Block::Paragraph(p) => {
                if let Some(why) = scan_props(&p.props) {
                    return Some(why);
                }
                if let Some(why) = scan_props(&p.direct_overrides) {
                    return Some(format!("direct override: {why}"));
                }
                if let Some(why) = p.section_end.as_deref().and_then(scan_section) {
                    return Some(format!("section break: {why}"));
                }
                for io in &p.inline_objects {
                    if let crate::InlineKind::TextBox { story, .. } = &io.kind {
                        stack.extend(story.body.iter());
                    }
                }
            }
            Block::Table(t) => {
                if let Some(why) = scan_cell_borders(&t.props.borders) {
                    return Some(format!("table {why}"));
                }
                for row in &t.rows {
                    for cell in &row.cells {
                        if let Some(why) = scan_cell_borders(&cell.props.borders) {
                            return Some(format!("cell {why}"));
                        }
                        stack.extend(cell.blocks.iter());
                    }
                }
            }
        }
    }
    None
}

/// The first border stroke in `doc` whose width is outside Word's range,
/// described for a failure message; `None` when every stroke is inside.
/// Walks the body (paragraph, table and cell borders at every depth, text
/// boxes), section page borders, header / footer / note stories, the style
/// table and the document defaults - exactly what `repair_bounds` repairs.
pub fn first_border_beyond_bound(doc: &DocumentTree) -> Option<String> {
    if let Some(why) = scan_section(&doc.body_section) {
        return Some(format!("body section: {why}"));
    }
    if let Some(why) = scan_props(&doc.style_defaults) {
        return Some(format!("document defaults: {why}"));
    }
    let mut style_ids: Vec<&String> = doc.styles.keys().collect();
    style_ids.sort();
    for id in style_ids {
        if let Some(why) = doc.styles.get(id).and_then(|s| scan_props(&s.para)) {
            return Some(format!("style {id}: {why}"));
        }
    }
    if let Some(why) = scan_blocks(doc.blocks.iter()) {
        return Some(why);
    }
    let mut stories: Vec<(&String, &Vec<Block>)> =
        doc.headers.iter().chain(doc.footers.iter()).collect();
    stories.sort_by(|a, b| a.0.cmp(b.0));
    for (id, blocks) in stories {
        if let Some(why) = scan_blocks(blocks.iter()) {
            return Some(format!("story {id}: {why}"));
        }
    }
    let mut notes: Vec<_> = doc
        .footnote_stories
        .iter()
        .chain(doc.endnote_stories.iter())
        .collect();
    notes.sort_by_key(|(id, _)| **id);
    for (id, note) in notes {
        if let Some(why) = scan_blocks(note.body.iter()) {
            return Some(format!("note {id}: {why}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Paragraph, Table, TableCell, TableRow};

    fn stroke(style: BorderStyle, size: u16) -> BorderStroke {
        BorderStroke {
            style,
            size_eighth_pt: size,
            ..Default::default()
        }
    }

    #[test]
    fn the_bounds_are_words() {
        assert_eq!(MAX_BORDER_EIGHTH_PT, 6 * 8, "6 pt");
        assert_eq!(MIN_BORDER_EIGHTH_PT, 8 / 4, "1/4 pt");
    }

    #[test]
    fn membership_and_clamp_agree_at_the_edges() {
        for (size, drawn, none) in [
            (0u16, false, true),
            (1, false, true),
            (2, true, true),
            (47, true, true),
            (48, true, true),
            (49, false, false),
            (96, false, false),
            (u16::MAX, false, false),
        ] {
            assert_eq!(
                border_size_in_bound(&BorderStyle::Single, size),
                drawn,
                "drawn {size}"
            );
            assert_eq!(
                border_size_in_bound(&BorderStyle::None, size),
                none,
                "none {size}"
            );
            for style in [
                BorderStyle::Single,
                BorderStyle::Double,
                BorderStyle::Dotted,
                BorderStyle::Dashed,
                BorderStyle::Other("wave".into()),
                BorderStyle::None,
            ] {
                let c = clamp_border_eighth_pt(&style, size);
                assert!(border_size_in_bound(&style, c), "{style:?} {size} -> {c}");
            }
        }
        assert_eq!(clamp_border_eighth_pt(&BorderStyle::Single, 0), 2);
        assert_eq!(clamp_border_eighth_pt(&BorderStyle::None, 0), 0);
        assert_eq!(clamp_border_eighth_pt(&BorderStyle::Dashed, u16::MAX), 48);
    }

    #[test]
    fn a_clean_document_scans_clean_and_a_hostile_one_names_the_edge() {
        let mut doc = DocumentTree::from_text("hello");
        assert_eq!(first_border_beyond_bound(&doc), None);

        let mut para = Paragraph::default();
        para.props.borders = Some(CellBorders {
            bottom: Some(stroke(BorderStyle::Single, 49)),
            ..Default::default()
        });
        doc.blocks.push_back(Block::Paragraph(para));
        let why = first_border_beyond_bound(&doc).expect("paragraph border");
        assert!(why.contains("bottom") && why.contains("49"), "{why}");
    }

    #[test]
    fn repair_reaches_cells_tables_pages_styles_and_text_boxes() {
        let mut doc = DocumentTree::from_text("body");
        let cell = TableCell {
            props: crate::CellProperties {
                borders: Some(CellBorders {
                    left: Some(stroke(BorderStyle::Single, 0)),
                    right: Some(stroke(BorderStyle::None, 0)),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        let table = Table {
            props: crate::TableProperties {
                borders: Some(CellBorders {
                    inside_h: Some(stroke(BorderStyle::Double, u16::MAX)),
                    ..Default::default()
                }),
                ..Default::default()
            },
            rows: vec![TableRow {
                cells: vec![cell],
                ..Default::default()
            }],
            ..Default::default()
        };
        doc.blocks.push_back(Block::Table(table));
        doc.body_section.page_borders = Some(PageBorders {
            top: Some(stroke(BorderStyle::Single, 4000)),
            ..Default::default()
        });
        doc.style_defaults.borders = Some(CellBorders {
            top: Some(stroke(BorderStyle::Dotted, 96)),
            ..Default::default()
        });
        assert!(first_border_beyond_bound(&doc).is_some());
        /* page top, defaults top, table insideH, cell left (0 drawn -> 2);
        the cell's `none` 0 is fine. */
        assert_eq!(doc.repair_bounds(), 4);
        assert_eq!(first_border_beyond_bound(&doc), None);
        assert_eq!(doc.repair_bounds(), 0, "idempotent");
    }
}
