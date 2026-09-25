//! Issues #252 / #250 — the single choke point that keeps the two
//! byte-offset tables of a paragraph edit in step with its text:
//!
//! - [`DocumentTree::comment_ranges`] — tree-level comment anchors,
//!   addressed by `(block path, byte offset)`;
//! - [`Paragraph::source_markup`] — the source run boundaries and
//!   positioned markers of a paragraph read from `.docx` (issue #199).
//!
//! Every in-place paragraph text change goes through
//! [`Paragraph::splice_text`], which rewrites the text AND remaps the
//! source markup, and returns the [`TextEdit`] it performed. The tree-level
//! caller hands that SAME record to [`DocumentTree::remap_text_edit`], so
//! the comment anchors see exactly the edit the markup saw — the two
//! tables cannot disagree. Paragraph primitives that rebuild a paragraph
//! (`delete_text`, `split_at`, `concat`) remap the markup themselves; the
//! tree-level edits built on them call the matching remap here
//! ([`DocumentTree::remap_text_edit`], [`DocumentTree::remap_paragraph_merge`])
//! or in `block_remap` ([`DocumentTree::remap_paragraph_split`],
//! [`DocumentTree::remap_block_splice`]).
//!
//! Routed through here (issue #252 audit): `insert_text` (and everything
//! built on it — `tracked_insert_text`, `insert_field_at`,
//! `insert_multiline`, the IME commit, `ReplaceRange` find/replace),
//! `delete_range` (single paragraph and paragraph merges),
//! `tracked_delete_range` (removing the reviewer's own pending insertion),
//! the inline-object splices (`insert_note_at`, `insert_inline_image_at`,
//! `insert_text_box_at`), revision accept/reject, `insert_rich` /
//! `insert_rich_blocks` (HTML paste) and the field restamp
//! (`restamp_fields`, F9 / save).
//!
//! Undo needs nothing: both tables live on the immutable tree, so every
//! undo snapshot carries its own consistent copy.

use crate::{
    BlockPath, DocumentTree, LogicalPos, Paragraph, PathStep, STALE_TEXT_LEN, SourceMarkup,
};

/// One in-place replacement of paragraph bytes `[at, at + removed)` by
/// `inserted` bytes (pure insertion: `removed == 0`; pure deletion:
/// `inserted == 0`). Offsets are pre-edit and char-boundary snapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextEdit {
    pub at: u32,
    pub removed: u32,
    pub inserted: u32,
}

impl Paragraph {
    /// Replace bytes `[at, at + removed)` of `text` with `inserted` and keep
    /// [`Paragraph::source_markup`] in step. Both ends are clamped to the
    /// text and snapped down to char boundaries. Style spans, fields,
    /// inline objects, hyperlinks and revisions are the caller's business
    /// (each edit path has its own travel rules for them); the returned
    /// [`TextEdit`] is what the caller feeds to
    /// [`DocumentTree::remap_text_edit`].
    pub fn splice_text(&mut self, at: u32, removed: u32, inserted: &str) -> TextEdit {
        let old_len = self.text.len() as u32;
        let s = self.snap_offset(at.min(old_len));
        let e = self
            .snap_offset(at.saturating_add(removed).min(old_len))
            .max(s);
        self.text.replace_range(s as usize..e as usize, inserted);
        let edit = TextEdit {
            at: s,
            removed: e - s,
            inserted: inserted.len() as u32,
        };
        SourceMarkup::note_replace(
            &mut self.source_markup,
            old_len,
            edit.at,
            edit.at + edit.removed,
            edit.inserted,
        );
        edit
    }
}

