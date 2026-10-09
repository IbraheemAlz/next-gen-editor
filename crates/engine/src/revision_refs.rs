//! Issues #305 / #304 — addressing ONE tracked change for the
//! single-revision accept / reject, which then runs through the same
//! resolver as accept-all ([`DocumentTree::resolve_revisions`]).
//!
//! A [`RevisionRef`] names a revision by where it lives: the paragraph
//! (a story-rooted [`BlockPath`]) and the slot in it — the n-th text
//! overlay of [`Paragraph::revisions`] or the n-th change on the
//! paragraph mark ([`Paragraph::mark_revisions`] — a mark can carry
//! several, issue #303). A [`RevisionPick`] is the set one resolution
//! pass touches.
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

use crate::{
    Block, BlockPath, DocumentTree, Paragraph, PathStep, Revision, RevisionKind, TableRow,
};

/// Which revision of a paragraph (or, issue #365, of a table row) a
/// [`RevisionRef`] means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RevisionSlot {
    /// `Paragraph::revisions[i]`.
    Text(u32),
    /// `Paragraph::mark_revisions[i]`.
    Mark(u32),
    /// Issue #365 — `rows[row].props.revisions[index]` of the TABLE at
    /// [`RevisionRef::path`] (a tracked row insertion / deletion).
    Row { row: u32, index: u32 },
}

