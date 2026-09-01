//! Issue #77 — geometry fingerprint for regression pins.
//!
//! [`geometry_fingerprint`] folds every page's size, every laid-out
//! paragraph's origin/size, every line's origin/height/width and glyph
//! count (bands included) into one `u64`. Tests that touch the layout
//! pipeline pin a field-free document's fingerprint before and after a
//! change so a refactor that is supposed to be geometry-neutral can be
//! shown to be. Floats are folded via their bit patterns, so two runs
//! agree only when the geometry is bit-identical — exactly the
//! sensitivity a pin needs.

use crate::boxes::{LayoutBlock, PageBox, ParagraphBox, for_each_paragraph_in_blocks};
use std::hash::{Hash, Hasher};

fn fold_f32(h: &mut impl Hasher, v: f32) {
    v.to_bits().hash(h);
}

fn fold_paragraph(h: &mut impl Hasher, p: &ParagraphBox) {
    fold_f32(h, p.origin.x);
    fold_f32(h, p.origin.y);
    fold_f32(h, p.size.width);
    fold_f32(h, p.size.height);
    (p.lines.len() as u64).hash(h);
    for line in &p.lines {
        fold_f32(h, line.origin.x);
        fold_f32(h, line.origin.y);
        fold_f32(h, line.height);
        fold_f32(h, line.width);
        fold_f32(h, line.baseline);
        let glyphs: u64 = line.runs.iter().map(|r| r.glyphs.len() as u64).sum();
        glyphs.hash(h);
    }
}

fn fold_blocks(h: &mut impl Hasher, blocks: &[LayoutBlock]) {
    (blocks.len() as u64).hash(h);
    for_each_paragraph_in_blocks(blocks, &mut |p| fold_paragraph(h, p));
}

/// Fingerprint of a paginated document (see the module docs).
pub fn geometry_fingerprint(pages: &[PageBox]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (pages.len() as u64).hash(&mut h);
    for page in pages {
        fold_f32(&mut h, page.size.width);
        fold_f32(&mut h, page.size.height);
        page.page_number.hash(&mut h);
        fold_blocks(&mut h, &page.blocks);
        for band in [page.header.as_ref(), page.footer.as_ref()] {
            match band {
                Some(b) => {
                    1u8.hash(&mut h);
                    fold_blocks(&mut h, &b.blocks);
                }
                None => 0u8.hash(&mut h),
            }
        }
    }
    h.finish()
}
