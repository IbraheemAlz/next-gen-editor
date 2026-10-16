//! Issue #503 (item 2) - Word's bounds for the page / paragraph point
//! fields.
//!
//! The #407 finiteness guard only refuses `NaN` / infinity, so
//! `SetPageMargins` took `1e33` pt for a margin, `SetParagraphIndent` an
//! indent of `f32::MAX`, `SetColumns` a gutter wider than the universe and
//! `SetTabStops` a stop at `-1e30`: finite, absurd, and a page with no
//! text area (or a line wrapped in zero width) for the layout to survive.
//! This is the model's side of the bound, ONE module beside
//! `font_size_bounds` / `emu_bounds`, shared by every door that sets one
//! of these fields:
//!
//! - the command path (engine-wasm `point_guard`, and the section-aware
//!   margin check in `do_set_page_margins`) REFUSES a value outside the
//!   bound with `ErrorKind::OutOfRange`, naming the command and the field;
//! - the `.docx` reader CLAMPS: per attribute `schema::measure`
//!   (`TWIPS` / `SIGNED_TWIPS` / `PAGE_SIZE` = +-22 in, a page at least
//!   0.1 in) already does it for every twips attribute the model holds
//!   (`w:pgSz`, `w:pgMar`, `w:ind`, `w:spacing`, `w:tab`, `w:cols
//!   w:space`); what no single attribute can see - four margins that
//!   together swallow the page - [`fit_margins_to_text_area`] repairs
//!   (reported as `DocxWarning::MeasureClamped`);
//! - HTML paste reads none of these properties (no indent / margin /
//!   spacing / tab CSS is imported), so it has no door to bound.
//!
//! Word's limits: a page side 0.1 in ..= 22 in; margins non-negative and
//! at most 22 in, the pair on one axis leaving at least 0.1 in of text
//! area; indents, spacing and tab positions at most 22 in (1584 pt).

use crate::{
    Block, DocumentTree, LineHeight, PageGeometry, ParaProperties, Paragraph, SectionProps,
};

/// Word's longest page side, indent, paragraph spacing, margin, tab
/// position and column gutter: 22 in = 1584 pt (31 680 twips).
pub const MAX_POINT_FIELD_PT: f32 = 1584.0;

/// The same bound in twips - what the model stores for indents, spacing
/// and line heights (`schema::measure::MAX_TWIPS` spells the same number).
pub const MAX_POINT_FIELD_TWIPS: i32 = 31_680;

/// Word's longest tab-stop list: 64 stops per paragraph.
pub const MAX_TAB_STOPS: usize = 64;

/// Word's shortest page side, in points: 0.1 in (`PAGE_SIZE` min 144 twips).
pub const MIN_PAGE_SIDE_PT: f32 = 7.2;

/// The least text area (per axis) a page's margins may leave: 0.1 in.
pub const MIN_TEXT_AREA_PT: f32 = 7.2;

/// Word's "Multiple" line spacing: 0.06 ..= 132 lines (132 x 12 pt = 1584 pt).
/// `SetLineSpacing` / `ModifyStyle` take a multiplier at or below 0 to
/// clear the override; anything positive must sit in this range.
pub const MIN_LINE_SPACING_MULTIPLE: f32 = 0.06;
pub const MAX_LINE_SPACING_MULTIPLE: f32 = 132.0;

/// A page margin the command path accepts: `0 ..= 1584` pt.
pub fn margin_in_bound(pt: f32) -> bool {
    (0.0..=MAX_POINT_FIELD_PT).contains(&pt)
}

/// An indent (start / end / first line, hanging as a negative first line)
/// the command path accepts: `|pt| <= 1584`. An outdent past the margin is
/// legal (negative `w:start`), so the lower bound is symmetric.
pub fn indent_in_bound(pt: f32) -> bool {
    (-MAX_POINT_FIELD_PT..=MAX_POINT_FIELD_PT).contains(&pt)
}

/// A tab-stop position the command path accepts: `|pt| <= 1584`.
pub fn tab_position_in_bound(pt: f32) -> bool {
    (-MAX_POINT_FIELD_PT..=MAX_POINT_FIELD_PT).contains(&pt)
}

