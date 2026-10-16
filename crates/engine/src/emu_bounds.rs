//! Issue #459 - the 22-inch bound on drawing extents and offsets.
//!
//! Word never lays out a shape larger than 22 in (20 116 800 EMU - DrawingML's
//! `ST_LineWidth` maximum), and since issue #407 the `.docx` reader clamps
//! every extent / offset it reads to that bound. This module is the model's
//! side of the same rule, shared by the reader (`format_docx::schema::
//! measure::MAX_EMU`), the command path (`Command::ResizeImage` /
//! `MoveImage`, `InsertImage`, `InsertTextBox` - engine-wasm) and the HTML
//! paste door, so one number decides what "too large" means.
//!
//! [`first_image_beyond_bound`] is the executable form of the rule for the
//! `rpc_command` fuzz target: no picture the document holds - body, table
//! cell, text box story at any depth - may carry an extent or a fixed
//! offset past the bound.

use crate::{Block, DocumentTree, FloatOffset, InlineKind, Paragraph, for_each_paragraph_nested};

/// Word's largest shape extent / offset: 22 in = 22 x 914 400 EMU
/// (20 116 800).
pub const MAX_DRAWING_EMU: i64 = 22 * 914_400;

/// A picture extent: at least 1 EMU (a zero-sized box has no handles and no
/// pixels), at most [`MAX_DRAWING_EMU`].
pub fn clamp_extent_emu(emu: i64) -> i64 {
    emu.clamp(1, MAX_DRAWING_EMU)
}

/// A fixed picture offset: within +-[`MAX_DRAWING_EMU`].
pub fn clamp_offset_emu(emu: i64) -> i64 {
    emu.clamp(-MAX_DRAWING_EMU, MAX_DRAWING_EMU)
}

/// Scale a natural `width x height` (EMU) down, keeping its aspect ratio,
/// until neither side passes [`MAX_DRAWING_EMU`]; a size already inside the
/// bound comes back unchanged. Each side stays at least 1 EMU when it was
/// positive.
pub fn fit_extent_within_bound(width: i64, height: i64) -> (i64, i64) {
    let longest = width.max(height);
    if longest <= MAX_DRAWING_EMU {
        return (width, height);
    }
    let scale = |side: i64| {
        if side <= 0 {
            side
        } else {
            ((i128::from(side) * i128::from(MAX_DRAWING_EMU)) / i128::from(longest)).max(1) as i64
        }
    };
    (scale(width), scale(height))
}

/// Text boxes nest at most this deep before the layout flattens them; the
/// scan below stops at the same kind of cap rather than trusting the tree.
const MAX_SCAN_DEPTH: u32 = 32;

/// The first picture in `doc` whose extent leaves `0..=`[`MAX_DRAWING_EMU`]
/// or whose fixed (`FloatOffset::Emu`) offset leaves +-the bound, described
/// for a failure message; `None` when every picture is inside it. Walks the
/// body, table cells at every depth and text-box stories.
pub fn first_image_beyond_bound(doc: &DocumentTree) -> Option<String> {
    scan_blocks(doc.blocks.iter(), 0)
}

fn scan_blocks<'a>(blocks: impl IntoIterator<Item = &'a Block>, depth: u32) -> Option<String> {
    let mut found = None;
    for_each_paragraph_nested(blocks, &mut |path, para| {
        if found.is_none() {
            found = scan_paragraph(para, depth).map(|why| format!("{path:?}: {why}"));
        }
    });
    found
}

fn scan_paragraph(para: &Paragraph, depth: u32) -> Option<String> {
    for io in &para.inline_objects {
        match &io.kind {
            InlineKind::Image {
                width_emu,
                height_emu,
                ..
            } => {
                for (name, v) in [("width_emu", *width_emu), ("height_emu", *height_emu)] {
                    if !(0..=MAX_DRAWING_EMU).contains(&v) {
                        return Some(format!("image {name} {v} is outside 0..={MAX_DRAWING_EMU}"));
                    }
                }
                if let Some(anchor) = io.anchor.as_deref() {
                    for (name, offset) in [
                        ("position_h", anchor.position_h.offset),
                        ("position_v", anchor.position_v.offset),
                    ] {
                        if let FloatOffset::Emu(v) = offset
                            && v.unsigned_abs() > MAX_DRAWING_EMU as u64
                        {
                            return Some(format!(
                                "image {name} offset {v} is outside +-{MAX_DRAWING_EMU}"
                            ));
                        }
                    }
                }
            }
            InlineKind::TextBox { story, .. } if depth < MAX_SCAN_DEPTH => {
                if let Some(why) = scan_blocks(story.body.iter(), depth + 1) {
                    return Some(why);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bound_is_22_inches() {
        assert_eq!(MAX_DRAWING_EMU, 20_116_800);
    }

    #[test]
    fn clamps_hold_at_the_edges() {
        assert_eq!(clamp_extent_emu(i64::MIN), 1);
        assert_eq!(clamp_extent_emu(0), 1);
        assert_eq!(clamp_extent_emu(MAX_DRAWING_EMU), MAX_DRAWING_EMU);
        assert_eq!(clamp_extent_emu(MAX_DRAWING_EMU + 1), MAX_DRAWING_EMU);
        assert_eq!(clamp_extent_emu(i64::MAX), MAX_DRAWING_EMU);
        assert_eq!(clamp_offset_emu(i64::MIN), -MAX_DRAWING_EMU);
        assert_eq!(clamp_offset_emu(-MAX_DRAWING_EMU - 1), -MAX_DRAWING_EMU);
        assert_eq!(clamp_offset_emu(-MAX_DRAWING_EMU), -MAX_DRAWING_EMU);
        assert_eq!(clamp_offset_emu(0), 0);
        assert_eq!(clamp_offset_emu(i64::MAX), MAX_DRAWING_EMU);
    }

    #[test]
    fn fitting_keeps_the_aspect_ratio_and_leaves_small_sizes_alone() {
        assert_eq!(
            fit_extent_within_bound(914_400, 457_200),
            (914_400, 457_200)
        );
        assert_eq!(
            fit_extent_within_bound(MAX_DRAWING_EMU, 1),
            (MAX_DRAWING_EMU, 1)
        );
        /* 2x the bound wide, 1x tall: both halve. */
        let (w, h) = fit_extent_within_bound(2 * MAX_DRAWING_EMU, MAX_DRAWING_EMU);
        assert_eq!((w, h), (MAX_DRAWING_EMU, MAX_DRAWING_EMU / 2));
        /* Extreme aspect: the short side stays a visible 1 EMU, never 0. */
        let (w, h) = fit_extent_within_bound(i64::MAX, 3);
        assert_eq!((w, h), (MAX_DRAWING_EMU, 1));
        let (w, h) = fit_extent_within_bound(0, 0);
        assert_eq!((w, h), (0, 0));
    }
}
