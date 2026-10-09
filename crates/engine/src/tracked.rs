//! Issues #301 / #298 — recording tracked deletions and STRUCTURAL tracked
//! changes: a paragraph break typed with review mode on (an inserted
//! paragraph mark) and a deletion of any range inside one container —
//! paragraph marks included (deleted marks). Issue #366 — a paste (plain
//! multi-line or rich) with review mode on records its text and every
//! paragraph mark it creates as inserted. The other text-level recorders
//! (`tracked_insert_text`, `tracked_format_change`) live with the
//! mutators in `lib.rs`; resolution (accept / reject) lives in
//! `revisions.rs`.
//!
//! Every text change goes through [`crate::Paragraph::splice_text`] (via
//! `revisions::remove_text`, the #265 own-insertion path that also shifts
//! every other offset table) and its edit record through
//! [`DocumentTree::remap_text_edit_record`]; every paragraph merge
//! through [`DocumentTree::merge_paragraph_with_next`] — so source markup
//! and comment anchors stay in step (issues #250 / #252 / #253).

use std::fmt;

use crate::text_remap::TextEdit;
use crate::{
    Block, BlockPath, DocumentTree, LogicalPos, Paragraph, PathStep, Revision, RevisionKind,
    bump_last_block_index, mutate_paragraph_in_top, order_positions,
};

/// Issue #298 — why a tracked deletion could not be recorded. The editor
/// answers it as an `Event::Error` (never a silent no-op). Since issue
/// #365 every range between two paragraphs is recordable — across cells
/// and over tables too — so the only refusal left is a malformed end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackedEditError {
    /// An end of the range does not address a paragraph.
    NoParagraph,
}

impl fmt::Display for TrackedEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoParagraph => "the deletion range does not address a paragraph",
        })
    }
}

impl std::error::Error for TrackedEditError {}

/// Issue #298 — a recorded tracked deletion: the new tree, and where the
/// range's ends landed in it (the reviewer's own pending insertions inside
/// the range were removed outright, so the end can move).
#[derive(Debug, Clone)]
pub struct TrackedDeletion {
    pub doc: DocumentTree,
    pub start: LogicalPos,
    pub end: LogicalPos,
}

/// A paragraph-mark revision (`start` / `end` are unused on a mark).
pub(crate) fn mark_change(kind: RevisionKind, author: &str, date: &str) -> Revision {
    Revision {
        start: 0,
        end: 0,
        kind,
        author: author.to_owned(),
        date: date.to_owned(),
        id: None,
        prev_attrs: None,
        move_name: None,
    }
}

/// Issue #301 — the text revisions of a paragraph split at byte `at`: the
/// left half keeps those before `at`, the right half gets those after it
/// (shifted), and a change straddling `at` is cut in two. The right piece
/// of a cut change carries no source `w:id` (ids are document-unique; the
/// writer assigns one). Issue #292 — the one cut every split uses:
/// [`Paragraph::split_at`] calls it, and `Paragraph::concat` re-joins the
/// two pieces at the seam.
pub(crate) fn split_revisions(revs: &[Revision], at: u32) -> (Vec<Revision>, Vec<Revision>) {
    let mut left = Vec::new();
    let mut right = Vec::new();
    for r in revs {
        if r.start < at {
            left.push(Revision {
                end: r.end.min(at),
                ..r.clone()
            });
        }
        if r.end > at {
            right.push(Revision {
                start: r.start.max(at) - at,
                end: r.end - at,
                id: if r.start < at { None } else { r.id },
                ..r.clone()
            });
        }
    }
    left.retain(|r| r.start < r.end);
    right.retain(|r| r.start < r.end);
    (left, right)
}