/// A column gutter (`SetColumns.gutter_pt`) the command path accepts:
/// `0 ..= 1584` pt.
pub fn column_gutter_in_bound(pt: f32) -> bool {
    (0.0..=MAX_POINT_FIELD_PT).contains(&pt)
}

/// A "Multiple" line-spacing multiplier the command path accepts: at or
/// below `0` (the documented "clear the override" sentinel) or inside
/// [`MIN_LINE_SPACING_MULTIPLE`]`..=`[`MAX_LINE_SPACING_MULTIPLE`].
pub fn line_spacing_multiple_in_bound(multiplier: f32) -> bool {
    multiplier <= 0.0
        || (MIN_LINE_SPACING_MULTIPLE..=MAX_LINE_SPACING_MULTIPLE).contains(&multiplier)
}

/// The axis on which a set of margins swallows the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAreaAxis {
    /// `left + right` leaves less than [`MIN_TEXT_AREA_PT`] of width.
    Horizontal,
    /// `top + bottom` leaves less than [`MIN_TEXT_AREA_PT`] of height.
    Vertical,
}

impl TextAreaAxis {
    /// The two margin fields this axis sums, for an error message.
    pub fn fields(self) -> &'static str {
        match self {
            TextAreaAxis::Horizontal => "left_pt + right_pt",
            TextAreaAxis::Vertical => "top_pt + bottom_pt",
        }
    }
}

/// `Ok` when margins `top` / `right` / `bottom` / `left` leave at least
/// [`MIN_TEXT_AREA_PT`] of text area on a `width` x `height` page, on both
/// axes (a negative margin counts as 0 - Word reads its magnitude as the
/// body offset and lets the header overlap, so it never *adds* room).
/// Summed in `f64`: the check sits exactly on the boundary for the stock
/// margins and must not flip with `f32` rounding.
pub fn margins_leave_text_area(
    width: f32,
    height: f32,
    top: f32,
    right: f32,
    bottom: f32,
    left: f32,
) -> Result<(), TextAreaAxis> {
    let leaves = |page: f32, a: f32, b: f32| {
        f64::from(a.max(0.0)) + f64::from(b.max(0.0))
            <= f64::from(page) - f64::from(MIN_TEXT_AREA_PT) + 1e-4
    };
    if !leaves(width, left, right) {
        return Err(TextAreaAxis::Horizontal);
    }
    if !leaves(height, top, bottom) {
        return Err(TextAreaAxis::Vertical);
    }
    Ok(())
}

/// The reader's repair for margins that together swallow the page: on an
/// axis whose positive margins leave less than [`MIN_TEXT_AREA_PT`], both
/// are scaled by one factor until they leave exactly that much (negative
/// margins are left alone). A NaN or infinite margin cannot reach here
/// (`measure` refuses it); a non-finite page side is not this function's
/// business. Returns `true` when anything moved.
pub fn fit_margins_to_text_area(g: &mut PageGeometry) -> bool {
    fn fit(page: f32, a: &mut f32, b: &mut f32) -> bool {
        let (pa, pb) = (a.max(0.0), b.max(0.0));
        let room = (page - MIN_TEXT_AREA_PT).max(0.0);
        let sum = f64::from(pa) + f64::from(pb);
        if sum <= f64::from(room) + 1e-4 {
            return false;
        }
        /* Aim a hair under the room so `f32` rounding of the scaled
        margins can never leave the pair a hair over it again. */
        let k = ((f64::from(room) - 0.01).max(0.0) / sum) as f32;
        if *a > 0.0 {
            *a *= k;
        }
        if *b > 0.0 {
            *b *= k;
        }
        true
    }
    let (w, h) = (g.width, g.height);
    let horizontal = fit(w, &mut g.margin_left, &mut g.margin_right);
    let vertical = fit(h, &mut g.margin_top, &mut g.margin_bottom);
    horizontal || vertical
}

/// Text boxes nest at most this deep before the scan stops.
const MAX_SCAN_DEPTH: u32 = 32;

