//! Issue #503 (item 1) - the restore-side repair for Word's size / point
//! bounds.
//!
//! #501 / #503 bound every door that SETS a font size or a page /
//! paragraph point field: the command guards refuse, the `.docx` reader
//! and the HTML paste door clamp. A crash-recovery snapshot is a door
//! none of them guard - its bytes come back from IndexedDB (or, for the
//! `snapshot_decode` fuzz target, from anywhere) and `Engine::restore`
//! installed them verbatim, so a `1e33` pt run or a `1e9` pt margin that
//! the command path would have refused reached layout and was defended
//! only by the paginator's cull clamp and the #499 progress bound.
//!
//! [`DocumentTree::repair_bounds`] is the restore door's clamp, the twin of
//! `repair_revision_ranges` (#283): it walks the whole model - body, table
//! cells at every depth, text-box stories, headers, footers, notes, the
//! style table and the document defaults - and pulls every value into the
//! SAME bounds the other doors enforce (`font_size_bounds`,
//! `point_bounds`), counting what it moved. The count is reported on
//! `Event::Recovered.bounds_repaired` so the user is told (a repaired
//! value is a changed document, however small).
//!
//! What it covers is exactly what the executable rules
//! ([`crate::first_font_size_beyond_bound`] /
//! [`crate::first_point_field_beyond_bound`]) check, so "restore, then the
//! rule holds" is an invariant the `snapshot_decode` fuzz target asserts.
//! A value that is not a number (NaN survives a MessagePack float) is
//! dropped (a size becomes unset and inherits) or reset to the A4 default,
//! never clamped into a bound it has no ordering against.

use crate::{
    Block, DocumentTree, InlineKind, LineHeight, PageGeometry, ParaProperties, Paragraph,
    SectionProps, SpanStyle, TabStop, font_size_bounds, point_bounds,
};

/// Pull `v` into `lo..=hi`; `1` when it moved.
fn clamp_i32(v: &mut i32, lo: i32, hi: i32) -> u32 {
    let c = (*v).clamp(lo, hi);
    u32::from(std::mem::replace(v, c) != c)
}

/// Pull `v` into `lo..=hi` (a NaN becomes `nan_to`); `1` when it moved.
fn clamp_f32(v: &mut f32, lo: f32, hi: f32, nan_to: f32) -> u32 {
    let c = if v.is_nan() { nan_to } else { v.clamp(lo, hi) };
    /* Bitwise: `-0.0 == 0.0`, and a NaN never equals itself. */
    let moved = c.to_bits() != v.to_bits();
    *v = c;
    u32::from(moved)
}

fn repair_size(size: &mut Option<f32>) -> u32 {
    let Some(pt) = *size else { return 0 };
    if font_size_bounds::font_size_in_bound(pt) {
        return 0;
    }
    *size = pt
        .is_finite()
        .then(|| font_size_bounds::clamp_font_size_pt(pt));
    1
}

/// A run / mark / style size pair pulled into Word's `1..=1638` pt; the
/// number of values moved.
pub fn repair_span_style_sizes(style: &mut SpanStyle) -> u32 {
    repair_size(&mut style.font_size) + repair_size(&mut style.font_size_cs)
}

fn repair_tab_stops(stops: &mut [TabStop]) -> u32 {
    let max = point_bounds::MAX_POINT_FIELD_PT;
    stops
        .iter_mut()
        .map(|s| clamp_f32(&mut s.position_pt, -max, max, 0.0))
        .sum()
}

fn repair_para_props(p: &mut ParaProperties) -> u32 {
    let max = point_bounds::MAX_POINT_FIELD_TWIPS;
    let mut n = 0;
    n += clamp_i32(&mut p.indent.start_twips, -max, max);
    n += clamp_i32(&mut p.indent.end_twips, -max, max);
    n += clamp_i32(&mut p.indent.first_line_twips, 0, max);
    n += clamp_i32(&mut p.indent.hanging_twips, 0, max);
    n += clamp_i32(&mut p.spacing.before_twips, 0, max);
    n += clamp_i32(&mut p.spacing.after_twips, 0, max);
    if let Some(
        LineHeight::Auto { twips } | LineHeight::Exact { twips } | LineHeight::AtLeast { twips },
    ) = p.line_height.as_mut()
    {
        n += clamp_i32(twips, -max, max);
    }
    n + repair_tab_stops(&mut p.tab_stops)
}

