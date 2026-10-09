//! Issues #305 / #304 — addressing ONE tracked change for the
//! single-revision accept / reject, which then runs through the same
//! resolver as accept-all ([`DocumentTree::resolve_revisions`]).
//!
//! A [`RevisionRef`] names a revision by where it lives: the paragraph
//! (a story-rooted [`BlockPath`]) and the slot in it — the n-th text
//! overlay of [`Paragraph::revisions`] or the paragraph-mark revision
//! ([`Paragraph::mark_revision`]). A [`RevisionPick`] is the set one
//! resolution pass touches.
//!
//! Issue #304 — the review UI addresses a revision by a stable
//! `revision_id` ([`DocumentTree::revision_entries`]), not by its range:
//! two wrappers over the same bytes (`<w:moveTo><w:del>`, Tika-792) are
//! two rows with two ids. The id is derived from the revision's CONTENT
//! — kind, author, date, `w:id`, move name and the text it covers (a
//! mark: its paragraph's text) — never from its position, so an edit
//! elsewhere, or resolving a neighbour, leaves it unchanged; identical
//! revisions are told apart in document order (the next free value).
//! Nothing is stored on the model: the id is recomputed from the tree
//! on every listing and every lookup, so undo, crash recovery and the
//! snapshot envelope are untouched. Resolving either half of a tracked
//! move resolves the whole move — every move revision sharing its
//! [`crate::Revision::move_name`] — as Word does.

use std::collections::HashSet;

use crate::{Block, BlockPath, DocumentTree, Paragraph, PathStep, Revision, RevisionKind};

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

/// Issue #304 — one tracked change as the review UI lists it.
#[derive(Debug, Clone)]
pub struct RevisionEntry<'a> {
    /// The stable `revision_id` (module docs).
    pub id: u32,
    pub at: RevisionRef,
    pub revision: &'a Revision,
    /// The paragraph the revision lives in.
    pub paragraph: &'a Paragraph,
}