impl DocumentTree {
    /// Issue #301 — a paragraph break typed with review mode on: split
    /// the paragraph at `at` (exactly [`Self::split_paragraph`]) and record
    /// the NEW paragraph mark — the one now ending the left half, at the
    /// split point; the original mark stays with the right half (#262's
    /// travel rule) — as inserted by `author`. That is Word's
    /// `<w:pPr><w:rPr><w:ins/>` on the first paragraph: accepting keeps
    /// the break, rejecting merges the two paragraphs back
    /// ([`Self::resolve_all_revisions`], or the single decision on the
    /// left paragraph's mark).
    pub fn tracked_split_paragraph(&self, at: LogicalPos, author: &str, date: &str) -> Self {
        /* The paragraph the new mark ends: the addressed one, or — on a
        document with no paragraph at all, where `split_paragraph`
        appends two empty ones — the first appended. */
        let target = if self.paragraph_at_path(&at.path).is_some() {
            Some(at.path.clone())
        } else if self.paragraph_count() == 0 && self.path_to_first_paragraph_deep().is_none() {
            Some(BlockPath::top(self.blocks.len() as u32))
        } else {
            None
        };
        let mut out = self.split_paragraph(at);
        if let Some(path) = target {
            let mut blocks = out.blocks.clone();
            let _ = mutate_paragraph_in_top(&mut blocks, &path, |p| {
                p.mark_revisions = vec![mark_change(RevisionKind::Insert, author, date)];
                p.dirty = true;
            });
            out.blocks = blocks;
        }
        out
    }