fn twips_in_bound(t: i32) -> bool {
    (-MAX_POINT_FIELD_TWIPS..=MAX_POINT_FIELD_TWIPS).contains(&t)
}

fn unsigned_twips_in_bound(t: i32) -> bool {
    (0..=MAX_POINT_FIELD_TWIPS).contains(&t)
}

fn scan_props(p: &ParaProperties) -> Option<String> {
    let i = &p.indent;
    for (name, t) in [
        ("indent.start_twips", i.start_twips),
        ("indent.end_twips", i.end_twips),
    ] {
        if !twips_in_bound(t) {
            return Some(format!("{name} {t} is outside +-{MAX_POINT_FIELD_TWIPS}"));
        }
    }
    for (name, t) in [
        ("indent.first_line_twips", i.first_line_twips),
        ("indent.hanging_twips", i.hanging_twips),
        ("spacing.before_twips", p.spacing.before_twips),
        ("spacing.after_twips", p.spacing.after_twips),
    ] {
        if !unsigned_twips_in_bound(t) {
            return Some(format!("{name} {t} is outside 0..={MAX_POINT_FIELD_TWIPS}"));
        }
    }
    if let Some(lh) = p.line_height {
        let (LineHeight::Auto { twips }
        | LineHeight::Exact { twips }
        | LineHeight::AtLeast { twips }) = lh;
        if !twips_in_bound(twips) {
            return Some(format!(
                "line_height {twips} twips is outside +-{MAX_POINT_FIELD_TWIPS}"
            ));
        }
    }
    if p.tab_stops.len() > MAX_TAB_STOPS {
        return Some(format!(
            "{} tab stops, more than Word's {MAX_TAB_STOPS}",
            p.tab_stops.len()
        ));
    }
    for stop in &p.tab_stops {
        if !tab_position_in_bound(stop.position_pt) {
            return Some(format!(
                "tab stop at {} pt is outside +-{MAX_POINT_FIELD_PT}",
                stop.position_pt
            ));
        }
    }
    None
}

fn scan_section(s: &SectionProps) -> Option<String> {
    let g = &s.geometry;
    for (name, v) in [("width", g.width), ("height", g.height)] {
        if !(MIN_PAGE_SIDE_PT..=MAX_POINT_FIELD_PT).contains(&v) {
            return Some(format!(
                "page {name} {v} is outside {MIN_PAGE_SIDE_PT}..={MAX_POINT_FIELD_PT}"
            ));
        }
    }
    /* Reader-read top / bottom margins are signed (a negative one is
    legal in a file); the command path only writes non-negative ones. */
    for (name, v) in [
        ("margin_left", g.margin_left),
        ("margin_right", g.margin_right),
        ("margin_top", g.margin_top),
        ("margin_bottom", g.margin_bottom),
    ] {
        if !(-MAX_POINT_FIELD_PT..=MAX_POINT_FIELD_PT).contains(&v) {
            return Some(format!("{name} {v} is outside +-{MAX_POINT_FIELD_PT}"));
        }
    }
    for (name, v) in [
        ("header_offset", g.header_offset),
        ("footer_offset", g.footer_offset),
    ] {
        if !(-MAX_POINT_FIELD_PT..=MAX_POINT_FIELD_PT).contains(&v) {
            return Some(format!("{name} {v} is outside +-{MAX_POINT_FIELD_PT}"));
        }
    }
    if let Err(axis) = margins_leave_text_area(
        g.width,
        g.height,
        g.margin_top,
        g.margin_right,
        g.margin_bottom,
        g.margin_left,
    ) {
        return Some(format!(
            "margins {} leave less than {MIN_TEXT_AREA_PT} pt of text area on a {} x {} page",
            axis.fields(),
            g.width,
            g.height
        ));
    }
    if !column_gutter_in_bound(s.columns.gutter_pt) {
        return Some(format!(
            "column gutter {} is outside 0..={MAX_POINT_FIELD_PT}",
            s.columns.gutter_pt
        ));
    }
    None
}

