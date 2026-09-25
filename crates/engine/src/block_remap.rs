//! Issue #152 — the single choke point that keeps block-indexed side
//! tables in step with structural (block-level) edits.
//!
//! [`DocumentTree::comment_ranges`] addresses its endpoints by
//! [`LogicalPos`] — a block *path*, not a stable id — so any mutation
//! that inserts or removes blocks in front of an anchor must rewrite the
//! anchor or the comment drifts onto a different paragraph. Every block
//! insertion / removal in this crate (tables, TOC stubs + regeneration,
//! paragraph splits behind section breaks and TOC insertion) calls one
//! of the entry points here on the POST-mutation tree:
//!
//! - [`DocumentTree::remap_block_splice`] — `removed` blocks starting at
//!   `at` inside one container were replaced by `inserted` blocks
//!   ([`DocumentTree::remap_block_indices`] is the top-level `±delta`
//!   convenience).
//! - [`DocumentTree::remap_paragraph_split`] — one paragraph was split
//!   in two at a byte offset.
//!
//! Audit (issue #152): `comment_ranges` is the only block-indexed side
//! table on the tree. Bookmarks, revisions, hyperlinks, fields and note
//! references ride their paragraph inline; section boundaries derive
//! from `Paragraph::section_end`; TOC regions are derived by scanning.
//! A future side table keyed by block path must be remapped here too.
//!
//! Undo needs nothing: the side table lives on the immutable tree, so
//! every undo snapshot carries its own consistent copy.

use crate::{BlockPath, DocumentTree, LogicalPos, PathStep};

impl DocumentTree {
    /// Top-level convenience over [`Self::remap_block_splice`]: `delta > 0`
    /// = `delta` blocks were inserted in front of block `at`; `delta < 0`
    /// = `-delta` blocks starting at `at` were removed.
    pub fn remap_block_indices(&mut self, at: u32, delta: i64) {
        let magnitude = u32::try_from(delta.unsigned_abs()).unwrap_or(u32::MAX);
        if delta >= 0 {
            self.remap_block_splice(&[], at, 0, magnitude);
        } else {
            self.remap_block_splice(&[], at, magnitude, 0);
        }
    }

    /// `removed` blocks at `at..at + removed` of the container addressed
    /// by `container` (the path steps leading INTO a block list: empty =
    /// the body, `[Block(t), Cell{r,c}]` = a table cell) were replaced by
    /// `inserted` blocks. Must run on the post-mutation tree.
    ///
    /// Anchors before `at` are untouched; anchors after the replaced run
    /// shift by `inserted - removed`. An anchor INSIDE the replaced run
    /// maps onto the replacement block at the same relative index (the
    /// last one when the replacement is shorter; offset clamped to the
    /// new paragraph); with no replacement at all it collapses onto the
    /// block that now sits at `at` (offset 0), or the end of the block
    /// before it when the removal emptied the container's tail.
    pub fn remap_block_splice(
        &mut self,
        container: &[PathStep],
        at: u32,
        removed: u32,
        inserted: u32,
    ) {
        if (removed == 0 && inserted == 0) || self.comment_ranges.is_empty() {
            return;
        }
        let mut ranges = std::mem::take(&mut self.comment_ranges);
        for r in &mut ranges {
            r.start = self.splice_pos(&r.start, container, at, removed, inserted);
            r.end = self.splice_pos(&r.end, container, at, removed, inserted);
        }
        self.comment_ranges = ranges;
    }

    /// The paragraph at `para` was split at byte `offset` into a left
    /// half (still at `para`) and a right half inserted right after it.
    /// Must run on the post-mutation tree. Anchors in later blocks of the
    /// same container shift down by one; anchors inside the split
    /// paragraph past the split point move onto the right half. A range
    /// START at exactly the split point follows the text to the right
    /// half; a range END there stays at the end of the left half.
    pub fn remap_paragraph_split(&mut self, para: &BlockPath, offset: u32) {
        let Some((PathStep::Block(idx), container)) = para.steps.split_last() else {
            return;
        };
        if self.comment_ranges.is_empty() {
            return;
        }
        let idx = *idx;
        let mut ranges = std::mem::take(&mut self.comment_ranges);
        for r in &mut ranges {
            let collapsed = r.start == r.end;
            r.start = self.splice_pos(&r.start, container, idx + 1, 0, 1);
            r.end = self.splice_pos(&r.end, container, idx + 1, 0, 1);
            r.start = split_pos(&r.start, para, idx, offset, true);
            r.end = if collapsed {
                r.start.clone()
            } else {
                split_pos(&r.end, para, idx, offset, false)
            };
        }
        self.comment_ranges = ranges;
    }