    /// Issue #366 — a multi-line plain paste with review mode on: exactly
    /// [`Self::insert_multiline`], with every line going through the
    /// tracked typing path ([`Self::tracked_insert_text`] — one `Insert`
    /// per line, growing an adjacent insertion of `author`'s, splitting a
    /// deletion it lands in) and every newline through the tracked
    /// paragraph break ([`Self::tracked_split_paragraph`] — the new mark
    /// is inserted). Rejecting the result removes the text and merges the
    /// paragraphs back; accepting keeps both. Returns the new tree and the
    /// caret at the end of the last pasted line.
    pub fn tracked_insert_multiline(
        &self,
        at: LogicalPos,
        text: &str,
        author: &str,
        date: &str,
    ) -> (Self, LogicalPos) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<&str> = normalized.split('\n').collect();
        let mut doc = self.clone();
        let mut cur = at;
        for (i, line) in lines.iter().enumerate() {
            /* Where the line lands: `tracked_insert_text` snaps the
            offset against the pre-insert text and appends at the
            document end when the path is stale — mirror both. */
            let landed = doc
                .paragraph_at_path(&cur.path)
                .map(|p| p.snap_offset(cur.offset));
            doc = doc.tracked_insert_text(cur.clone(), line, author.to_owned(), date.to_owned());
            let path = if landed.is_some() {
                cur.path.clone()
            } else {
                doc.path_to_last_top_paragraph()
                    .unwrap_or(BlockPath::top(0))
            };
            let start = landed.unwrap_or_else(|| {
                doc.paragraph_at_path(&path)
                    .map_or(0, |p| p.text.len().saturating_sub(line.len()) as u32)
            });
            let after = LogicalPos::new(path.clone(), start + line.len() as u32);
            if i + 1 < lines.len() {
                doc = doc.tracked_split_paragraph(after, author, date);
                cur = LogicalPos::new(bump_last_block_index(&path), 0);
            } else {
                cur = after;
            }
        }
        (doc, cur)
    }

    /// Issue #366 — a rich (HTML) paste with review mode on: exactly
    /// [`Self::insert_rich_blocks`], with the pasted content recorded as
    /// `author`'s insertion. The fragment is new text whatever it carried:
    /// each pasted paragraph (table cells included) becomes ONE `Insert`
    /// over its text. Every paragraph mark the paste CREATES is an
    /// inserted mark: the one now ending the head (the first pasted
    /// paragraph's, or — when the paste opens with a table — the head's
    /// fresh mark) and each middle paragraph's; the last pasted
    /// paragraph's mark gives way to the target's original one, which
    /// ends the tail (`Paragraph::concat`), so a one-paragraph paste
    /// creates none. Rejecting the result removes the text and merges the
    /// paragraphs back; accepting keeps everything.
    pub fn tracked_insert_rich_blocks(
        &self,
        at: LogicalPos,
        blocks_in: &[Block],
        author: &str,
        date: &str,
    ) -> (Self, LogicalPos) {
        let stamped: Vec<Block> = blocks_in
            .iter()
            .map(|b| stamp_inserted(b, author, date))
            .collect();
        let target = self.rich_paste_target(&at);
        let (mut out, caret) = self.insert_rich_blocks(at, &stamped);
        let Some((PathStep::Block(idx), container)) =
            target.as_ref().and_then(|(t, _)| t.steps.split_last())
        else {
            return (out, caret);
        };
        /* The created marks, by their block index in the container (the
        layout `insert_rich_blocks` builds): the head at `idx`, the
        opening table (when the paste opens with one) after it, then the
        middle blocks; the last pasted block ends at the original mark. */
        let n = blocks_in.len();
        let opens_with_paragraph = matches!(blocks_in.first(), Some(Block::Paragraph(_)));
        let mut created = Vec::new();
        if n >= 2 || !opens_with_paragraph {
            created.push(*idx);
        }
        let first_middle = idx + 1 + u32::from(!opens_with_paragraph);
        let middles = blocks_in.get(1..n.saturating_sub(1)).unwrap_or(&[]);
        created.extend(
            (first_middle..)
                .zip(middles)
                .filter(|(_, b)| matches!(b, Block::Paragraph(_)))
                .map(|(k, _)| k),
        );
        let mut blocks = out.blocks.clone();
        for i in created {
            let _ = mutate_paragraph_in_top(&mut blocks, &child(container, i), |p| {
                p.mark_revisions = vec![mark_change(RevisionKind::Insert, author, date)];
            });
        }
        out.blocks = blocks;
        (out, caret)
    }

    /// Sprint 14 (#14) / issues #298 / #365 — a deletion with review mode
    /// on, over any range between two paragraphs:
    ///
    /// - text: per paragraph (the head's tail, every whole middle
    ///   paragraph, the last one's head), bytes inside one of `author`'s
    ///   own pending insertions are removed outright (deleting your own
    ///   pending edit undoes it — the #265 path), bytes already deleted
    ///   stay as they are, the rest is marked `Delete` by `author`
    ///   (coalescing with an adjacent deletion of theirs);
    /// - paragraph marks the range swallows: a mark `author` inserted
    ///   (a tracked Enter of theirs, issue #301) is removed — the two
    ///   paragraphs merge — and every other one is marked `Delete`
    ///   (next to an insertion by someone else, issue #303), unless
    ///   already deleted. Issue #365 — a mark is swallowed when the
    ///   paragraph it would merge with on accept (the next one, past any
    ///   table the range deletes whole) is inside the range;
    /// - issue #365 — table rows, Word's way: a range that crosses a row
    ///   boundary (or enters a table from outside) deletes every row it
    ///   touches WHOLE — the row is marked deleted (`<w:trPr><w:del/>`)
    ///   and its cells' contents like text above (a nested table's rows
    ///   marked too); a row `author` inserted is removed outright. A
    ///   range across cells of ONE row deletes each cell's sub-range,
    ///   with no structural change. A range from body text into a table
    ///   is the body part plus the whole rows it reaches.
    ///
    /// Accepting the result removes the text and rows and merges the
    /// paragraphs ([`Self::resolve_all_revisions`]); rejecting restores
    /// everything. `start` stays put; `end` is returned where it landed.
    /// An end that addresses no paragraph is refused
    /// ([`TrackedEditError`]) — never silently ignored.
    pub fn try_tracked_delete_range(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        author: &str,
        date: &str,
    ) -> Result<TrackedDeletion, TrackedEditError> {
        let (start, end) = order_positions(start, end);
        if self.paragraph_at_path(&start.path).is_none()
            || self.paragraph_at_path(&end.path).is_none()
        {
            return Err(TrackedEditError::NoParagraph);
        }
        let mut out = self.clone();
        let (start, end) = out.tracked_delete_between(&start, &end, author, date)?;
        let (start, end) = (out.settle_pos(start), out.settle_pos(end));
        Ok(TrackedDeletion {
            doc: out.with_list_markers_refreshed(),
            start,
            end,
        })
    }

    /// [`Self::try_tracked_delete_range`]'s dispatch on where the two
    /// (ordered, paragraph-addressing) ends meet: one container, or
    /// different cells of one table. Returns where the ends landed.
    fn tracked_delete_between(
        &mut self,
        start: &LogicalPos,
        end: &LogicalPos,
        author: &str,
        date: &str,
    ) -> Result<(LogicalPos, LogicalPos), TrackedEditError> {
        let (ss, es) = (&start.path.steps, &end.path.steps);
        let common = ss.iter().zip(es).take_while(|(a, b)| a == b).count();
        match (ss.get(common), es.get(common)) {
            /* One paragraph. */
            (None, None) => match ss.split_last() {
                Some((PathStep::Block(i), container)) => {
                    Ok(self.tracked_delete_in(container, *i, *i, start, end, author, date))
                }
                _ => Err(TrackedEditError::NoParagraph),
            },
            /* Blocks `s ..= e` of one container (either end may lie in a
            table among them). */
            (Some(PathStep::Block(s)), Some(PathStep::Block(e))) => {
                Ok(self.tracked_delete_in(&ss[..common], *s, *e, start, end, author, date))
            }
            /* Different cells of the table at `ss[..common]`. */
            (
                Some(PathStep::Cell { row: r1, col: c1 }),
                Some(PathStep::Cell { row: r2, col: _ }),
            ) => {
                let table = BlockPath {
                    steps: ss[..common].to_vec(),
                };
                if r1 == r2 {
                    self.tracked_delete_across_cells(&table, *r1, *c1, start, end, author, date)
                } else {
                    let (r1, r2) = (*r1, *r2);
                    self.mark_rows_deleted(&table, r1, r2, author, date);
                    Ok((start.clone(), end.clone()))
                }
            }
            _ => Err(TrackedEditError::NoParagraph),
        }
    }

    /// Issue #365 — a range across cells `c1 ..` of row `row` of `table`
    /// (both ends in that row): each cell's sub-range — the first cell
    /// from `start` to its end, the middle ones whole, the last one from
    /// its start to `end` — deleted like a range of its own (each cell is
    /// its own container: no structural change).
    #[allow(clippy::too_many_arguments)]
    fn tracked_delete_across_cells(
        &mut self,
        table: &BlockPath,
        row: u32,
        c1: u32,
        start: &LogicalPos,
        end: &LogicalPos,
        author: &str,
        date: &str,
    ) -> Result<(LogicalPos, LogicalPos), TrackedEditError> {
        let d = table.steps.len();
        let Some(PathStep::Cell { col: c2, .. }) = end.path.steps.get(d) else {
            return Err(TrackedEditError::NoParagraph);
        };
        let mut end_at = end.clone();
        /* From the last cell back, so the end's report is taken first and
        no earlier cell's edit can move it. */
        for c in (c1..=*c2).rev() {
            let cell = child_cell(table, row, c);
            let (Some(first), Some(last)) = (
                self.cell_text_edge(&cell, false),
                self.cell_text_edge(&cell, true),
            ) else {
                continue;
            };
            let from = if c == c1 { start.clone() } else { first };
            let to = if c == *c2 { end.clone() } else { last };
            let (_, landed) = self.tracked_delete_between(&from, &to, author, date)?;
            if c == *c2 {
                end_at = landed;
            }
        }
        Ok((start.clone(), end_at))
    }

    /// The first paragraph start (`at_end == false`) or the last paragraph
    /// end directly inside the cell container `cell` (its steps lead into
    /// the cell's block list).
    fn cell_text_edge(&self, cell: &BlockPath, at_end: bool) -> Option<LogicalPos> {
        let blocks = crate::parent_container_snapshot(self, &child(&cell.steps, 0))?;
        let mut paras = blocks
            .iter()
            .enumerate()
            .filter_map(|(i, b)| b.as_paragraph().map(|p| (i as u32, p)));
        let (i, p) = if at_end {
            paras.next_back()?
        } else {
            paras.next()?
        };
        let offset = if at_end { p.text.len() as u32 } else { 0 };
        Some(LogicalPos::new(child(&cell.steps, i), offset))
    }

    /// Blocks `s ..= e` of `container` (its steps lead into the block
    /// list): `start` lies in block `s` — the paragraph itself, or deep
    /// inside a table there — and `end` in block `e`. Text, swallowed
    /// marks and whole rows are recorded from the back, so a merge or a
    /// removed table never shifts a block still to visit. Returns where
    /// the ends landed (`start` never moves).
    #[allow(clippy::too_many_arguments)]
    fn tracked_delete_in(
        &mut self,
        container: &[PathStep],
        s: u32,
        e: u32,
        start: &LogicalPos,
        end: &LogicalPos,
        author: &str,
        date: &str,
    ) -> (LogicalPos, LogicalPos) {
        let d = container.len();
        let row_of = |pos: &LogicalPos| match pos.path.steps.get(d + 1) {
            Some(PathStep::Cell { row, .. }) => Some(*row),
            _ => None,
        };
        let (start_row, end_row) = (row_of(start), row_of(end));
        let Some(blocks) = crate::parent_container_snapshot(self, &child(container, 0)) else {
            return (start.clone(), end.clone());
        };
        let is_para = |i: u32| matches!(blocks.get(i as usize), Some(Block::Paragraph(_)));
        /* The last paragraph a swallowed mark may merge into: `e` itself,
        or — when the range ends inside a table — the one before it. */
        let limit = if end_row.is_none() {
            Some(e)
        } else {
            e.checked_sub(1)
        };
        let swallowed =
            |i: u32| limit.is_some_and(|limit| (i + 1..=limit).find(|&j| is_para(j)).is_some());
        let snap = |pos: &LogicalPos| {
            self.paragraph_at_path(&pos.path)
                .map_or(pos.offset, |p| p.snap_offset(pos.offset))
        };
        let (s_off, e_off) = (snap(start), snap(end));
        let mut end_idx = e;
        let mut end_off = e_off;
        let at = |i: u32| child(container, i);
        for i in (s..=e).rev() {
            match blocks.get(i as usize) {
                Some(Block::Paragraph(_)) => {
                    /* 1. Its text. */
                    let path = at(i);
                    let len = self
                        .paragraph_at_path(&path)
                        .map_or(0, |p| p.text.len() as u32);
                    let from = if i == s && start_row.is_none() {
                        s_off
                    } else {
                        0
                    };
                    let to = if i == e && end_row.is_none() {
                        e_off
                    } else {
                        len
                    };
                    for edit in self.tracked_delete_text(&path, from, to, author, date) {
                        if i == end_idx && end_row.is_none() {
                            end_off -= edit.removed;
                        }
                    }
                    /* 2. Its mark, when the range swallows it. */
                    if i < e && swallowed(i) {
                        let head_len = self
                            .paragraph_at_path(&path)
                            .map_or(0, |p| p.text.len() as u32);
                        if self.delete_mark(container, i, author, date) {
                            if end_row.is_none() && end_idx == i + 1 {
                                end_off += head_len;
                                end_idx = i;
                            } else {
                                end_idx -= 1;
                            }
                        }
                    }
                }
                Some(Block::Table(t)) => {
                    let Some(last) = (t.rows.len() as u32).checked_sub(1) else {
                        continue;
                    };
                    let from = if i == s { start_row.unwrap_or(0) } else { 0 };
                    let to = if i == e {
                        end_row.unwrap_or(last)
                    } else {
                        last
                    };
                    if self.mark_rows_deleted(&at(i), from, to, author, date) && end_idx > i {
                        end_idx -= 1;
                    }
                }
                None => {}
            }
        }
        let end_at = if end_row.is_none() {
            LogicalPos::new(at(end_idx), end_off)
        } else {
            let mut steps = end.path.steps.clone();
            steps[d] = PathStep::Block(end_idx);
            LogicalPos::new(BlockPath { steps }, end.offset)
        };
        (LogicalPos::new(start.path.clone(), s_off), end_at)
    }

    /// Record the tracked deletion of bytes `[from, to)` of the paragraph
    /// at `path` (see [`tracked_delete_span`]); the comment anchors and
    /// source markup follow every edit. Returns the edits performed.
    fn tracked_delete_text(
        &mut self,
        path: &BlockPath,
        from: u32,
        to: u32,
        author: &str,
        date: &str,
    ) -> Vec<TextEdit> {
        if from >= to {
            return Vec::new();
        }
        let mut edits = Vec::new();
        let mut blocks = self.blocks.clone();
        let _ = mutate_paragraph_in_top(&mut blocks, path, |p| {
            edits = tracked_delete_span(p, from, to, author, date);
        });
        self.blocks = blocks;
        for edit in &edits {
            self.remap_text_edit_record(path, *edit);
        }
        edits
    }

    /// The mark of paragraph `i` of `container` is swallowed by a tracked
    /// deletion: `author`'s own inserted mark is removed (the paragraph
    /// merges with the next one — `true`), any other one is marked
    /// `Delete` unless already deleted.
    fn delete_mark(&mut self, container: &[PathStep], i: u32, author: &str, date: &str) -> bool {
        let path = child(container, i);
        let Some(head) = self.paragraph_at_path(&path) else {
            return false;
        };
        let own = head
            .mark_revisions
            .iter()
            .any(|r| r.kind == RevisionKind::Insert && r.author == author);
        let dead = head
            .mark_revisions
            .iter()
            .any(|r| matches!(r.kind, RevisionKind::Delete | RevisionKind::MoveFrom));
        if own && self.merge_paragraph_with_next(container, i) {
            return true;
        }
        if !dead {
            let mut blocks = self.blocks.clone();
            let _ = mutate_paragraph_in_top(&mut blocks, &path, |p| {
                p.mark_revisions
                    .push(mark_change(RevisionKind::Delete, author, date));
                p.dirty = true;
            });
            self.blocks = blocks;
        }
        false
    }

    /// Issue #365 — delete rows `from ..= to` of the table at `table`
    /// whole, with review mode on: each row is marked deleted by `author`
    /// (`<w:trPr><w:del/>`, unless already deleted) and its contents
    /// recorded as deleted — every paragraph of its cells like text (the
    /// reviewer's own insertions removed), every mark but each cell's
    /// last swallowed, a nested table's rows deleted the same way (its
    /// text is left alone: the resolver's text walk does not reach that
    /// deep, and the row goes with its parent on accept anyway). A row
    /// `author` inserted is removed outright. `true` when the table
    /// itself left its container (every row was such a row).
    pub(crate) fn mark_rows_deleted(
        &mut self,
        table: &BlockPath,
        from: u32,
        to: u32,
        author: &str,
        date: &str,
    ) -> bool {
        let Some(t) = self.table_at_path(table) else {
            return false;
        };
        let to = to.min((t.rows.len() as u32).saturating_sub(1));
        let mut own = Vec::new();
        let mut marked = Vec::new();
        let mut t = t.clone();
        for r in from..=to {
            let Some(row) = t.rows.get_mut(r as usize) else {
                continue;
            };
            let revs = &mut row.props.revisions;
            if revs
                .iter()
                .any(|x| x.kind == RevisionKind::Insert && x.author == author)
            {
                own.push(r);
                continue;
            }
            if !revs.iter().any(|x| x.kind == RevisionKind::Delete) {
                revs.push(mark_change(RevisionKind::Delete, author, date));
            }
            marked.push((r, row.cells.len() as u32));
        }
        t.dirty = true;
        t.source_xml = None;
        let mut top = self.blocks.clone();
        let _ = crate::replace_block_in_top(&mut top, table, Block::Table(t));
        self.blocks = top;
        /* The contents of every marked row, cell by cell. */
        let reaches_text = table.steps.len() == 1;
        for (r, cells) in marked {
            for c in 0..cells {
                let cell = child_cell(table, r, c);
                let Some(blocks) = crate::parent_container_snapshot(self, &child(&cell.steps, 0))
                else {
                    continue;
                };
                let n = blocks.len() as u32;
                for k in (0..n).rev() {
                    let path = child(&cell.steps, k);
                    match &blocks[k as usize] {
                        Block::Paragraph(p) if reaches_text => {
                            let len = p.text.len() as u32;
                            self.tracked_delete_text(&path, 0, len, author, date);
                            if k + 1 < n {
                                self.delete_mark(&cell.steps, k, author, date);
                            }
                        }
                        Block::Paragraph(_) => {}
                        Block::Table(inner) => {
                            let rows = inner.rows.len() as u32;
                            if rows > 0 {
                                self.mark_rows_deleted(&path, 0, rows - 1, author, date);
                            }
                        }
                    }
                }
            }
        }
        self.remove_table_rows(table, &own)
    }

    /// `pos` if it still addresses a paragraph (offset snapped), else the
    /// nearest paragraph of the same container: the start of the first
    /// one at or after its block, else the end of the last one before.
    fn settle_pos(&self, pos: LogicalPos) -> LogicalPos {
        if let Some(p) = self.paragraph_at_path(&pos.path) {
            let offset = p.snap_offset(pos.offset);
            return LogicalPos::new(pos.path, offset);
        }
        let mut steps = pos.path.steps.clone();
        while let Some(PathStep::Block(i)) = steps.pop() {
            if let Some(blocks) = crate::parent_container_snapshot(self, &child(&steps, 0)) {
                let found = (i as usize..blocks.len())
                    .find(|&k| blocks[k].as_paragraph().is_some())
                    .map(|k| LogicalPos::new(child(&steps, k as u32), 0))
                    .or_else(|| {
                        (0..(i as usize).min(blocks.len())).rev().find_map(|k| {
                            blocks[k].as_paragraph().map(|p| {
                                LogicalPos::new(child(&steps, k as u32), p.text.len() as u32)
                            })
                        })
                    });
                if let Some(found) = found {
                    return found;
                }
            }
            /* Up one container: drop the `Cell` step too. */
            if !matches!(steps.pop(), Some(PathStep::Cell { .. })) {
                break;
            }
        }
        pos
    }
}

