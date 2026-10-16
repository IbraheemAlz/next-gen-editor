//! Issue #501 - Word's font-size bound.
//!
//! Word never sets a character larger than 1638 pt (`<w:sz w:val="3276"/>`,
//! half-points) nor smaller than 1 pt. Until this module every door
//! accepted any *finite* size: `ApplyFormatting` took `1.0951575e33`, and
//! a line taller than its page pushed the paginator's running height past
//! every finite cull target (the #499 stall). This is the model's side of
//! the bound, shared by every door that sets a run / mark / style size:
//!
//! - the command path (`ApplyFormatting`, `ModifyStyle` - engine-wasm
//!   `font_size_guard`) REFUSES a size outside `1..=1638` with
//!   `ErrorKind::OutOfRange`;
//! - the `.docx` reader (`<w:sz>` / `<w:szCs>`) and the HTML paste door
//!   (`font-size:Npx`) CLAMP into it - a document or a clipboard is not
//!   the user's to re-type - the reader with `DocxWarning::MeasureClamped`;
//! - the `.docx` writer clamps what it spells, so a size that bypassed a
//!   door cannot become an out-of-schema `w:val`.
//!
//! One number, so "too large" means the same thing everywhere.

/// Word's largest font size, in points (`w:val="3276"` half-points).
pub const MAX_FONT_SIZE_PT: f32 = 1638.0;

/// Word's smallest font size, in points.
pub const MIN_FONT_SIZE_PT: f32 = 1.0;

/// `true` for a size Word can hold: finite and inside
/// [`MIN_FONT_SIZE_PT`]`..=`[`MAX_FONT_SIZE_PT`].
pub fn font_size_in_bound(pt: f32) -> bool {
    (MIN_FONT_SIZE_PT..=MAX_FONT_SIZE_PT).contains(&pt)
}

/// A font size inside the bound. A NaN (which `f32::clamp` would pass
/// through) becomes the minimum; callers that can tell "unusable" from
/// "too small" drop a non-finite value instead.
pub fn clamp_font_size_pt(pt: f32) -> f32 {
    if pt.is_nan() {
        MIN_FONT_SIZE_PT
    } else {
        pt.clamp(MIN_FONT_SIZE_PT, MAX_FONT_SIZE_PT)
    }
}

/// Text boxes nest at most this deep before the layout flattens them; the
/// scan stops at the same kind of cap rather than trusting the tree.
const MAX_SCAN_DEPTH: u32 = 32;

/// The first run, paragraph-mark, tracked-format or style size in `doc`
/// outside [`MIN_FONT_SIZE_PT`]`..=`[`MAX_FONT_SIZE_PT`], described for a
/// failure message; `None` when every size is inside it. The executable
/// form of the rule for the `rpc_command` and `snapshot_decode` fuzz
/// targets: no command may leave - and no restored snapshot may hold - a
/// size Word cannot hold. Walks the body, table cells at every depth,
/// text-box stories, header / footer / note stories, the style table and
/// the document run defaults (issue #503: what `DocumentTree::
/// repair_bounds` repairs, so "restore, then the rule holds"); an unset
/// size is the document default, not checked.
pub fn first_font_size_beyond_bound(doc: &crate::DocumentTree) -> Option<String> {
    if let Some(why) = scan_style(&doc.style_run_defaults) {
        return Some(format!("document run defaults: {why}"));
    }
    let mut style_ids: Vec<&String> = doc.styles.keys().collect();
    style_ids.sort();
    for id in style_ids {
        if let Some(why) = doc.styles.get(id).and_then(|s| scan_style(&s.run)) {
            return Some(format!("style {id}: {why}"));
        }
    }
    if let Some(why) = scan_blocks(doc.blocks.iter(), 0) {
        return Some(why);
    }
    let mut stories: Vec<(&String, &Vec<crate::Block>)> =
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
    None
}