impl DocumentTree {
    /// Issue #304 — every tracked change the resolver reaches (the body
    /// and one level of table cells), in document order — per paragraph
    /// its text revisions, then its mark — each with its stable
    /// `revision_id`. Ids are unique within the document.
    pub fn revision_entries(&self) -> Vec<RevisionEntry<'_>> {
        let mut used = HashSet::new();
        let mut out = Vec::new();
        for (path, p) in paragraphs_deep(&self.blocks) {
            let slots = p
                .revisions
                .iter()
                .enumerate()
                .map(|(i, r)| (RevisionSlot::Text(i as u32), r))
                .chain(p.mark_revision.iter().map(|r| (RevisionSlot::Mark, r)));
            for (slot, r) in slots {
                let mut id = content_key(p, slot, r);
                while !used.insert(id) {
                    id = id.wrapping_add(1);
                }
                out.push(RevisionEntry {
                    id,
                    at: RevisionRef {
                        path: path.clone(),
                        slot,
                    },
                    revision: r,
                    paragraph: p,
                });
            }
        }
        out
    }

    /// Issue #304 — the revision listed under `revision_id`, if it still
    /// exists.
    pub fn revision_by_id(&self, revision_id: u32) -> Option<RevisionRef> {
        self.revision_entries()
            .into_iter()
            .find(|e| e.id == revision_id)
            .map(|e| e.at)
    }

    /// The legacy `(top-level block, start, end)` address of the
    /// single-revision commands: the first text revision of top-level
    /// paragraph `block` covering exactly `[start, end)`; a
    /// paragraph-mark revision answers to the empty range at the
    /// paragraph end (issue #262 — `revisions_snapshot` lists it so; text
    /// revisions are never empty). Two wrappers over one range are
    /// ambiguous here — address them by id ([`Self::revision_by_id`]).
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
    /// through [`Self::resolve_revisions`] — together with, for a tracked
    /// move, every other move revision of the same move (issue #304:
    /// accepting the destination drops the source, rejecting either
    /// restores the source). `None` when `at` names no revision (the
    /// tree is unchanged — no undo step for the caller).
    pub fn resolve_revision(&self, at: &RevisionRef, accept: bool) -> Option<Self> {
        let p = self.paragraph_at_path(&at.path)?;
        let target = match at.slot {
            RevisionSlot::Text(i) => p.revisions.get(i as usize),
            RevisionSlot::Mark => p.mark_revision.as_ref(),
        }?;
        let mut picked = HashSet::from([at.clone()]);
        if let Some(name) = move_of(target) {
            for (path, p) in paragraphs_deep(&self.blocks) {
                for (i, r) in p.revisions.iter().enumerate() {
                    if move_of(r) == Some(name) {
                        picked.insert(RevisionRef {
                            path: path.clone(),
                            slot: RevisionSlot::Text(i as u32),
                        });
                    }
                }
                if p.mark_revision.as_ref().and_then(move_of) == Some(name) {
                    picked.insert(RevisionRef {
                        path,
                        slot: RevisionSlot::Mark,
                    });
                }
            }
        }
        Some(self.resolve_revisions(accept, &RevisionPick::Only(picked)))
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

/// The name of the tracked move `r` is a half of (`None`: not a move, or
/// a move read outside a named range — nothing to pair it with).
fn move_of(r: &Revision) -> Option<&str> {
    match r.kind {
        RevisionKind::MoveFrom | RevisionKind::MoveTo => r.move_name.as_deref(),
        _ => None,
    }
}

/// Every paragraph the resolver walks — the body and one level of table
/// cells, `fields::for_each_paragraph_deep`'s reach — with its path, in
/// document order.
fn paragraphs_deep(blocks: &im::Vector<Block>) -> Vec<(BlockPath, &Paragraph)> {
    let mut out = Vec::new();
    for (bi, block) in blocks.iter().enumerate() {
        match block {
            Block::Paragraph(p) => out.push((BlockPath::top(bi as u32), p)),
            Block::Table(t) => {
                for (ri, row) in t.rows.iter().enumerate() {
                    for (ci, cell) in row.cells.iter().enumerate() {
                        for (pi, nested) in cell.blocks.iter().enumerate() {
                            if let Block::Paragraph(p) = nested {
                                let steps = vec![
                                    PathStep::Block(bi as u32),
                                    PathStep::Cell {
                                        row: ri as u32,
                                        col: ci as u32,
                                    },
                                    PathStep::Block(pi as u32),
                                ];
                                out.push((BlockPath { steps }, p));
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// The content half of a revision's id: FNV-1a over what the revision
/// IS — never where it sits.
fn content_key(p: &Paragraph, slot: RevisionSlot, r: &Revision) -> u32 {
    let mut h = Fnv1a::new();
    let kind = match r.kind {
        RevisionKind::Insert => 1,
        RevisionKind::Delete => 2,
        RevisionKind::FormatChange => 3,
        RevisionKind::MoveFrom => 4,
        RevisionKind::MoveTo => 5,
    };
    let covered = match slot {
        RevisionSlot::Text(_) => {
            let len = p.text.len() as u32;
            let s = p.snap_offset(r.start.min(len));
            let e = p.snap_offset(r.end.min(len)).max(s);
            p.text.get(s as usize..e as usize).unwrap_or("")
        }
        RevisionSlot::Mark => p.text.as_str(),
    };
    h.write(&[kind, u8::from(slot == RevisionSlot::Mark)]);
    h.write_str(&r.author);
    h.write_str(&r.date);
    match r.id {
        Some(id) => {
            h.write(b"#");
            h.write(&id.to_le_bytes());
        }
        None => h.write(b"-"),
    }
    h.write_str(r.move_name.as_deref().unwrap_or(""));
    h.write_str(covered);
    h.0
}

/// 32-bit FNV-1a.
struct Fnv1a(u32);

impl Fnv1a {
    fn new() -> Self {
        Self(0x811c_9dc5)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u32::from(b);
            self.0 = self.0.wrapping_mul(0x0100_0193);
        }
    }

    /// `s` + a terminator, so adjacent fields cannot run together.
    fn write_str(&mut self, s: &str) {
        self.write(s.as_bytes());
        self.write(&[0xFF]);
    }
}

#[cfg(test)]
#[path = "revision_ref_tests.rs"]
mod tests;