impl SourceMarkup {
    /// Bytes `[s, e)` of a text that was `old_len` bytes long were replaced
    /// by `rep_len` bytes. Pure insertion / deletion defer to
    /// [`Self::note_insert`] / [`Self::note_delete`] (their travel rules);
    /// a true replacement keeps a run covering the replaced bytes on the
    /// replacement (a run ENDING at `s` does not grow over it) and
    /// collapses markers inside the gap to `s`.
    pub fn note_replace(slot: &mut Option<Box<Self>>, old_len: u32, s: u32, e: u32, rep_len: u32) {
        if s >= e {
            Self::note_insert(slot, old_len, s, rep_len);
            return;
        }
        if rep_len == 0 {
            Self::note_delete(slot, old_len, s, e);
            return;
        }
        let Some(m) = slot.as_deref_mut() else {
            return;
        };
        debug_assert_in_step(m, old_len);
        if m.text_len != old_len {
            m.text_len = STALE_TEXT_LEN;
            return;
        }
        let shift = |p: u32| p - (e - s) + rep_len;
        for r in &mut m.runs {
            r.start = if r.start <= s {
                r.start
            } else if r.start >= e {
                shift(r.start)
            } else {
                s
            };
            r.end = if r.end <= s {
                r.end
            } else if r.end >= e {
                shift(r.end)
            } else {
                s + rep_len
            };
        }
        m.runs.retain(|r| r.start < r.end);
        for mk in &mut m.markers {
            mk.at = if mk.at <= s {
                mk.at
            } else if mk.at >= e {
                shift(mk.at)
            } else {
                s
            };
        }
        m.text_len = old_len - (e - s) + rep_len;
    }
}

/// Issue #250 — test-only debug assertion: a remapped edit must find the
/// markup either in step with the pre-edit text or already marked stale.
/// Anything else means an earlier text edit bypassed the remap (the
/// release build silently goes stale; tests fail loudly).
#[cfg(any(test, feature = "markup-assert"))]
pub(crate) fn debug_assert_in_step(m: &SourceMarkup, old_len: u32) {
    assert!(
        m.text_len == old_len || m.text_len == STALE_TEXT_LEN,
        "issue #250: source markup out of step before a remapped edit \
         (markup synced to {} bytes, text was {old_len}) — a text edit \
         bypassed Paragraph::splice_text",
        m.text_len
    );
}

#[cfg(not(any(test, feature = "markup-assert")))]
#[inline(always)]
pub(crate) fn debug_assert_in_step(_m: &SourceMarkup, _old_len: u32) {}

/// Issue #250 — test-only debug assertion over a whole tree: every
/// paragraph (cells included) carrying source markup is in step with its
/// text or explicitly stale. Run by `UndoStack::push` in test builds, so
/// every mutation any test commits is checked.
#[cfg(any(test, feature = "markup-assert"))]
pub(crate) fn debug_assert_tree_in_step(doc: &DocumentTree) {
    crate::fields::for_each_paragraph_deep(&doc.blocks, &mut |path, p| {
        if let Some(m) = p.source_markup.as_deref() {
            assert!(
                m.offsets_valid(p.text.len()) || m.text_len == STALE_TEXT_LEN,
                "issue #250: paragraph {path:?} carries source markup synced to {} \
                 bytes but its text is {} bytes — a text edit bypassed \
                 Paragraph::splice_text",
                m.text_len,
                p.text.len()
            );
        }
    });
}

#[cfg(not(any(test, feature = "markup-assert")))]
#[inline(always)]
pub(crate) fn debug_assert_tree_in_step(_doc: &DocumentTree) {}

impl DocumentTree {
    /// Issue #252 — the paragraph at `para` had bytes `[at, at + removed)`
    /// replaced by `inserted` bytes (a [`TextEdit`]). Remaps every comment
    /// anchor inside that paragraph; anchors elsewhere are untouched.
    ///
    /// - Pure insertion at `at`: a range START at `at` follows the text
    ///   (typing right before a comment stays outside it); a range END at
    ///   `at` stays (typing right after a comment stays outside it).
    ///   Anchors past `at` shift right.
    /// - Deletion / replacement: anchors before `at` are unchanged,
    ///   anchors at or past the gap's end shift by the length delta; a
    ///   START inside the gap clamps to `at`, an END inside clamps to the
    ///   end of the replacement (a comment reaching into replaced text
    ///   covers the replacement).
    /// - A collapsed (point) anchor follows the START rule so it stays
    ///   collapsed.
    pub fn remap_text_edit(&mut self, para: &BlockPath, at: u32, removed: u32, inserted: u32) {
        if (removed == 0 && inserted == 0) || self.comment_ranges.is_empty() {
            return;
        }
        let end = at.saturating_add(removed);
        let shift = |o: u32| o - removed + inserted;
        let map_start = |o: u32| {
            if o < at {
                o
            } else if removed == 0 || o >= end {
                shift(o)
            } else {
                at
            }
        };
        let map_end = |o: u32| {
            if o <= at {
                o
            } else if o >= end {
                shift(o)
            } else {
                at + inserted
            }
        };
        for r in &mut self.comment_ranges {
            let collapsed = r.start == r.end;
            if r.start.path == *para {
                r.start.offset = map_start(r.start.offset);
            }
            if collapsed {
                r.end = r.start.clone();
            } else if r.end.path == *para {
                r.end.offset = map_end(r.end.offset);
                if r.start.path == *para && r.end.offset < r.start.offset {
                    r.end.offset = r.start.offset;
                }
            }
        }
    }