/// `table` + `Cell { row, col }`: the steps leading into a cell's blocks.
fn child_cell(table: &BlockPath, row: u32, col: u32) -> BlockPath {
    let mut steps = table.steps.clone();
    steps.push(PathStep::Cell { row, col });
    BlockPath { steps }
}

/// Issue #366 — `block` as pasted new text by `author`: every paragraph
/// (table cells included) one `Insert` over its whole text, whatever
/// revisions the fragment carried. Inside a pasted table every cell
/// paragraph's mark but the cell's last (which ends the cell) is a
/// pasted paragraph break — an inserted mark; a top-level paragraph's
/// mark is the paste's business ([`DocumentTree::insert_rich_blocks`]
/// strips fragment marks).
fn stamp_inserted(block: &Block, author: &str, date: &str) -> Block {
    match block {
        Block::Paragraph(p) => {
            let mut p = p.clone();
            let len = p.text.len() as u32;
            p.revisions = if len > 0 {
                vec![Revision {
                    start: 0,
                    end: len,
                    ..mark_change(RevisionKind::Insert, author, date)
                }]
            } else {
                Vec::new()
            };
            p.dirty = true;
            Block::Paragraph(p)
        }
        Block::Table(t) => {
            let mut t = t.clone();
            /* Issue #365 — a pasted row is an inserted row
            (`<w:trPr><w:ins/>`): rejecting the paste removes it. */
            for row in t.rows.iter_mut() {
                row.props.revisions = vec![mark_change(RevisionKind::Insert, author, date)];
            }
            for cell in t.rows.iter_mut().flat_map(|r| r.cells.iter_mut()) {
                let n = cell.blocks.len();
                for (i, b) in cell.blocks.iter_mut().enumerate() {
                    let mut stamped = stamp_inserted(b, author, date);
                    if let Block::Paragraph(p) = &mut stamped {
                        p.mark_revisions = if i + 1 < n {
                            vec![mark_change(RevisionKind::Insert, author, date)]
                        } else {
                            Vec::new()
                        };
                    }
                    *b = stamped;
                }
            }
            t.dirty = true;
            t.source_xml = None;
            Block::Table(t)
        }
    }
}