/// One tracked change: the paragraph it lives in (the table, for a
/// [`RevisionSlot::Row`]) and its slot there.
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

    /// Paragraph-mark revision `i` of the paragraph at `path`.
    pub(crate) fn mark(&self, path: &BlockPath, i: usize) -> bool {
        match self {
            Self::All => true,
            Self::Only(set) => set.contains(&RevisionRef {
                path: path.clone(),
                slot: RevisionSlot::Mark(i as u32),
            }),
        }
    }

    /// Issue #365 — revision `i` of row `row` of the table at `table`.
    pub(crate) fn row(&self, table: &BlockPath, row: usize, i: usize) -> bool {
        match self {
            Self::All => true,
            Self::Only(set) => set.contains(&RevisionRef {
                path: table.clone(),
                slot: RevisionSlot::Row {
                    row: row as u32,
                    index: i as u32,
                },
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
    /// The paragraph the revision lives in; `None` for a table-row
    /// revision (issue #365 — [`RevisionSlot::Row`]).
    pub paragraph: Option<&'a Paragraph>,
}

impl DocumentTree {
    /// Issue #304 — every tracked change the resolver reaches (the body
    /// and one level of table cells), in document order — per paragraph
    /// its text revisions, then its mark's changes in order (one entry
    /// each, issue #303); issue #365 — per row of a top-level table its
    /// row changes, ahead of the row's cell paragraphs — each with its
    /// stable `revision_id`. Ids are unique within the document.
    pub fn revision_entries(&self) -> Vec<RevisionEntry<'_>> {
        let mut used = HashSet::new();
        let mut out = Vec::new();
        let mut push = |at: RevisionRef, key: u32, revision, paragraph| {
            let mut id = key;
            while !used.insert(id) {
                id = id.wrapping_add(1);
            }
            out.push(RevisionEntry {
                id,
                at,
                revision,
                paragraph,
            });
        };
        for item in items_deep(&self.blocks) {
            match item {
                Item::Paragraph(path, p) => {
                    let slots = p
                        .revisions
                        .iter()
                        .enumerate()
                        .map(|(i, r)| (RevisionSlot::Text(i as u32), r))
                        .chain(
                            p.mark_revisions
                                .iter()
                                .enumerate()
                                .map(|(i, r)| (RevisionSlot::Mark(i as u32), r)),
                        );
                    for (slot, r) in slots {
                        let at = RevisionRef {
                            path: path.clone(),
                            slot,
                        };
                        push(at, content_key(p, slot, r), r, Some(p));
                    }
                }
                Item::Row(table, row, tr) => {
                    for (i, r) in tr.props.revisions.iter().enumerate() {
                        let at = RevisionRef {
                            path: table.clone(),
                            slot: RevisionSlot::Row {
                                row,
                                index: i as u32,
                            },
                        };
                        push(at, row_key(tr, r), r, None);
                    }
                }
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
    /// revisions are never empty) — the FIRST of a mark's changes (issue
    /// #303). Two wrappers over one range, and a mark's later changes, are
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
        (start == end && !p.mark_revisions.is_empty() && start as usize == p.text.len()).then_some(
            RevisionRef {
                path,
                slot: RevisionSlot::Mark(0),
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
        let target = match at.slot {
            RevisionSlot::Text(i) => self.paragraph_at_path(&at.path)?.revisions.get(i as usize),
            RevisionSlot::Mark(i) => self
                .paragraph_at_path(&at.path)?
                .mark_revisions
                .get(i as usize),
            RevisionSlot::Row { row, index } => self
                .table_at_path(&at.path)?
                .rows
                .get(row as usize)?
                .props
                .revisions
                .get(index as usize),
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
                for (i, r) in p.mark_revisions.iter().enumerate() {
                    if move_of(r) == Some(name) {
                        picked.insert(RevisionRef {
                            path: path.clone(),
                            slot: RevisionSlot::Mark(i as u32),
                        });
                    }
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
    items_deep(blocks)
        .into_iter()
        .filter_map(|item| match item {
            Item::Paragraph(path, p) => Some((path, p)),
            Item::Row(..) => None,
        })
        .collect()
}

/// One stop of [`items_deep`].
enum Item<'a> {
    Paragraph(BlockPath, &'a Paragraph),
    /// Issue #365 — row `.1` of the top-level table at `.0`.
    Row(BlockPath, u32, &'a TableRow),
}

/// Every paragraph [`paragraphs_deep`] walks and, issue #365, every row
/// of a top-level table (ahead of its cell paragraphs), in document
/// order.
fn items_deep(blocks: &im::Vector<Block>) -> Vec<Item<'_>> {
    let mut out = Vec::new();
    for (bi, block) in blocks.iter().enumerate() {
        match block {
            Block::Paragraph(p) => out.push(Item::Paragraph(BlockPath::top(bi as u32), p)),
            Block::Table(t) => {
                for (ri, row) in t.rows.iter().enumerate() {
                    out.push(Item::Row(BlockPath::top(bi as u32), ri as u32, row));
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
                                out.push(Item::Paragraph(BlockPath { steps }, p));
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

/// Issue #365 — the content half of a row revision's id: what the
/// revision is (kind, author, date, `w:id`) and the row's text (its
/// direct cell paragraphs', cell by cell) — never where the row sits.
fn row_key(row: &TableRow, r: &Revision) -> u32 {
    let mut h = Fnv1a::new();
    h.write(&[kind_code(r.kind), 2]);
    h.write_str(&r.author);
    h.write_str(&r.date);
    match r.id {
        Some(id) => {
            h.write(b"#");
            h.write(&id.to_le_bytes());
        }
        None => h.write(b"-"),
    }
    for cell in &row.cells {
        for p in cell.blocks.iter().filter_map(Block::as_paragraph) {
            h.write_str(&p.text);
        }
        h.write(&[0xFE]);
    }
    h.0
}

fn kind_code(kind: RevisionKind) -> u8 {
    match kind {
        RevisionKind::Insert => 1,
        RevisionKind::Delete => 2,
        RevisionKind::FormatChange => 3,
        RevisionKind::MoveFrom => 4,
        RevisionKind::MoveTo => 5,
    }
}

/// The content half of a revision's id: FNV-1a over what the revision
/// IS — never where it sits.
fn content_key(p: &Paragraph, slot: RevisionSlot, r: &Revision) -> u32 {
    let mut h = Fnv1a::new();
    let kind = kind_code(r.kind);
    let covered = match slot {
        RevisionSlot::Text(_) => {
            let len = p.text.len() as u32;
            let s = p.snap_offset(r.start.min(len));
            let e = p.snap_offset(r.end.min(len)).max(s);
            p.text.get(s as usize..e as usize).unwrap_or("")
        }
        RevisionSlot::Mark(_) | RevisionSlot::Row { .. } => p.text.as_str(),
    };
    h.write(&[kind, u8::from(matches!(slot, RevisionSlot::Mark(_)))]);
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