    fn splice_pos(
        &self,
        pos: &LogicalPos,
        container: &[PathStep],
        at: u32,
        removed: u32,
        inserted: u32,
    ) -> LogicalPos {
        let d = container.len();
        if pos.path.steps.len() <= d || pos.path.steps[..d] != *container {
            return pos.clone();
        }
        let PathStep::Block(i) = pos.path.steps[d] else {
            return pos.clone();
        };
        if i < at {
            return pos.clone();
        }
        let end = at.saturating_add(removed);
        if i >= end {
            let mut out = pos.clone();
            out.path.steps[d] = PathStep::Block(i - removed + inserted);
            return out;
        }
        /* Inside the replaced run: the anchored block itself is gone. */
        let at_block = |n: u32| {
            let mut steps = container.to_vec();
            steps.push(PathStep::Block(n));
            BlockPath { steps }
        };
        let rel = i - at;
        if inserted > 0 {
            let path = at_block(at + rel.min(inserted - 1));
            let offset = if rel < inserted { pos.offset } else { u32::MAX };
            let offset = self
                .paragraph_at_path(&path)
                .map_or(0, |p| offset.min(p.text.len() as u32));
            return LogicalPos { path, offset };
        }
        let here = at_block(at);
        if self.block_at(&here).is_some() || at == 0 {
            return LogicalPos {
                path: here,
                offset: 0,
            };
        }
        let prev = at_block(at - 1);
        let offset = self
            .paragraph_at_path(&prev)
            .map_or(0, |p| p.text.len() as u32);
        LogicalPos { path: prev, offset }
    }
}

/// [`DocumentTree::remap_paragraph_split`] for one endpoint that already
/// had the later-block shift applied (the split paragraph keeps `idx`).
fn split_pos(pos: &LogicalPos, para: &BlockPath, idx: u32, at: u32, is_start: bool) -> LogicalPos {
    if pos.path != *para {
        return pos.clone();
    }
    let moves = if is_start {
        pos.offset >= at
    } else {
        pos.offset > at
    };
    if !moves {
        return pos.clone();
    }
    let mut out = pos.clone();
    if let Some(last) = out.path.steps.last_mut() {
        *last = PathStep::Block(idx + 1);
    }
    out.offset = pos.offset - at;
    out
}

#[cfg(test)]
mod tests {
    use crate::{Block, BlockPath, DocumentTree, LogicalPos, PathStep, SectionType, UndoStack};

    /// Three paragraphs; the comment covers "target" (all of block 2).
    fn commented_doc() -> DocumentTree {
        let doc = DocumentTree::from_paragraphs(["first".into(), "second".into(), "target".into()]);
        doc.insert_comment(
            LogicalPos::new(BlockPath::top(2), 0),
            LogicalPos::new(BlockPath::top(2), 6),
            "c".into(),
            "A".into(),
            "2026-01-01T00:00:00Z".into(),
        )
        .0
    }

    /// The text the (single) comment range covers — the invariant the
    /// issue's acceptance pins: the comment stays on ITS paragraph.
    fn commented_text(doc: &DocumentTree) -> String {
        let r = &doc.comment_ranges[0];
        doc.text_range(r.start.clone(), r.end.clone())
    }

    fn top(doc: &DocumentTree) -> (u32, u32, u32, u32) {
        let r = &doc.comment_ranges[0];
        let idx = |p: &LogicalPos| match p.path.steps.as_slice() {
            [PathStep::Block(i)] => *i,
            other => panic!("expected a top-level path, got {other:?}"),
        };
        (idx(&r.start), r.start.offset, idx(&r.end), r.end.offset)
    }

    #[test]
    fn insert_table_above_keeps_the_comment_on_its_paragraph() {
        let doc = commented_doc();
        let with_table = doc.insert_table(BlockPath::top(0), 2, 2);
        assert!(matches!(with_table.blocks[0], Block::Table(_)));
        assert_eq!(top(&with_table), (3, 0, 3, 6));
        assert_eq!(commented_text(&with_table), "target");
    }

    #[test]
    fn insert_table_with_escape_paragraph_shifts_by_two() {
        /* Inserting in front of a table needs a trailing paragraph too. */
        let doc = commented_doc().insert_table(BlockPath::top(0), 1, 1);
        let doc = doc.insert_table(BlockPath::top(0), 1, 1);
        assert!(matches!(doc.blocks[1], Block::Paragraph(_)));
        assert_eq!(top(&doc), (5, 0, 5, 6));
        assert_eq!(commented_text(&doc), "target");
    }

    #[test]
    fn insert_table_below_leaves_the_comment_alone() {
        let doc = commented_doc();
        let after = doc.insert_table(BlockPath::top(3), 1, 1);
        assert_eq!(top(&after), (2, 0, 2, 6));
    }

