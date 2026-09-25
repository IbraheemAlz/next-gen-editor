//! Issue #243 — comment anchors in a *regenerated* paragraph.
//!
//! A comment is anchored in `document.xml` by up to three in-paragraph
//! pieces: `<w:commentRangeStart w:id/>`, `<w:commentRangeEnd w:id/>` and
//! the run holding `<w:commentReference w:id/>`. The model keeps the range
//! at the tree level ([`engine::DocumentTree::comment_ranges`], remapped by
//! every edit through `engine::text_remap`) and the comment body in
//! `comment_defs`; the reader additionally records each piece's source
//! bytes as a *comment-anchor* [`engine::SourceMarker`] (offset-remapped
//! with the rest of the paragraph's source markup).
//!
//! A clean paragraph re-emits its source bytes and needs none of this. A
//! regenerated one used to drop all three pieces (the comment detached from
//! its text; a paragraph holding only a reference saved as `<w:p/>`). The
//! writer now:
//!
//! - replays a comment-anchor marker verbatim only when it is *verified*:
//!   a range end must sit exactly where the tree puts that end of that
//!   comment (or, for a comment the tree has no range for — a table-cell
//!   anchor, an unpaired end — the comment must still exist); a reference
//!   only while the comment exists. A deleted comment's markers are
//!   dropped, never resurrected.
//! - synthesizes every tree endpoint that no verbatim byte carries (an
//!   engine-minted comment, a paragraph whose markup went stale): a bare
//!   `<w:commentRangeStart/End w:id/>` at the tree offset, followed after
//!   the end by a `CommentReference`-styled reference run when the source
//!   has none anywhere.
//!
//! [`CommentPlan`] is computed once per `document.xml` write (from the tree)
//! and published for the duration of the write, the
//! `WRITE_INHERITED_DIRECTIONS` pattern — the paragraph serializer has no
//! document in hand.

use engine::{Block, CommentAnchor, CommentAnchorKind, DocumentTree, Paragraph, SourceMarker};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// One tree endpoint to place in a paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeAnchor {
    pub at: u32,
    /// Emission order at one offset: an ordinary range end (0) before a
    /// collapsed range's start (1) and end (2), before an ordinary start
    /// (3) — adjacent comments never overlap and a collapsed one stays
    /// `Start, End`.
    pub rank: u8,
    pub kind: CommentAnchorKind,
    pub id: u32,
}

/// Comment anchoring facts for one `document.xml` write.
#[derive(Debug, Default)]
pub struct CommentPlan {
    /// Comments that exist (`comment_defs` ∪ `comment_ranges`).
    live: HashSet<u32>,
    /// Comments the tree has a range for.
    ranged: HashSet<u32>,
    /// Tree endpoints per paragraph, keyed by the paragraph's address in
    /// the tree being written.
    by_para: HashMap<usize, Vec<TreeAnchor>>,
    /// Anchor pieces some verbatim byte already carries (a clean
    /// paragraph's source, a block-level fragment, a verified marker).
    present: HashSet<(CommentAnchorKind, u32)>,
    /// Reference runs already synthesized in this write.
    synthesized_refs: HashSet<u32>,
}

fn para_key(p: &Paragraph) -> usize {
    p as *const Paragraph as usize
}

/// Every block of `blocks`, table cells included, parents first.
fn walk_blocks<'a>(blocks: impl IntoIterator<Item = &'a Block>, f: &mut dyn FnMut(&'a Block)) {
    for b in blocks {
        f(b);
        if let Block::Table(t) = b {
            for row in &t.rows {
                for cell in &row.cells {
                    walk_blocks(&cell.blocks, f);
                }
            }
        }
    }
}
/// `(kind, id)` of every comment anchor in a block-level verbatim
/// fragment (a range marker between two paragraphs, issue #120).
fn scan_fragment(xml: &[u8], out: &mut HashSet<(CommentAnchorKind, u32)>) {
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    loop {
        let e = match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) => e,
            Ok(Event::Eof) | Err(_) => return,
            Ok(_) => {
                buf.clear();
                continue;
            }
        };
        let kind = match e.name().as_ref() {
            b"w:commentRangeStart" => Some(CommentAnchorKind::RangeStart),
            b"w:commentRangeEnd" => Some(CommentAnchorKind::RangeEnd),
            b"w:commentReference" => Some(CommentAnchorKind::Reference),
            _ => None,
        };
        if let Some(kind) = kind
            && let Some(id) = e
                .attributes()
                .flatten()
                .find(|a| a.key.as_ref() == b"w:id")
                .and_then(|a| std::str::from_utf8(&a.value).ok()?.trim().parse().ok())
        {
            out.insert((kind, id));
        }
        buf.clear();
    }
}