fn repair_geometry(g: &mut PageGeometry) -> u32 {
    let max = point_bounds::MAX_POINT_FIELD_PT;
    let a4 = PageGeometry::a4();
    let mut n = 0;
    n += clamp_f32(&mut g.width, point_bounds::MIN_PAGE_SIDE_PT, max, a4.width);
    n += clamp_f32(
        &mut g.height,
        point_bounds::MIN_PAGE_SIDE_PT,
        max,
        a4.height,
    );
    for (v, default) in [
        (&mut g.margin_top, a4.margin_top),
        (&mut g.margin_right, a4.margin_right),
        (&mut g.margin_bottom, a4.margin_bottom),
        (&mut g.margin_left, a4.margin_left),
        (&mut g.header_offset, a4.header_offset),
        (&mut g.footer_offset, a4.footer_offset),
    ] {
        n += clamp_f32(v, -max, max, default);
    }
    /* What no single field shows: margins that together swallow the page
    (the reader's own repair, `DocxWarning::MeasureClamped`). */
    n + u32::from(point_bounds::fit_margins_to_text_area(g))
}

fn repair_section(s: &mut SectionProps) -> u32 {
    repair_geometry(&mut s.geometry)
        + clamp_f32(
            &mut s.columns.gutter_pt,
            0.0,
            point_bounds::MAX_POINT_FIELD_PT,
            0.0,
        )
}

fn repair_paragraph<'a>(para: &'a mut Paragraph, children: &mut Vec<&'a mut Block>) -> u32 {
    let mut n = repair_para_props(&mut para.props) + repair_para_props(&mut para.direct_overrides);
    for run in &mut para.spans {
        n += repair_span_style_sizes(&mut run.style);
    }
    if let Some(mark) = para.mark_style.as_deref_mut() {
        n += repair_span_style_sizes(mark);
    }
    for rev in &mut para.revisions {
        if let Some(prev) = rev.prev_attrs.as_mut() {
            n += repair_span_style_sizes(prev);
        }
    }
    if let Some(section) = para.section_end.as_deref_mut() {
        n += repair_section(section);
    }
    for io in &mut para.inline_objects {
        if let InlineKind::TextBox { story, .. } = &mut io.kind {
            children.extend(story.body.iter_mut());
        }
    }
    n
}

/// Every paragraph reachable from `roots` - through table cells and
/// text-box stories at any depth. An explicit worklist, not recursion: a
/// hostile tree's depth cannot run the walker off the stack.
fn repair_blocks<'a>(roots: impl Iterator<Item = &'a mut Block>) -> u32 {
    let mut n = 0;
    let mut stack: Vec<&'a mut Block> = roots.collect();
    while let Some(block) = stack.pop() {
        match block {
            Block::Paragraph(p) => n += repair_paragraph(p, &mut stack),
            Block::Table(t) => {
                for row in &mut t.rows {
                    for cell in &mut row.cells {
                        stack.extend(cell.blocks.iter_mut());
                    }
                }
            }
        }
    }
    n
}

