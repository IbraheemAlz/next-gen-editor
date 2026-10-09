//! Issue #305 — addressing ONE tracked change for the single-revision
//! accept / reject, which then runs through the same resolver as
//! accept-all ([`DocumentTree::resolve_revisions`]).
//!
//! A [`RevisionRef`] names a revision by where it lives: the paragraph
//! (a story-rooted [`BlockPath`]) and the slot in it — the n-th text
//! overlay of [`Paragraph::revisions`] or the paragraph-mark revision
//! ([`Paragraph::mark_revision`]). A [`RevisionPick`] is the set one
//! resolution pass touches.

use std::collections::HashSet;

use crate::{Block, BlockPath, DocumentTree};

/// Which revision of a paragraph a [`RevisionRef`] means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RevisionSlot {
    /// `Paragraph::revisions[i]`.
    Text(u32),
    /// `Paragraph::mark_revision`.
    Mark,
}

/// One tracked change: the paragraph it lives in and its slot there.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RevisionRef {
    pub path: BlockPath,
    pub slot: RevisionSlot,
}

/// The tracked changes one [`DocumentTree::resolve_revisions`] pass
/// resolves.
#[derive(Debug, Clone)]
pub enum RevisionPick {
    /// Every revision of the body (accept-all / reject-all).
    All,
    /// Exactly these (the single-revision path).
    Only(HashSet<RevisionRef>),
}

impl RevisionPick {
    /// Text revision `i` of the paragraph at `path`.
    pub(crate) fn text(&self, path: &BlockPath, i: usize) -> bool {
        match self {
            Self::All => true,
            Self::Only(set) => set.contains(&RevisionRef {
                path: path.clone(),
                slot: RevisionSlot::Text(i as u32),
            }),
        }
    }

    /// The paragraph-mark revision of the paragraph at `path`.
    pub(crate) fn mark(&self, path: &BlockPath) -> bool {
        match self {
            Self::All => true,
            Self::Only(set) => set.contains(&RevisionRef {
                path: path.clone(),
                slot: RevisionSlot::Mark,
            }),
        }
    }
}

impl DocumentTree {
    /// The legacy `(top-level block, start, end)` address of the
    /// single-revision commands: the first text revision of top-level
    /// paragraph `block` covering exactly `[start, end)`; a
    /// paragraph-mark revision answers to the empty range at the
    /// paragraph end (issue #262 — `revisions_snapshot` lists it so; text
    /// revisions are never empty).
    pub fn revision_at_range(&self, block: u32, start: u32, end: u32) -> Option<RevisionRef> {
        let p = self
            .blocks
            .get(block as usize)
            .and_then(Block::as_paragraph)?;
        let path = BlockPath::top(block);
        if let Some(i) = p
            .revisions
            .iter()
            .position(|r| r.start == start && r.end == end)
        {
            return Some(RevisionRef {
                path,
                slot: RevisionSlot::Text(i as u32),
            });
        }
        (start == end && p.mark_revision.is_some() && start as usize == p.text.len()).then_some(
            RevisionRef {
                path,
                slot: RevisionSlot::Mark,
            },
        )
    }

    /// Accept (`accept == true`) or reject the one revision `at` names,
    /// through [`Self::resolve_revisions`]. `None` when `at` names no
    /// revision (the tree is unchanged — no undo step for the caller).
    pub fn resolve_revision(&self, at: &RevisionRef, accept: bool) -> Option<Self> {
        let p = self.paragraph_at_path(&at.path)?;
        let exists = match at.slot {
            RevisionSlot::Text(i) => (i as usize) < p.revisions.len(),
            RevisionSlot::Mark => p.mark_revision.is_some(),
        };
        if !exists {
            return None;
        }
        let pick = RevisionPick::Only(HashSet::from([at.clone()]));
        Some(self.resolve_revisions(accept, &pick))
    }

    /// Accept a tracked-change revision identified by (top-level
    /// `block`, byte `start`, byte `end`) — [`Self::revision_at_range`].
    /// Semantics (issue #247): an accepted insertion / move destination
    /// keeps its text, an accepted deletion / move source loses it; a
    /// paragraph-mark revision resolves like accept-all's. No such
    /// revision → the tree comes back unchanged.
    pub fn accept_revision_at(&self, block: u32, start: u32, end: u32) -> Self {
        self.resolve_revision_at(block, start, end, true)
    }

    /// Reject a tracked-change revision identified by (top-level
    /// `block`, byte `start`, byte `end`): a rejected insertion / move
    /// destination loses its text, a rejected deletion / move source
    /// keeps it, a rejected formatting change restores the recorded
    /// style.
    pub fn reject_revision_at(&self, block: u32, start: u32, end: u32) -> Self {
        self.resolve_revision_at(block, start, end, false)
    }

    fn resolve_revision_at(&self, block: u32, start: u32, end: u32, accept: bool) -> Self {
        self.revision_at_range(block, start, end)
            .and_then(|at| self.resolve_revision(&at, accept))
            .unwrap_or_else(|| self.clone())
    }
}

#[cfg(test)]
#[path = "revision_ref_tests.rs"]
mod tests;
