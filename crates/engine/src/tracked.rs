//! Issues #301 / #298 — recording tracked deletions and STRUCTURAL tracked
//! changes: a paragraph break typed with review mode on (an inserted
//! paragraph mark) and a deletion of any range inside one container —
//! paragraph marks included (deleted marks). The other text-level
//! recorders (`tracked_insert_text`, `tracked_format_change`) live with
//! the mutators in `lib.rs`; resolution (accept / reject) lives in
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
    BlockPath, DocumentTree, LogicalPos, Paragraph, PathStep, Revision, RevisionKind,
    mutate_paragraph_in_top, order_positions, same_parent,
};

/// Issue #298 — why a tracked deletion could not be recorded. The editor
/// answers it as an `Event::Error` (never a silent no-op).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackedEditError {
    /// The range crosses a container boundary (body ↔ table cell, two
    /// cells): the deletion would have to reshape the table.
    CrossContainer,
    /// The range spans a table (or another non-paragraph block): tracked
    /// table-row deletion (`<w:trPr><w:del/>`) is not modeled.
    SpansTable,
    /// An end of the range does not address a paragraph.
    NoParagraph,
}

impl fmt::Display for TrackedEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CrossContainer => {
                "with track changes on, a deletion cannot cross a table-cell boundary yet"
            }
            Self::SpansTable => {
                "with track changes on, a deletion cannot span a table yet \
                 (tracked table-row deletion is not modeled)"
            }
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
/// writer assigns one).
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

    /// Sprint 14 (#14) / issue #298 — a deletion with review mode on, over
    /// any range inside ONE container (the body, or one table cell):
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
    ///   already deleted.
    ///
    /// Accepting the result removes the text and merges the paragraphs
    /// ([`Self::resolve_all_revisions`]); rejecting restores everything.
    /// `start` stays put; `end` is returned where it landed. A range that
    /// crosses a container boundary or spans a table is refused
    /// ([`TrackedEditError`]) — never silently ignored.
    pub fn try_tracked_delete_range(
        &self,
        start: LogicalPos,
        end: LogicalPos,
        author: &str,
        date: &str,
    ) -> Result<TrackedDeletion, TrackedEditError> {
        let (start, end) = order_positions(start, end);
        if !same_parent(&start.path, &end.path) {
            return Err(TrackedEditError::CrossContainer);
        }
        let (Some((PathStep::Block(s_idx), container)), Some(PathStep::Block(e_idx))) =
            (start.path.steps.split_last(), end.path.steps.last())
        else {
            return Err(TrackedEditError::NoParagraph);
        };
        let (s_idx, e_idx) = (*s_idx, *e_idx);
        let at = |i: u32| child(container, i);
        for i in s_idx..=e_idx {
            if self.paragraph_at_path(&at(i)).is_none() {
                return Err(if i == s_idx || i == e_idx {
                    TrackedEditError::NoParagraph
                } else {
                    TrackedEditError::SpansTable
                });
            }
        }
        let snap = |i: u32, off: u32| {
            self.paragraph_at_path(&at(i))
                .map_or(off, |p| p.snap_offset(off))
        };
        let (s_off, e_off) = (snap(s_idx, start.offset), snap(e_idx, end.offset));
        let start = LogicalPos::new(at(s_idx), s_off);
        let mut out = self.clone();
        /* 1. Text, paragraph by paragraph. */
        let mut end_off = e_off;
        for i in s_idx..=e_idx {
            let path = at(i);
            let len = out
                .paragraph_at_path(&path)
                .map_or(0, |p| p.text.len() as u32);
            let s = if i == s_idx { s_off } else { 0 };
            let e = if i == e_idx { e_off } else { len };
            if s >= e {
                continue;
            }
            let mut edits = Vec::new();
            let mut blocks = out.blocks.clone();
            let _ = mutate_paragraph_in_top(&mut blocks, &path, |p| {
                edits = tracked_delete_span(p, s, e, author, date);
            });
            out.blocks = blocks;
            for edit in edits {
                if i == e_idx {
                    end_off -= edit.removed;
                }
                out.remap_text_edit_record(&path, edit);
            }
        }
        /* 2. The swallowed marks (paragraphs `s_idx .. e_idx`), from the
        back so a merge never shifts a mark still to visit. */
        let mut end_idx = e_idx;
        let mut merged = false;
        for i in (s_idx..e_idx).rev() {
            let Some(head) = out.paragraph_at_path(&at(i)) else {
                continue;
            };
            let own = head
                .mark_revisions
                .iter()
                .any(|r| r.kind == RevisionKind::Insert && r.author == author);
            let dead = head
                .mark_revisions
                .iter()
                .any(|r| matches!(r.kind, RevisionKind::Delete | RevisionKind::MoveFrom));
            let head_len = head.text.len() as u32;
            if own && out.merge_paragraph_with_next(container, i) {
                merged = true;
                if end_idx == i + 1 {
                    end_off += head_len;
                    end_idx = i;
                } else {
                    end_idx -= 1;
                }
                continue;
            }
            if !dead {
                let mut blocks = out.blocks.clone();
                let _ = mutate_paragraph_in_top(&mut blocks, &at(i), |p| {
                    p.mark_revisions
                        .push(mark_change(RevisionKind::Delete, author, date));
                    p.dirty = true;
                });
                out.blocks = blocks;
            }
        }
        if merged {
            out = out.with_list_markers_refreshed();
        }
        Ok(TrackedDeletion {
            doc: out,
            start,
            end: LogicalPos::new(at(end_idx), end_off),
        })
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
    let clipped = |r: &Revision| (r.start.max(s), r.end.min(e));
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