    /// [`Self::remap_text_edit`] for a [`TextEdit`] record.
    pub fn remap_text_edit_record(&mut self, para: &BlockPath, edit: TextEdit) {
        self.remap_text_edit(para, edit.at, edit.removed, edit.inserted);
    }

    /// Issue #252 — a cross-paragraph deletion `[(start, s), (end block
    /// ep, e))` inside one container: the paragraph at `start` now holds
    /// its own `[0, s)` followed by the old `ep` paragraph's `[e, ..)`, and
    /// blocks `start + 1 ..= ep` are gone. Must run on the POST-mutation
    /// tree. Anchors in the deleted span (the head's tail, whole middle
    /// blocks — tables included — and the end paragraph's head) collapse
    /// onto the merge point; anchors in the end paragraph's surviving tail
    /// move onto the merged paragraph; later blocks shift up.
    pub fn remap_paragraph_merge(&mut self, start: &BlockPath, s: u32, ep: u32, e: u32) {
        let Some((PathStep::Block(sp), container)) = start.steps.split_last() else {
            return;
        };
        let sp = *sp;
        if ep <= sp || self.comment_ranges.is_empty() {
            return;
        }
        let d = container.len();
        let map = |pos: &LogicalPos| -> LogicalPos {
            if pos.path.steps.len() <= d || pos.path.steps[..d] != *container {
                return pos.clone();
            }
            let PathStep::Block(i) = pos.path.steps[d] else {
                return pos.clone();
            };
            let direct = pos.path.steps.len() == d + 1;
            let merged = |offset: u32| LogicalPos {
                path: start.clone(),
                offset,
            };
            if i < sp {
                pos.clone()
            } else if i > ep {
                let mut out = pos.clone();
                out.path.steps[d] = PathStep::Block(i - (ep - sp));
                out
            } else if i == sp && direct {
                merged(pos.offset.min(s))
            } else if i == ep && direct && pos.offset > e {
                merged(s + (pos.offset - e))
            } else {
                merged(s)
            }
        };
        for r in &mut self.comment_ranges {
            r.start = map(&r.start);
            r.end = map(&r.end);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Block, BlockPath, DocumentTree, ImageBlob, LogicalPos, NoteKind, Paragraph, PathStep,
        Revision, RevisionKind, SourceMarkup, SourceRun, UndoStack,
    };

    fn pos(block: u32, offset: u32) -> LogicalPos {
        LogicalPos::new(BlockPath::top(block), offset)
    }

    /// "alpha beta gamma" in block 0, "second" in block 1, "third" in
    /// block 2; the comment covers "beta" (bytes 6..10 of block 0).
    fn doc() -> DocumentTree {
        let d = DocumentTree::from_paragraphs([
            "alpha beta gamma".into(),
            "second".into(),
            "third".into(),
        ]);
        commented(d, pos(0, 6), pos(0, 10))
    }

    fn commented(d: DocumentTree, s: LogicalPos, e: LogicalPos) -> DocumentTree {
        d.insert_comment(s, e, "c".into(), "A".into(), "2026-01-01T00:00:00Z".into())
            .0
    }

    fn covered(d: &DocumentTree) -> String {
        let r = &d.comment_ranges[0];
        d.text_range(r.start.clone(), r.end.clone())
    }

    fn anchors(d: &DocumentTree) -> ((Vec<PathStep>, u32), (Vec<PathStep>, u32)) {
        let r = &d.comment_ranges[0];
        (
            (r.start.path.steps.clone(), r.start.offset),
            (r.end.path.steps.clone(), r.end.offset),
        )
    }

    /// Give every top-level paragraph one source run over its whole text,
    /// so the #250 assertion (`UndoStack::push`) has markup to check.
    fn with_markup(mut d: DocumentTree) -> DocumentTree {
        let mut blocks = d.blocks.clone();
        for b in blocks.iter_mut() {
            if let Block::Paragraph(p) = b {
                let len = p.text.len() as u32;
                p.source_markup = Some(Box::new(SourceMarkup {
                    text_len: len,
                    runs: vec![SourceRun {
                        start: 0,
                        end: len,
                        ..SourceRun::default()
                    }],
                    ..SourceMarkup::default()
                }));
            }
        }
        d.blocks = blocks;
        d
    }

    fn in_step(p: &Paragraph) -> bool {
        p.source_markup
            .as_deref()
            .is_some_and(|m| m.offsets_valid(p.text.len()))
    }

    /* ---- insert_text: before / inside / after ---- */

    #[test]
    fn typing_before_a_comment_shifts_it() {
        let d = doc().insert_text(pos(0, 0), ">> ");
        assert_eq!(covered(&d), "beta");
        /* Exactly at the start: the typed text stays outside. */
        let d = doc().insert_text(pos(0, 6), "X");
        assert_eq!(covered(&d), "beta");
    }

    #[test]
    fn typing_inside_a_comment_grows_it() {
        let d = doc().insert_text(pos(0, 8), "--");
        assert_eq!(covered(&d), "be--ta");
    }

    #[test]
    fn typing_after_a_comment_leaves_it() {
        /* Exactly at the end: outside. */
        let d = doc().insert_text(pos(0, 10), "X");
        assert_eq!(covered(&d), "beta");
        let d = doc().insert_text(pos(0, 16), " delta");
        assert_eq!(anchors(&d), anchors(&doc()));
    }

    #[test]
    fn typing_in_another_paragraph_does_not_touch_the_comment() {
        let d = doc().insert_text(pos(1, 0), "zzz");
        assert_eq!(anchors(&d), anchors(&doc()));
    }

    #[test]
    fn a_collapsed_anchor_stays_collapsed() {
        let d = commented(DocumentTree::from_text("hello world"), pos(0, 5), pos(0, 5));
        let d = d.insert_text(pos(0, 5), "XY");
        let r = &d.comment_ranges[0];
        assert_eq!(r.start, r.end);
        assert_eq!(r.start.offset, 7);
    }

    /* ---- delete_range: single paragraph ---- */

    #[test]
    fn deleting_before_inside_and_after_a_comment() {
        let d = doc().delete_range(pos(0, 0), pos(0, 6));
        assert_eq!(covered(&d), "beta");
        let d = doc().delete_range(pos(0, 7), pos(0, 9));
        assert_eq!(covered(&d), "ba");
        let d = doc().delete_range(pos(0, 10), pos(0, 16));
        assert_eq!(covered(&d), "beta");
        /* A deletion straddling the comment start clips it. */
        let d = doc().delete_range(pos(0, 3), pos(0, 8));
        assert_eq!(covered(&d), "ta");
    }

    /* ---- delete_range: paragraph merges ---- */

    #[test]
    fn a_paragraph_merge_keeps_the_comment_on_its_text() {
        /* Comment on "third" (block 2); delete the break between 0 and 1. */
        let d = commented(
            DocumentTree::from_paragraphs(["one".into(), "two".into(), "third".into()]),
            pos(2, 0),
            pos(2, 5),
        );
        let merged = d.delete_range(pos(0, 3), pos(1, 0));
        assert_eq!(merged.blocks.len(), 2);
        assert_eq!(covered(&merged), "third");
        /* Comment in the END paragraph's surviving tail moves onto the
        merged paragraph. */
        let d = commented(
            DocumentTree::from_paragraphs(["one".into(), "two words".into()]),
            pos(1, 4),
            pos(1, 9),
        );
        let merged = d.delete_range(pos(0, 1), pos(1, 2));
        assert_eq!(merged.paragraph_text(0), Some("oo words"));
        assert_eq!(covered(&merged), "words");
        /* Comment inside a deleted middle paragraph collapses onto the
        merge point, which still addresses a real paragraph. */
        let d = commented(
            DocumentTree::from_paragraphs(["a".into(), "gone".into(), "b".into()]),
            pos(1, 0),
            pos(1, 4),
        );
        let merged = d.delete_range(pos(0, 1), pos(2, 0));
        assert_eq!(
            anchors(&merged),
            ((vec![PathStep::Block(0)], 1), (vec![PathStep::Block(0)], 1))
        );
        /* Comment before the deleted span stays; one ending inside it
        clips to the merge point. */
        let d = commented(
            DocumentTree::from_paragraphs(["abcdef".into(), "xyz".into()]),
            pos(0, 1),
            pos(1, 2),
        );
        let merged = d.delete_range(pos(0, 4), pos(1, 1));
        assert_eq!(merged.paragraph_text(0), Some("abcdyz"));
        assert_eq!(covered(&merged), "bcdy");
    }

    /* ---- tracked changes ---- */

    #[test]
    fn tracked_typing_and_own_backspace_remap_both_tables() {
        let d = with_markup(doc());
        let mut undo = UndoStack::new(d.clone(), 100);
        let t = d.tracked_insert_text(pos(0, 0), "XYZ ", "A".into(), "d".into());
        undo.push(t.clone());
        assert_eq!(covered(&t), "beta");
        /* Backspace inside the reviewer's own pending insertion removes the
        text (the owning-insert path) — both tables follow. */
        let t2 = t.tracked_delete_range(pos(0, 1), pos(0, 3), "A".into(), "d".into());
        undo.push(t2.clone());
        assert_eq!(t2.paragraph_text(0), Some("X alpha beta gamma"));
        assert_eq!(covered(&t2), "beta");
        assert!(in_step(t2.nth_paragraph(0).unwrap()));
        assert!(undo.undo() && undo.undo());
        assert_eq!(anchors(undo.current()), anchors(&d));
    }

    #[test]
    fn accept_and_reject_remap_the_comment() {
        let mut d = with_markup(doc());
        let mut blocks = d.blocks.clone();
        if let Block::Paragraph(p) = &mut blocks[0] {
            /* "alpha " (0..6) is a tracked deletion. */
            p.revisions.push(Revision {
                start: 0,
                end: 6,
                kind: RevisionKind::Delete,
                author: "A".into(),
                date: "d".into(),
                id: None,
                prev_attrs: None,
                move_name: None,
            });
        }
        d.blocks = blocks;
        let accepted = d.accept_revision_at(0, 0, 6);
        assert_eq!(accepted.paragraph_text(0), Some("beta gamma"));
        assert_eq!(covered(&accepted), "beta");
        assert!(in_step(accepted.nth_paragraph(0).unwrap()));
        let rejected = d.reject_revision_at(0, 0, 6);
        assert_eq!(covered(&rejected), "beta");
    }

    /// Issue #253 — "Accept all" (the UI walks every revision in reverse
    /// document order through `accept_revision_at`) keeps a comment on its
    /// text across several paragraphs' deletions. (The engine-side
    /// accept-all and its paragraph-MARK merges: `revision_tests`, #262.)
    #[test]
    fn accept_all_keeps_the_comment_on_its_text() {
        let base = with_markup(DocumentTree::from_paragraphs([
            "drop keep".into(),
            "xx target yy".into(),
        ]));
        let mut blocks = base.blocks.clone();
        let del = |start, end| Revision {
            start,
            end,
            kind: RevisionKind::Delete,
            author: "A".into(),
            date: "d".into(),
            id: None,
            prev_attrs: None,
            move_name: None,
        };
        if let Block::Paragraph(p) = &mut blocks[0] {
            p.revisions.push(del(0, 5));
        }
        if let Block::Paragraph(p) = &mut blocks[1] {
            p.revisions.push(del(0, 3));
            p.revisions.push(del(9, 12));
        }
        let mut d = base;
        d.blocks = blocks;
        let d = commented(d, pos(1, 3), pos(1, 9));
        let mut undo = UndoStack::new(d.clone(), 100);
        for (block, s, e) in [(1, 9, 12), (1, 0, 3), (0, 0, 5)] {
            let next = undo.current().accept_revision_at(block, s, e);
            undo.push(next);
        }
        let done = undo.current();
        assert_eq!(done.paragraph_text(0), Some("keep"));
        assert_eq!(done.paragraph_text(1), Some("target"));
        assert_eq!(covered(done), "target");
        assert!(in_step(done.nth_paragraph(1).unwrap()));
    }

    /* ---- field / note / inline objects ---- */

    #[test]
    fn inserting_a_field_before_the_comment_shifts_it() {
        let d = with_markup(doc()).insert_field_at(pos(0, 0), "PAGE", "1");
        assert_eq!(covered(&d), "beta");
        assert!(in_step(d.nth_paragraph(0).unwrap()));
        let d = doc().insert_field_at(pos(0, 8), "PAGE", "12");
        assert_eq!(covered(&d), "be12ta");
    }

    #[test]
    fn inserting_a_note_image_or_text_box_before_the_comment_shifts_it() {
        let (d, _) = with_markup(doc()).insert_note_at(pos(0, 2), NoteKind::Footnote);
        assert_eq!(covered(&d), "beta");
        assert!(in_step(d.nth_paragraph(0).unwrap()));
        let blob = ImageBlob {
            content_type: "image/png".into(),
            data: vec![0],
        };
        let d = with_markup(doc()).insert_inline_image_at(pos(0, 0), blob, 10, 10);
        assert_eq!(covered(&d), "beta");
        assert!(in_step(d.nth_paragraph(0).unwrap()));
        let (d, _, _) = with_markup(doc()).insert_text_box_at(pos(0, 3), 10, 10);
        assert_eq!(covered(&d), "beta");
        /* After the comment: unchanged. */
        let (d, _) = doc().insert_note_at(pos(0, 12), NoteKind::Endnote);
        assert_eq!(anchors(&d), anchors(&doc()));
        /* Inside: the object joins the commented text. */
        let (d, _) = doc().insert_note_at(pos(0, 8), NoteKind::Footnote);
        assert_eq!(covered(&d), "be\u{FFFC}ta");
    }

    /* ---- multi-line, IME-shaped and rich paste ---- */

    #[test]
    fn multiline_paste_before_the_comment_moves_it_to_the_last_line() {
        let (d, _) = doc().insert_multiline(pos(0, 0), "one\ntwo\n");
        assert_eq!(d.paragraph_text(2), Some("alpha beta gamma"));
        assert_eq!(covered(&d), "beta");
    }

    #[test]
    fn rich_paste_single_paragraph_before_inside_after() {
        let frag = [Paragraph {
            text: "PASTE".into(),
            ..Paragraph::default()
        }];
        let (d, _) = with_markup(doc()).insert_rich(pos(0, 0), &frag);
        assert_eq!(covered(&d), "beta");
        assert!(in_step(d.nth_paragraph(0).unwrap()));
        let (d, _) = doc().insert_rich(pos(0, 8), &frag);
        assert_eq!(covered(&d), "bePASTEta");
        let (d, _) = doc().insert_rich(pos(0, 10), &frag);
        assert_eq!(covered(&d), "beta");
    }

    #[test]
    fn rich_paste_multi_paragraph_before_the_comment() {
        let frag = [
            Paragraph {
                text: "P1".into(),
                ..Paragraph::default()
            },
            Paragraph {
                text: "P2".into(),
                ..Paragraph::default()
            },
            Paragraph {
                text: "P3".into(),
                ..Paragraph::default()
            },
        ];
        let (d, _) = doc().insert_rich(pos(0, 2), &frag);
        assert_eq!(d.paragraph_text(2), Some("P3pha beta gamma"));
        assert_eq!(covered(&d), "beta");
        /* The later paragraphs shift down by the inserted block count. */
        let d2 = commented(
            DocumentTree::from_paragraphs(["x".into(), "target".into()]),
            pos(1, 0),
            pos(1, 6),
        );
        let (d2, _) = d2.insert_rich(pos(0, 1), &frag);
        assert_eq!(covered(&d2), "target");
        /* After the paste point in the same paragraph (multi-block):
        the anchor rides the tail onto the last pasted paragraph. */
        let (d3, _) = doc().insert_rich(pos(0, 6), &frag);
        assert_eq!(covered(&d3), "beta");
    }

    #[test]
    fn rich_block_paste_with_a_table_keeps_the_comment() {
        let table = DocumentTree::new()
            .insert_table(BlockPath::top(0), 1, 1)
            .blocks
            .iter()
            .find(|b| matches!(b, Block::Table(_)))
            .cloned()
            .expect("table");
        let blocks = [
            Block::Paragraph(Paragraph {
                text: "head".into(),
                ..Paragraph::default()
            }),
            table.clone(),
            Block::Paragraph(Paragraph {
                text: "last".into(),
                ..Paragraph::default()
            }),
        ];
        let (d, _) = doc().insert_rich_blocks(pos(0, 0), &blocks);
        assert_eq!(covered(&d), "beta");
        /* A paste ending in a table: the tail becomes its own paragraph. */
        let blocks = [
            Block::Paragraph(Paragraph {
                text: "head".into(),
                ..Paragraph::default()
            }),
            table.clone(),
        ];
        let (d, _) = doc().insert_rich_blocks(pos(0, 2), &blocks);
        assert_eq!(covered(&d), "beta");
        let (d, _) = doc().insert_rich_blocks(pos(0, 12), &blocks);
        assert_eq!(covered(&d), "beta");
        /* A lone table: head / table / tail (this shape used to panic on
        an inverted middle-block slice and insert the table twice). */
        let (d, _) = doc().insert_rich_blocks(pos(0, 3), std::slice::from_ref(&table));
        assert_eq!(d.blocks.len(), 5);
        assert!(matches!(d.blocks[1], Block::Table(_)));
        assert_eq!(d.paragraph_text(0), Some("alp"));
        assert_eq!(covered(&d), "beta");
    }

    #[test]
    fn a_field_restamp_remaps_the_comment_and_the_markup() {
        /* "p. 1 of the text": field result "1" at 3..4; comment on "text". */
        let d = with_markup(DocumentTree::from_text("p. 1 of the text"));
        let mut blocks = d.blocks.clone();
        if let Block::Paragraph(p) = &mut blocks[0] {
            p.fields.push(crate::Field {
                start: 3,
                end: 4,
                instruction: "NUMPAGES".into(),
                span: None,
                source: None,
            });
        }
        let mut d = d;
        d.blocks = blocks;
        let d = commented(d, pos(0, 12), pos(0, 16));
        let restamped = d.restamp_fields(&mut |_| Some("1234".into()));
        assert_eq!(restamped.paragraph_text(0), Some("p. 1234 of the text"));
        assert_eq!(covered(&restamped), "text");
        assert!(in_step(restamped.nth_paragraph(0).unwrap()));
        let mut undo = UndoStack::new(d, 10);
        undo.push(restamped);
    }

    #[test]
    fn undo_restores_the_pre_edit_anchor() {
        let d = doc();
        let mut undo = UndoStack::new(d.clone(), 100);
        undo.push(d.insert_text(pos(0, 0), "12345"));
        assert_eq!(undo.current().comment_ranges[0].start.offset, 11);
        undo.push(undo.current().delete_range(pos(0, 0), pos(1, 0)));
        assert!(undo.undo());
        assert_eq!(undo.current().comment_ranges[0].start.offset, 11);
        assert!(undo.undo());
        assert_eq!(anchors(undo.current()), anchors(&d));
        assert_eq!(covered(undo.current()), "beta");
    }

    /* ---- the shared record ---- */

    #[test]
    fn splice_text_drives_both_tables_from_one_record() {
        let mut d = with_markup(doc());
        let mut p = d.nth_paragraph(0).unwrap().clone();
        let edit = p.splice_text(8, 4, "XYZW!");
        assert_eq!(p.text, "alpha beXYZW!amma");
        assert!(in_step(&p));
        let mut blocks = d.blocks.clone();
        blocks.set(0, Block::Paragraph(p));
        d.blocks = blocks;
        d.remap_text_edit_record(&BlockPath::top(0), edit);
        assert_eq!(covered(&d), "beXYZW!");
    }

    #[test]
    fn note_replace_keeps_a_covering_run_on_the_replacement() {
        let mut slot = Some(Box::new(SourceMarkup {
            text_len: 10,
            runs: vec![
                SourceRun {
                    start: 0,
                    end: 4,
                    ..SourceRun::default()
                },
                SourceRun {
                    start: 4,
                    end: 7,
                    ..SourceRun::default()
                },
                SourceRun {
                    start: 7,
                    end: 10,
                    ..SourceRun::default()
                },
            ],
            ..SourceMarkup::default()
        }));
        SourceMarkup::note_replace(&mut slot, 10, 4, 7, 5);
        let m = slot.unwrap();
        let r: Vec<_> = m.runs.iter().map(|r| (r.start, r.end)).collect();
        assert_eq!(r, vec![(0, 4), (4, 9), (9, 12)]);
        assert_eq!(m.text_len, 12);
    }

    #[test]
    #[should_panic(expected = "issue #250")]
    fn the_debug_assertion_catches_an_unaware_edit() {
        let d = with_markup(doc());
        let mut blocks = d.blocks.clone();
        if let Block::Paragraph(p) = &mut blocks[0] {
            p.text.push_str(" unaware");
        }
        let mut bad = d.clone();
        bad.blocks = blocks;
        let mut undo = UndoStack::new(d, 10);
        undo.push(bad);
    }
}
