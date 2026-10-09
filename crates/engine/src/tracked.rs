//! Issues #301 / #298 — recording STRUCTURAL tracked changes: a paragraph
//! break typed with review mode on (an inserted paragraph mark) and a
//! deletion that crosses paragraph marks (deleted marks). The text-level
//! recorders (`tracked_insert_text`, `tracked_delete_range`'s
//! same-paragraph case, `tracked_format_change`) live with the other
//! mutators in `lib.rs`; resolution (accept / reject) lives in
//! `revisions.rs`.
//!
//! Every text change goes through [`crate::Paragraph::splice_text`] (via
//! the shared helpers) and every paragraph merge through
//! [`DocumentTree::merge_paragraph_with_next`] — so source markup and
//! comment anchors stay in step (issues #250 / #252 / #253).

use crate::{BlockPath, DocumentTree, LogicalPos, Revision, RevisionKind, mutate_paragraph_in_top};

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
}