    #[test]
    fn delete_table_above_shifts_back_and_round_trips() {
        let doc = commented_doc();
        let with_table = doc.insert_table(BlockPath::top(1), 2, 2);
        assert_eq!(top(&with_table), (3, 0, 3, 6));
        let back = with_table.delete_table(BlockPath::top(1));
        assert_eq!(top(&back), (2, 0, 2, 6));
        assert_eq!(commented_text(&back), "target");
        /* A no-op delete (not a table) must not shift anything. */
        let noop = back.delete_table(BlockPath::top(0));
        assert_eq!(top(&noop), (2, 0, 2, 6));
    }

    #[test]
    fn deleting_a_table_that_holds_an_anchor_collapses_onto_the_next_block() {
        let doc = DocumentTree::from_paragraphs(["a".into(), "b".into()]).insert_table(
            BlockPath::top(1),
            1,
            1,
        );
        let cell_para = BlockPath {
            steps: vec![
                PathStep::Block(1),
                PathStep::Cell { row: 0, col: 0 },
                PathStep::Block(0),
            ],
        };
        let (doc, _) = doc.insert_comment(
            LogicalPos::new(BlockPath::top(0), 0),
            LogicalPos::new(cell_para, 0),
            "c".into(),
            "A".into(),
            "d".into(),
        );
        let doc = doc.delete_table(BlockPath::top(1));
        let r = &doc.comment_ranges[0];
        assert_eq!(r.start, LogicalPos::new(BlockPath::top(0), 0));
        assert_eq!(r.end, LogicalPos::new(BlockPath::top(1), 0));
    }

    #[test]
    fn page_break_above_does_not_move_the_comment() {
        /* `InsertPageBreak` flips `page_break_before` — no block moves. */
        let doc =
            commented_doc().set_page_break_before(LogicalPos::new(BlockPath::top(1), 0), true);
        assert_eq!(top(&doc), (2, 0, 2, 6));
        assert_eq!(commented_text(&doc), "target");
    }

    #[test]
    fn section_break_above_keeps_the_comment_on_its_paragraph() {
        let doc = commented_doc();
        let split = doc
            .insert_section_break_at(LogicalPos::new(BlockPath::top(0), 2), SectionType::NextPage);
        assert_eq!(split.blocks.len(), 4);
        assert_eq!(top(&split), (3, 0, 3, 6));
        assert_eq!(commented_text(&split), "target");
    }

    #[test]
    fn splitting_the_commented_paragraph_carries_the_tail_anchor() {
        let doc = DocumentTree::from_text("hello world");
        let (doc, _) = doc.insert_comment(
            LogicalPos::new(BlockPath::top(0), 2),
            LogicalPos::new(BlockPath::top(0), 9),
            "c".into(),
            "A".into(),
            "d".into(),
        );
        let split = doc.split_paragraph(LogicalPos::new(BlockPath::top(0), 6));
        assert_eq!(top(&split), (0, 2, 1, 3));
        /* Start exactly at the split point follows the right half. */
        let (doc2, _) = DocumentTree::from_text("hello world").insert_comment(
            LogicalPos::new(BlockPath::top(0), 6),
            LogicalPos::new(BlockPath::top(0), 11),
            "c".into(),
            "A".into(),
            "d".into(),
        );
        let split = doc2.split_paragraph(LogicalPos::new(BlockPath::top(0), 6));
        assert_eq!(top(&split), (1, 0, 1, 5));
        assert_eq!(commented_text(&split), "world");
    }

    #[test]
    fn undo_restores_the_pre_insertion_anchor() {
        let doc = commented_doc();
        let mut undo = UndoStack::new(doc.clone(), 100);
        undo.push(doc.insert_table(BlockPath::top(0), 1, 1));
        assert_eq!(top(undo.current()), (3, 0, 3, 6));
        assert!(undo.undo());
        assert_eq!(top(undo.current()), (2, 0, 2, 6));
        assert_eq!(commented_text(undo.current()), "target");
    }

    #[test]
    fn splice_remaps_anchors_inside_a_replaced_run() {
        let mut doc =
            DocumentTree::from_paragraphs(["a".into(), "bb".into(), "cc".into(), "d".into()]);
        doc = doc
            .insert_comment(
                LogicalPos::new(BlockPath::top(2), 1),
                LogicalPos::new(BlockPath::top(3), 1),
                "c".into(),
                "A".into(),
                "d".into(),
            )
            .0;
        /* Replace blocks 1..=2 with a single one-byte paragraph. */
        let mut blocks = doc.blocks.clone();
        blocks.remove(1);
        blocks.remove(1);
        blocks.insert(1, doc.blocks[0].clone());
        doc.blocks = blocks;
        doc.remap_block_splice(&[], 1, 2, 1);
        let r = &doc.comment_ranges[0];
        assert_eq!(r.start, LogicalPos::new(BlockPath::top(1), 1));
        assert_eq!(r.end, LogicalPos::new(BlockPath::top(2), 1));
    }
}