fn scan_blocks<'a>(
    blocks: impl IntoIterator<Item = &'a crate::Block>,
    depth: u32,
) -> Option<String> {
    let mut found = None;
    crate::for_each_paragraph_nested(blocks, &mut |path, para| {
        if found.is_none() {
            found = scan_paragraph(para, depth).map(|why| format!("{path:?}: {why}"));
        }
    });
    found
}

fn scan_style(style: &crate::SpanStyle) -> Option<String> {
    for (name, size) in [
        ("font_size", style.font_size),
        ("font_size_cs", style.font_size_cs),
    ] {
        if let Some(pt) = size
            && !font_size_in_bound(pt)
        {
            return Some(format!(
                "{name} {pt} is outside {MIN_FONT_SIZE_PT}..={MAX_FONT_SIZE_PT}"
            ));
        }
    }
    None
}

fn scan_paragraph(para: &crate::Paragraph, depth: u32) -> Option<String> {
    for run in &para.spans {
        if let Some(why) = scan_style(&run.style) {
            return Some(format!("run {}..{}: {why}", run.start, run.end));
        }
    }
    if let Some(why) = para.mark_style.as_deref().and_then(scan_style) {
        return Some(format!("paragraph mark: {why}"));
    }
    for rev in &para.revisions {
        if let Some(why) = rev.prev_attrs.as_ref().and_then(scan_style) {
            return Some(format!("tracked format change: {why}"));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bound_is_word_s() {
        assert_eq!(MAX_FONT_SIZE_PT, 1638.0);
        assert_eq!((MAX_FONT_SIZE_PT * 2.0) as u32, 3276);
        assert_eq!(MIN_FONT_SIZE_PT, 1.0);
    }

    #[test]
    fn the_scan_finds_a_run_or_a_mark_outside_the_bound() {
        use crate::{DocumentTree, SpanStyle, StyleRun};
        let sized = |pt: f32| SpanStyle {
            font_size: Some(pt),
            ..Default::default()
        };
        let doc_with = |run: Option<f32>, mark: Option<f32>| {
            let mut doc = DocumentTree::from_text("hello");
            let p = doc.blocks[0].as_paragraph_mut().expect("paragraph");
            if let Some(pt) = run {
                p.spans.push(StyleRun {
                    start: 0,
                    end: 5,
                    style: sized(pt),
                });
            }
            p.mark_style = mark.map(|pt| Box::new(sized(pt)));
            doc
        };
        assert_eq!(first_font_size_beyond_bound(&doc_with(None, None)), None);
        assert_eq!(
            first_font_size_beyond_bound(&doc_with(Some(1638.0), Some(1.0))),
            None
        );
        let run = first_font_size_beyond_bound(&doc_with(Some(1638.5), None)).expect("run");
        assert!(run.contains("run 0..5") && run.contains("1638.5"), "{run}");
        let mark = first_font_size_beyond_bound(&doc_with(None, Some(0.5))).expect("mark");
        assert!(mark.contains("paragraph mark"), "{mark}");
    }

    #[test]
    fn membership_and_clamp_agree_at_the_edges() {
        for (pt, inside) in [
            (0.0, false),
            (0.5, false),
            (1.0, true),
            (11.0, true),
            (1638.0, true),
            (1638.5, false),
            (1e33, false),
            (-1.0, false),
            (f32::NAN, false),
            (f32::INFINITY, false),
            (f32::NEG_INFINITY, false),
        ] {
            assert_eq!(font_size_in_bound(pt), inside, "{pt}");
            assert!(font_size_in_bound(clamp_font_size_pt(pt)), "{pt}");
        }
        assert_eq!(clamp_font_size_pt(0.0), 1.0);
        assert_eq!(clamp_font_size_pt(1e33), 1638.0);
        assert_eq!(clamp_font_size_pt(f32::INFINITY), 1638.0);
        assert_eq!(clamp_font_size_pt(f32::NEG_INFINITY), 1.0);
        assert_eq!(clamp_font_size_pt(f32::NAN), 1.0);
        assert_eq!(clamp_font_size_pt(10.5), 10.5);
    }
}