/// `container` + `Block(i)`.
fn child(container: &[PathStep], i: u32) -> BlockPath {
    let mut steps = container.to_vec();
    steps.push(PathStep::Block(i));
    BlockPath { steps }
}

/// Record the tracked deletion of bytes `[s, e)` of `para` by `author`
/// (see [`DocumentTree::try_tracked_delete_range`]); returns the text
/// edits performed, in order, for the caller's comment-anchor remap.
fn tracked_delete_span(
    para: &mut Paragraph,
    s: u32,
    e: u32,
    author: &str,
    date: &str,
) -> Vec<TextEdit> {
    let (s, e) = (para.snap_offset(s), para.snap_offset(e));
    if s >= e {
        return Vec::new();
    }
    /* Issue #115 — every boundary the deletion stores is a char boundary,
    whatever offsets an existing revision carries. */
    let snap = |o: u32| crate::snap_offset(&para.text, o);
    let clipped = |r: &Revision| (snap(r.start.max(s)), snap(r.end.min(e)));
    let own = union(
        para.revisions
            .iter()
            .filter(|r| r.kind == RevisionKind::Insert && r.author == author)
            .map(clipped),
    );
    let dead = union(
        para.revisions
            .iter()
            .filter(|r| matches!(r.kind, RevisionKind::Delete | RevisionKind::MoveFrom))
            .map(clipped),
    );
    for (a, b) in subtract(&subtract(&[(s, e)], &own), &dead) {
        mark_deleted(para, a, b, author, date);
    }
    /* Remove the own insertions back to front, so each edit's offsets
    are valid when it runs (and in the order the caller remaps them). */
    let edits = own
        .iter()
        .rev()
        .map(|&(a, b)| crate::revisions::remove_text(para, a, b))
        .collect();
    coalesce_deletions(para, author, date);
    para.dirty = true;
    edits
}