impl CommentPlan {
    /// The plan for writing `doc`'s body.
    pub fn for_document(doc: &DocumentTree) -> Self {
        let mut plan = Self {
            live: doc.comment_defs.keys().copied().collect(),
            ..Self::default()
        };
        for r in &doc.comment_ranges {
            plan.live.insert(r.id);
            plan.ranged.insert(r.id);
            let collapsed = r.start == r.end;
            for (pos, kind, rank) in [
                (
                    &r.start,
                    CommentAnchorKind::RangeStart,
                    if collapsed { 1 } else { 3 },
                ),
                (
                    &r.end,
                    CommentAnchorKind::RangeEnd,
                    if collapsed { 2 } else { 0 },
                ),
            ] {
                if let Some(p) = doc.paragraph_at_path(&pos.path) {
                    plan.by_para
                        .entry(para_key(p))
                        .or_default()
                        .push(TreeAnchor {
                            at: pos.offset.min(p.text.len() as u32),
                            rank,
                            kind,
                            id: r.id,
                        });
                }
            }
        }
        for anchors in plan.by_para.values_mut() {
            anchors.sort_by_key(|a| (a.at, a.rank));
        }
        /* What verbatim bytes already carry. */
        let mut present = HashSet::new();
        walk_blocks(doc.blocks.iter(), &mut |b| {
            if let Some(bx) = b.body_xml() {
                for frag in bx.before.iter().chain(bx.after.iter()) {
                    if let engine::BodyFragment::Verbatim { xml } = frag {
                        scan_fragment(xml, &mut present);
                    }
                }
            }
            let Block::Paragraph(p) = b else {
                return;
            };
            let Some(m) = p.source_markup.as_deref() else {
                return;
            };
            let clean = !p.dirty && p.source_xml.is_some();
            if !clean && !m.offsets_valid(p.text.len()) {
                return;
            }
            for mk in &m.markers {
                if let Some(c) = mk.comment
                    && (clean || plan.verified(p, mk.at, c))
                {
                    present.insert((c.kind, c.id));
                }
            }
        });
        plan.present = present;
        plan
    }

    /// `true` when the comment-anchor marker `c` at offset `at` of `p` may
    /// be replayed verbatim.
    fn verified(&self, p: &Paragraph, at: u32, c: CommentAnchor) -> bool {
        if !self.live.contains(&c.id) {
            return false;
        }
        match c.kind {
            CommentAnchorKind::Reference => true,
            _ if !self.ranged.contains(&c.id) => true,
            kind => self.by_para.get(&para_key(p)).is_some_and(|v| {
                v.iter()
                    .any(|a| a.kind == kind && a.id == c.id && a.at == at)
            }),
        }
    }
}

thread_local! {
    static WRITE_COMMENT_PLAN: RefCell<Option<CommentPlan>> = const { RefCell::new(None) };
}

/// Clears the published plan when the write ends (early `?` returns
/// included), restoring any outer one.
pub struct CommentPlanScope(Option<CommentPlan>);

impl Drop for CommentPlanScope {
    fn drop(&mut self) {
        let prev = self.0.take();
        WRITE_COMMENT_PLAN.with(|c| *c.borrow_mut() = prev);
    }
}

/// Publish `doc`'s plan for the duration of one body write.
pub fn publish(doc: &DocumentTree) -> CommentPlanScope {
    let plan = CommentPlan::for_document(doc);
    CommentPlanScope(WRITE_COMMENT_PLAN.with(|c| c.borrow_mut().replace(plan)))
}

/// What the run walk of one regenerated paragraph needs: the tree
/// endpoints no verbatim byte carries (to synthesize), and a verdict per
/// comment-anchor marker. `None` outside a published write (a header /
/// footer / note part): markers replay as recorded, nothing is
/// synthesized.
pub struct ParagraphAnchors {
    pub synthesize: Vec<TreeAnchor>,
}

/// The tree endpoints of `p` that must be synthesized.
pub fn paragraph_anchors(p: &Paragraph) -> ParagraphAnchors {
    WRITE_COMMENT_PLAN.with(|c| {
        let plan = c.borrow();
        let synthesize = plan
            .as_ref()
            .and_then(|plan| {
                plan.by_para.get(&para_key(p)).map(|v| {
                    v.iter()
                        .filter(|a| !plan.present.contains(&(a.kind, a.id)))
                        .copied()
                        .collect()
                })
            })
            .unwrap_or_default();
        ParagraphAnchors { synthesize }
    })
}

/// `true` when marker `mk` (at its offset in `p`) may be written: every
/// ordinary marker; a comment-anchor marker only when verified.
pub fn keep_marker(p: &Paragraph, mk: &SourceMarker) -> bool {
    let Some(c) = mk.comment else {
        return true;
    };
    WRITE_COMMENT_PLAN.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_none_or(|plan| plan.verified(p, mk.at, c))
    })
}

/// Write a synthesized range marker.
pub fn push_range_marker(kind: CommentAnchorKind, id: u32, out: &mut String) {
    let name = match kind {
        CommentAnchorKind::RangeStart => "w:commentRangeStart",
        CommentAnchorKind::RangeEnd => "w:commentRangeEnd",
        CommentAnchorKind::Reference => return,
    };
    out.push_str(&format!("<{name} w:id=\"{id}\"/>"));
}

/// After the range END of comment `id` was written: synthesize its
/// reference run when no verbatim byte carries one and none was
/// synthesized yet (Word's `CommentReference` character style).
pub fn after_range_end(id: u32, out: &mut String) {
    let emit = WRITE_COMMENT_PLAN.with(|c| {
        let mut plan = c.borrow_mut();
        let Some(plan) = plan.as_mut() else {
            return false;
        };
        plan.live.contains(&id)
            && !plan.present.contains(&(CommentAnchorKind::Reference, id))
            && plan.synthesized_refs.insert(id)
    });
    if emit {
        out.push_str(&format!(
            "<w:r><w:rPr><w:rStyle w:val=\"CommentReference\"/></w:rPr><w:commentReference w:id=\"{id}\"/></w:r>"
        ));
    }
}