impl DocumentTree {
    /// Issue #503 - hold every font size and every page / paragraph point
    /// field of this tree to Word's bounds, in place; the number of values
    /// moved (`0` for any tree the engine, the reader or a command built).
    /// Run on a restored snapshot, whose bytes may come from anywhere (see
    /// the module doc). Idempotent: a repaired tree repairs to `0`.
    pub fn repair_bounds(&mut self) -> u32 {
        let mut n = repair_blocks(self.blocks.iter_mut());
        for part in self.headers.values_mut().chain(self.footers.values_mut()) {
            n += repair_blocks(part.iter_mut());
        }
        for note in self
            .footnote_stories
            .values_mut()
            .chain(self.endnote_stories.values_mut())
        {
            n += repair_blocks(note.body.iter_mut());
        }
        n += repair_section(&mut self.body_section);
        n += repair_para_props(&mut self.style_defaults);
        n += repair_span_style_sizes(&mut self.style_run_defaults);
        for style in self.styles.values_mut() {
            n += repair_para_props(&mut style.para);
            n += repair_span_style_sizes(&mut style.run);
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StyleRun, TabKind, TabLeader};

    fn sized(pt: f32) -> SpanStyle {
        SpanStyle {
            font_size: Some(pt),
            ..Default::default()
        }
    }

    fn hostile() -> DocumentTree {
        let mut doc = DocumentTree::from_text("hello");
        {
            let p = doc.blocks[0].as_paragraph_mut().expect("paragraph");
            p.spans.push(StyleRun {
                start: 0,
                end: 5,
                style: sized(1.0e33),
            });
            p.mark_style = Some(Box::new(sized(0.0)));
            p.props.indent.start_twips = i32::MAX;
            p.props.spacing.after_twips = -5;
            p.props.tab_stops.push(TabStop {
                position_pt: -1e30,
                kind: TabKind::Left,
                leader: TabLeader::None,
            });
            p.props.tab_stops.push(TabStop {
                position_pt: f32::NAN,
                kind: TabKind::Left,
                leader: TabLeader::None,
            });
        }
        doc.body_section.geometry.margin_left = 1e9;
        doc.body_section.geometry.margin_right = 1e9;
        doc.body_section.columns.gutter_pt = f32::MAX;
        doc.style_run_defaults = sized(5000.0);
        doc
    }

    #[test]
    fn a_clean_tree_repairs_to_zero() {
        let mut doc = DocumentTree::from_text("hello");
        assert_eq!(doc.repair_bounds(), 0);
        let mut a4 = DocumentTree::new();
        assert_eq!(a4.repair_bounds(), 0);
    }

    #[test]
    fn a_hostile_tree_is_clamped_counted_and_then_holds_the_rules() {
        let mut doc = hostile();
        assert!(first_font_size_beyond_bound_or_point(&doc));
        let n = doc.repair_bounds();
        /* run size, mark size, indent, spacing, two tab stops, the two
        margins (clamped to 1584 then fitted - the fit counts once), the
        gutter, the run default. */
        assert!(n >= 9, "{n}");
        assert_eq!(crate::first_font_size_beyond_bound(&doc), None);
        assert_eq!(crate::first_point_field_beyond_bound(&doc), None);
        let p = doc.blocks[0].as_paragraph().expect("paragraph");
        assert_eq!(p.spans[0].style.font_size, Some(1638.0));
        assert_eq!(p.mark_style.as_ref().and_then(|m| m.font_size), Some(1.0));
        assert_eq!(p.props.tab_stops[1].position_pt, 0.0, "NaN resets to 0");
        assert_eq!(doc.repair_bounds(), 0, "idempotent");
    }

    #[test]
    fn a_nan_size_is_dropped_not_clamped() {
        let mut style = sized(f32::NAN);
        assert_eq!(repair_span_style_sizes(&mut style), 1);
        assert_eq!(style.font_size, None);
        let mut inf = sized(f32::INFINITY);
        assert_eq!(repair_span_style_sizes(&mut inf), 1);
        assert_eq!(inf.font_size, None);
    }

    fn first_font_size_beyond_bound_or_point(doc: &DocumentTree) -> bool {
        crate::first_font_size_beyond_bound(doc).is_some()
            && crate::first_point_field_beyond_bound(doc).is_some()
    }

    #[test]
    fn it_reaches_cells_headers_notes_styles_and_text_boxes() {
        use crate::{ParagraphStyle, Table, TableCell, TableRow};
        let mut doc = DocumentTree::from_text("body");
        let mut cell_para = Paragraph::default();
        cell_para.props.indent.end_twips = -99_999;
        let cell = TableCell {
            blocks: vec![Block::Paragraph(cell_para)],
            ..Default::default()
        };
        let row = TableRow {
            cells: vec![cell],
            ..Default::default()
        };
        let table = Table {
            rows: vec![row],
            ..Default::default()
        };
        doc.blocks.push_back(Block::Table(table));
        let mut head = Paragraph::default();
        head.spans.push(StyleRun {
            start: 0,
            end: 0,
            style: sized(9999.0),
        });
        doc.headers
            .insert("rId1".into(), vec![Block::Paragraph(head)]);
        doc.styles.insert(
            "Big".into(),
            ParagraphStyle {
                id: "Big".into(),
                run: sized(1e30),
                para: ParaProperties {
                    line_height: Some(LineHeight::Exact { twips: i32::MIN }),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        assert!(crate::first_font_size_beyond_bound(&doc).is_some());
        assert!(crate::first_point_field_beyond_bound(&doc).is_some());
        assert_eq!(doc.repair_bounds(), 4);
        assert_eq!(crate::first_font_size_beyond_bound(&doc), None);
        assert_eq!(crate::first_point_field_beyond_bound(&doc), None);
    }
}