/// Sorted union of the non-empty intervals `it`.
fn union(it: impl Iterator<Item = (u32, u32)>) -> Vec<(u32, u32)> {
    let mut v: Vec<(u32, u32)> = it.filter(|(a, b)| a < b).collect();
    v.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(v.len());
    for (a, b) in v {
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// `segments` minus the sorted, disjoint `cuts`.
fn subtract(segments: &[(u32, u32)], cuts: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for &(mut a, b) in segments {
        for &(c, d) in cuts {
            if d <= a || c >= b {
                continue;
            }
            if c > a {
                out.push((a, c));
            }
            a = a.max(d);
        }
        if a < b {
            out.push((a, b));
        }
    }
    out
}

/// Mark `[a, b)` deleted by `author`, growing an adjacent deletion of
/// theirs instead of fragmenting.
fn mark_deleted(para: &mut Paragraph, a: u32, b: u32, author: &str, date: &str) {
    let mine = |r: &&mut Revision| r.kind == RevisionKind::Delete && r.author == author;
    if let Some(left) = para.revisions.iter_mut().filter(mine).find(|r| r.end == a) {
        left.end = b;
        left.date = date.to_owned();
    } else if let Some(right) = para
        .revisions
        .iter_mut()
        .filter(mine)
        .find(|r| r.start == b)
    {
        right.start = a;
        right.date = date.to_owned();
    } else {
        para.revisions.push(Revision {
            start: a,
            end: b,
            ..mark_change(RevisionKind::Delete, author, date)
        });
    }
}

/// Merge `author`'s deletions that became adjacent once their own
/// insertion between them was removed (at least one side engine-minted,
/// so two source revisions keep their ids).
fn coalesce_deletions(para: &mut Paragraph, author: &str, date: &str) {
    loop {
        let revs = &para.revisions;
        let pair = (0..revs.len()).find_map(|i| {
            (0..revs.len()).find_map(|j| {
                let (l, r) = (&revs[i], &revs[j]);
                (i != j
                    && l.kind == RevisionKind::Delete
                    && r.kind == RevisionKind::Delete
                    && l.author == author
                    && r.author == author
                    && l.end == r.start
                    && (l.id.is_none() || r.id.is_none()))
                .then_some((i, j))
            })
        });
        let Some((i, j)) = pair else {
            return;
        };
        let right = para.revisions[j].clone();
        let left = &mut para.revisions[i];
        left.end = right.end;
        left.id = left.id.or(right.id);
        left.date = date.to_owned();
        para.revisions.remove(j);
    }
}