fn scan_paragraph(para: &Paragraph, depth: u32) -> Option<String> {
    if let Some(why) = scan_props(&para.props) {
        return Some(why);
    }
    if let Some(why) = scan_props(&para.direct_overrides) {
        return Some(format!("direct override: {why}"));
    }
    if let Some(section) = &para.section_end
        && let Some(why) = scan_section(section)
    {
        return Some(format!("section break: {why}"));
    }
    for io in &para.inline_objects {
        if let crate::InlineKind::TextBox { story, .. } = &io.kind
            && depth < MAX_SCAN_DEPTH
            && let Some(why) = scan_blocks(story.body.iter(), depth + 1)
        {
            return Some(why);
        }
    }
    None
}

fn scan_blocks<'a>(blocks: impl IntoIterator<Item = &'a Block>, depth: u32) -> Option<String> {
    let mut found = None;
    crate::for_each_paragraph_nested(blocks, &mut |path, para| {
        if found.is_none() {
            found = scan_paragraph(para, depth).map(|why| format!("{path:?}: {why}"));
        }
    });
    found
}

/// The first page / paragraph point field in `doc` outside its Word bound,
/// described for a failure message; `None` when every one is inside. The
/// executable form of the rule for the `rpc_command` fuzz target: no
/// command may leave a page size, margin, column gutter, indent, spacing,
/// line height or tab stop Word cannot hold. Walks the body (tables at
/// every depth, text boxes), header / footer / note stories, every
/// section's geometry and the style table.
pub fn first_point_field_beyond_bound(doc: &DocumentTree) -> Option<String> {
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
    if let Some(why) = scan_blocks(doc.blocks.iter(), 0) {
        return Some(why);
    }
    let mut stories: Vec<(&String, &Vec<Block>)> =
        doc.headers.iter().chain(doc.footers.iter()).collect();
    stories.sort_by(|a, b| a.0.cmp(b.0));
    for (id, blocks) in stories {
        if let Some(why) = scan_blocks(blocks.iter(), 0) {
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
        if let Some(why) = scan_blocks(note.body.iter(), 0) {
            return Some(format!("note {id}: {why}"));
        }
    }
    /* Issue #503 (residue) - a border stroke's width is a paragraph / page
    measure in Word's eyes too; its scan lives in `border_bounds`. */
    crate::first_border_beyond_bound(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bounds_are_words() {
        assert_eq!(MAX_POINT_FIELD_PT, 1584.0);
        assert_eq!((MAX_POINT_FIELD_PT * 20.0) as i32, MAX_POINT_FIELD_TWIPS);
        assert_eq!((MIN_PAGE_SIDE_PT * 20.0).round() as i32, 144);
        assert_eq!(
            MAX_LINE_SPACING_MULTIPLE * 240.0,
            MAX_POINT_FIELD_TWIPS as f32
        );
    }

    #[test]
    fn membership_agrees_at_the_edges() {
        // (value, accepted as an unsigned field, accepted as a signed one)
        for (pt, unsigned, signed) in [
            (0.0, true, true),
            (-1.0, false, true),
            (-1584.0, false, true),
            (-1585.0, false, false),
            (1584.0, true, true),
            (1585.0, false, false),
            (1e33, false, false),
            (f32::MAX, false, false),
            (f32::MIN, false, false),
            (f32::NAN, false, false),
            (f32::INFINITY, false, false),
            (f32::NEG_INFINITY, false, false),
        ] {
            assert_eq!(margin_in_bound(pt), unsigned, "margin {pt}");
            assert_eq!(column_gutter_in_bound(pt), unsigned, "gutter {pt}");
            assert_eq!(indent_in_bound(pt), signed, "indent {pt}");
            assert_eq!(tab_position_in_bound(pt), signed, "tab {pt}");
        }
    }

    #[test]
    fn the_multiplier_clears_at_or_below_zero_and_is_bounded_above() {
        for (m, ok) in [
            (-5.0, true),
            (0.0, true),
            (0.01, false),
            (0.059, false),
            (0.06, true),
            (1.0, true),
            (132.0, true),
            (132.5, false),
            (1e33, false),
            (f32::MAX, false),
            (f32::NAN, false),
        ] {
            assert_eq!(line_spacing_multiple_in_bound(m), ok, "{m}");
        }
    }

    #[test]
    fn margins_must_leave_a_tenth_of_an_inch() {
        // US Letter, the stock 1 in margins.
        assert!(margins_leave_text_area(612.0, 792.0, 72.0, 72.0, 72.0, 72.0).is_ok());
        // Exactly 0.1 in left on the horizontal axis.
        assert!(margins_leave_text_area(612.0, 792.0, 0.0, 302.4, 0.0, 302.4).is_ok());
        // One point under that.
        assert_eq!(
            margins_leave_text_area(612.0, 792.0, 0.0, 303.0, 0.0, 303.0),
            Err(TextAreaAxis::Horizontal)
        );
        assert_eq!(
            margins_leave_text_area(612.0, 792.0, 400.0, 0.0, 400.0, 0.0),
            Err(TextAreaAxis::Vertical)
        );
        // A negative margin never adds room.
        assert_eq!(
            margins_leave_text_area(612.0, 792.0, 0.0, -50.0, 0.0, 700.0),
            Err(TextAreaAxis::Horizontal)
        );
    }

    #[test]
    fn the_reader_repair_leaves_a_tenth_of_an_inch_and_only_then() {
        let mut stock = PageGeometry::a4();
        assert!(!fit_margins_to_text_area(&mut stock));
        assert_eq!(stock, PageGeometry::a4());

        let mut g = PageGeometry::letter();
        g.margin_left = 1584.0;
        g.margin_right = 1584.0;
        g.margin_top = -36.0;
        g.margin_bottom = 1584.0;
        assert!(fit_margins_to_text_area(&mut g));
        assert!(
            margins_leave_text_area(
                g.width,
                g.height,
                g.margin_top,
                g.margin_right,
                g.margin_bottom,
                g.margin_left
            )
            .is_ok(),
            "{g:?}"
        );
        assert_eq!(g.margin_top, -36.0, "a negative margin is left alone");
        assert!(g.margin_left > 0.0 && g.margin_left == g.margin_right);
        // Idempotent: a repaired geometry needs no second repair.
        assert!(!fit_margins_to_text_area(&mut g));
    }

    #[test]
    fn the_scan_finds_a_paragraph_a_section_and_a_style_outside_the_bound() {
        let doc = DocumentTree::from_text("hello");
        assert_eq!(first_point_field_beyond_bound(&doc), None);

        let mut indent = DocumentTree::from_text("hello");
        indent.blocks[0]
            .as_paragraph_mut()
            .expect("paragraph")
            .props
            .indent
            .start_twips = MAX_POINT_FIELD_TWIPS + 1;
        let why = first_point_field_beyond_bound(&indent).expect("indent");
        assert!(why.contains("indent.start_twips"), "{why}");

        let mut tab = DocumentTree::from_text("hello");
        tab.blocks[0]
            .as_paragraph_mut()
            .expect("paragraph")
            .props
            .tab_stops
            .push(crate::TabStop {
                position_pt: 1e30,
                kind: crate::TabKind::Left,
                leader: crate::TabLeader::None,
            });
        assert!(first_point_field_beyond_bound(&tab).is_some());

        let mut section = DocumentTree::from_text("hello");
        section.body_section.geometry.margin_left = 1e30;
        let why = first_point_field_beyond_bound(&section).expect("margin");
        assert!(why.contains("margin_left"), "{why}");

        let mut swallowed = DocumentTree::from_text("hello");
        swallowed.body_section.geometry.margin_left = 400.0;
        swallowed.body_section.geometry.margin_right = 400.0;
        let why = first_point_field_beyond_bound(&swallowed).expect("text area");
        assert!(why.contains("text area"), "{why}");

        let mut gutter = DocumentTree::from_text("hello");
        gutter.body_section.columns.gutter_pt = 1585.0;
        assert!(first_point_field_beyond_bound(&gutter).is_some());
    }
}
