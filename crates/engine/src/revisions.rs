//! Issues #262 / #247 — resolving tracked changes structurally: the
//! document-wide accept / reject ([`DocumentTree::resolve_all_revisions`])
//! and the paragraph-MARK revisions (a tracked paragraph split or merge,
//! [`crate::Paragraph::mark_revisions`], several on one mark since #303).
//!
//! Issue #305 — ONE implementation: accept-all / reject-all and the
//! single-revision path ([`DocumentTree::accept_revision_at`], the
//! `revision_refs` addressing) are both [`DocumentTree::resolve_revisions`]
//! with a different [`RevisionPick`].
//!
//! Every text removal goes through [`remove_text`] —
//! [`Paragraph::splice_text`] plus the one overlay-shift rule — and the
//! returned edit through [`DocumentTree::remap_text_edit_record`]; every
//! merge through [`DocumentTree::remap_paragraph_merge`] (or, for a head
//! that vanishes whole, [`DocumentTree::remap_block_splice`]) — so the
//! source markup and the comment anchors stay in step with the text
//! (issues #250 / #252 / #253).

use std::collections::HashSet;

use crate::revision_refs::RevisionPick;
use crate::text_remap::TextEdit;
use crate::{
    Block, BlockPath, DocumentTree, Paragraph, PathStep, Revision, RevisionKind, SpanStyle,
    StyleRun, delete_block_at_path, mutate_paragraph_in_top, parent_container_snapshot,
    replace_block_in_top,
};

impl DocumentTree {
    /// `true` when any body paragraph (table cells included) carries a
    /// tracked change — text overlay or paragraph mark.
    pub fn has_revisions(&self) -> bool {
        let mut any = false;
        crate::fields::for_each_paragraph_deep(&self.blocks, &mut |_, p| {
            any |= !p.revisions.is_empty() || !p.mark_revisions.is_empty();
        });
        any
    }

    /// Issue #262 — accept (`accept == true`) or reject every tracked
    /// change of the body (table cells included), in document order, as
    /// ONE new tree (one undo step):
    ///
    /// 1. Text revisions, per paragraph: an accepted deletion / move
    ///    source or a rejected insertion / move destination removes its
    ///    text; the rest stays live. A rejected formatting change
    ///    restores the recorded style.
    /// 2. Paragraph-mark revisions, per container from the END (so a
    ///    chain of merges resolves in one pass): an accepted deleted /
    ///    moved-away mark, or a rejected inserted / moved-in one, merges
    ///    the paragraph with the next one; otherwise the mark just loses
    ///    its revision. A mark in front of a table, at the end of its
    ///    container, or carrying a section break cannot merge and only
    ///    loses its revision. A mark carrying several changes (issue #303)
    ///    resolves them in order: any one that removes the mark merges.
    /// 3. With every move resolved, the orphaned in-paragraph move-range
    ///    markers (`<w:moveFromRangeStart/>` …) are dropped.
    pub fn resolve_all_revisions(&self, accept: bool) -> Self {
        self.resolve_revisions(accept, &RevisionPick::All)
    }

    /// Issues #262 / #305 — accept (`accept == true`) or reject the
    /// tracked changes `pick` selects, as ONE new tree (one undo step).
    /// The single implementation behind accept-all / reject-all
    /// ([`RevisionPick::All`], steps 1–3 of
    /// [`Self::resolve_all_revisions`]) and the single-revision path
    /// ([`RevisionPick::Only`]): a revision the pick leaves out stays
    /// pending and travels with its text like every other overlay
    /// ([`remove_text`]); a resolved move drops only its own range
    /// markers.
    pub fn resolve_revisions(&self, accept: bool, pick: &RevisionPick) -> Self {
        let mut out = self.clone();
        let mut moves = ResolvedMoves::default();
        let mut paths = Vec::new();
        crate::fields::for_each_paragraph_deep(&out.blocks, &mut |path, p| {
            let mut picked_text = false;
            for (i, r) in p.revisions.iter().enumerate() {
                if pick.text(&path, i) {
                    picked_text = true;
                    moves.note(r);
                }
            }
            for (j, r) in p.mark_revisions.iter().enumerate() {
                if pick.mark(&path, j) {
                    moves.note(r);
                }
            }
            if picked_text {
                paths.push(path);
            }
        });
        for path in paths {
            let mut edits = Vec::new();
            let mut blocks = out.blocks.clone();
            let _ = mutate_paragraph_in_top(&mut blocks, &path, |para| {
                edits = resolve_text_revisions(para, accept, |i| pick.text(&path, i));
            });
            out.blocks = blocks;
            for e in edits {
                out.remap_text_edit_record(&path, e);
            }
        }
        out.resolve_marks_in(&[], accept, pick);
        out.drop_move_range_markers(&moves, pick);
        out.with_list_markers_refreshed()
    }

    /// Step 3: drop the in-paragraph range markers of the moves this pass
    /// resolved — every move-range marker when everything was resolved,
    /// otherwise the `w:name`d starts of the resolved moves and the ends
    /// sharing their `w:id`.
    fn drop_move_range_markers(&mut self, moves: &ResolvedMoves, pick: &RevisionPick) {
        if !moves.any {
            return;
        }
        let names = match pick {
            RevisionPick::All => None,
            RevisionPick::Only(_) => Some(&moves.names),
        };
        /* The ends carry only `w:id`: pair them through their starts. */
        let mut ends: HashSet<(bool, String)> = HashSet::new();
        if let Some(names) = names {
            crate::fields::for_each_paragraph_deep(&self.blocks, &mut |_, p| {
                for mk in p.source_markup.iter().flat_map(|m| m.markers.iter()) {
                    if let Some((from, true)) = move_range_role(&mk.xml)
                        && xml_attr(&mk.xml, b"w:name").is_some_and(|n| names.contains(&n))
                        && let Some(id) = xml_attr(&mk.xml, b"w:id")
                    {
                        ends.insert((from, id));
                    }
                }
            });
        }
        let drops = |xml: &[u8]| match (move_range_role(xml), names) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some((_, true)), Some(names)) => {
                xml_attr(xml, b"w:name").is_some_and(|n| names.contains(&n))
            }
            (Some((from, false)), Some(_)) => {
                xml_attr(xml, b"w:id").is_some_and(|id| ends.contains(&(from, id)))
            }
        };
        let mut marked = Vec::new();
        crate::fields::for_each_paragraph_deep(&self.blocks, &mut |path, p| {
            if p.source_markup
                .as_deref()
                .is_some_and(|m| m.markers.iter().any(|mk| drops(&mk.xml)))
            {
                marked.push(path);
            }
        });
        let mut blocks = self.blocks.clone();
        for path in marked {
            let _ = mutate_paragraph_in_top(&mut blocks, &path, |para| {
                if let Some(m) = para.source_markup.as_deref_mut() {
                    m.markers.retain(|mk| !drops(&mk.xml));
                }
            });
        }
        self.blocks = blocks;
    }

    /// Resolve every paragraph-mark revision `pick` selects in the
    /// container `container` (the steps leading INTO a block list: empty
    /// = the body), deepest first, from the container's end — so a merge
    /// never moves a block this walk has still to visit.
    fn resolve_marks_in(&mut self, container: &[PathStep], accept: bool, pick: &RevisionPick) {
        let Some(blocks) = container_blocks(self, container) else {
            return;
        };
        for i in (0..blocks.len() as u32).rev() {
            let path = child(container, i);
            match self.block_at(&path) {
                Some(Block::Table(t)) => {
                    let cells: Vec<(u32, u32)> = t
                        .rows
                        .iter()
                        .enumerate()
                        .flat_map(|(r, row)| {
                            (0..row.cells.len()).map(move |c| (r as u32, c as u32))
                        })
                        .collect();
                    for (row, col) in cells {
                        let mut inner = path.steps.clone();
                        inner.push(PathStep::Cell { row, col });
                        self.resolve_marks_in(&inner, accept, pick);
                    }
                }
                Some(Block::Paragraph(p))
                    if (0..p.mark_revisions.len()).any(|j| pick.mark(&path, j)) =>
                {
                    self.resolve_mark(container, i, accept, pick);
                }
                _ => {}
            }
        }
    }

    /// Resolve the mark revisions `pick` selects of paragraph `i` of
    /// `container` (every one for accept-all, the addressed one for a
    /// single decision). Issue #303 — a mark's changes resolve in order:
    /// a decided change that removes the mark (an accepted deletion /
    /// move source, a rejected insertion / move destination) merges the
    /// paragraph with the next one, and every other change goes with the
    /// mark it described; otherwise the decided changes are dropped and
    /// the rest stay pending.
    fn resolve_mark(&mut self, container: &[PathStep], i: u32, accept: bool, pick: &RevisionPick) {
        let Some(blocks) = container_blocks(self, container) else {
            return;
        };
        let Some(Block::Paragraph(head)) = blocks.get(i as usize) else {
            return;
        };
        let path = child(container, i);
        let decided: Vec<bool> = (0..head.mark_revisions.len())
            .map(|j| pick.mark(&path, j))
            .collect();
        if !decided.contains(&true) {
            return;
        }
        let removes = head
            .mark_revisions
            .iter()
            .zip(&decided)
            .any(|(r, &d)| d && r.kind.removes_text(accept));
        if !(removes && self.merge_paragraph_with_next(container, i)) {
            let mut top = self.blocks.clone();
            let _ = mutate_paragraph_in_top(&mut top, &path, |p| {
                let mut j = 0;
                p.mark_revisions.retain(|_| {
                    let keep = !decided.get(j).copied().unwrap_or(false);
                    j += 1;
                    keep
                });
            });
            self.blocks = top;
        }
    }

    /// Remove the MARK of paragraph `i` of `container`: merge it with the
    /// paragraph after it (the merged paragraph ends with the tail's mark
    /// and its revisions), remapping the comment anchors. `false` — and
    /// nothing done — when that mark cannot go: the next block is not a
    /// paragraph (a table, or the container's end) or the mark carries a
    /// section break. Shared by the mark resolution above and the tracked
    /// deletion of a reviewer's own inserted mark (issue #298).
    pub(crate) fn merge_paragraph_with_next(&mut self, container: &[PathStep], i: u32) -> bool {
        let Some(blocks) = container_blocks(self, container) else {
            return false;
        };
        let (Some(Block::Paragraph(head)), Some(Block::Paragraph(tail))) =
            (blocks.get(i as usize), blocks.get(i as usize + 1))
        else {
            return false;
        };
        if head.section_end.is_some() {
            return false;
        }
        let path = child(container, i);
        let head_len = head.text.len() as u32;
        let mut top = self.blocks.clone();
        if vanishes_whole(head) {
            /* The whole paragraph was the change (a deleted paragraph, a
            rejected inserted one): the next one survives untouched —
            clean source bytes included. */
            let _ = delete_block_at_path(&mut top, &path);
            self.blocks = top;
            self.remap_block_splice(container, i, 1, 0);
        } else {
            let merged = merge_pair(head, tail);
            let _ = delete_block_at_path(&mut top, &child(container, i + 1));
            let _ = replace_block_in_top(&mut top, &path, Block::Paragraph(merged));
            self.blocks = top;
            self.remap_paragraph_merge(&path, head_len, i + 1, 0);
        }
        true
    }
}

/// `container` + `Block(i)`.
fn child(container: &[PathStep], i: u32) -> BlockPath {
    let mut steps = container.to_vec();
    steps.push(PathStep::Block(i));
    BlockPath { steps }
}

/// The block list `container` addresses (empty = the body).
fn container_blocks(doc: &DocumentTree, container: &[PathStep]) -> Option<Vec<Block>> {
    parent_container_snapshot(doc, &child(container, 0))
}

/// An empty paragraph with nothing positioned in it: merging it away
/// leaves the next paragraph exactly as it was.
fn vanishes_whole(p: &Paragraph) -> bool {
    p.text.is_empty()
        && p.inline_objects.is_empty()
        && p.bookmarks.is_empty()
        && p.body_xml.is_none()
        && p.source_markup
            .as_deref()
            .is_none_or(|m| m.markers.is_empty())
}

/// `head` + `tail` for a resolved paragraph-mark revision: the head's
/// mark is gone. Issue #292 — exactly [`Paragraph::concat`] now: it
/// carries both sides' hyperlinks and revisions (shifted) and the head's
/// paragraph style — or, when the head's text was removed whole, the
/// tail's paragraph properties (the surviving paragraph is the tail's).
fn merge_pair(head: &Paragraph, tail: &Paragraph) -> Paragraph {
    head.concat(tail)
}

/// Resolve the text revisions of `para` whose index `pick` selects (see
/// [`DocumentTree::resolve_all_revisions`] step 1); the others stay
/// pending and shift with the removed text. Returns the text edits
/// performed, in order, for the caller's anchor remap.
fn resolve_text_revisions(
    para: &mut Paragraph,
    accept: bool,
    pick: impl Fn(usize) -> bool,
) -> Vec<TextEdit> {
    let mut revs = Vec::new();
    for (i, r) in std::mem::take(&mut para.revisions).into_iter().enumerate() {
        if pick(i) {
            revs.push(r);
        } else {
            para.revisions.push(r);
        }
    }
    for r in revs.iter().filter(|r| r.kind == RevisionKind::FormatChange) {
        if !accept {
            if let Some(prev) = &r.prev_attrs {
                restyle(para, r.start, r.end, prev);
            }
        } else {
            drop_format_change_records(para, r.start, r.end);
        }
    }
    let mut cuts: Vec<(u32, u32)> = revs
        .iter()
        .filter(|r| r.kind.removes_text(accept))
        .map(|r| (para.snap_offset(r.start), para.snap_offset(r.end)))
        .filter(|(s, e)| s < e)
        .collect();
    cuts.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(cuts.len());
    for (s, e) in cuts {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
        .into_iter()
        .rev()
        .map(|(s, e)| remove_text(para, s, e))
        .collect()
}

/// Issue #305 — THE way a resolved revision (and a reviewer's own
/// removed insertion, `tracked_delete_range`) takes bytes `[s, e)` out of
/// `para`: one [`Paragraph::splice_text`] (text + source markup) and one
/// overlay-shift rule ([`shift_overlays_after_removal`]) for everything
/// else, so the single-revision path and accept-all cannot disagree. The
/// returned edit is what the caller hands to
/// [`DocumentTree::remap_text_edit_record`].
pub(crate) fn remove_text(para: &mut Paragraph, s: u32, e: u32) -> TextEdit {
    let edit = para.splice_text(s, e.saturating_sub(s), "");
    shift_overlays_after_removal(para, edit.at, edit.removed);
    para.dirty = true;
    edit
}

/// Shift every byte-offset-bearing overlay of `para` across the removal
/// of `removed_len` bytes at `from` (the text and the source markup were
/// already spliced by [`Paragraph::splice_text`]): offsets at or past the
/// gap's end move left; a range boundary inside the gap clamps to its
/// start and a range left empty is dropped (spans, hyperlinks, fields,
/// pending revisions); an inline object — a single U+FFFC sentinel byte,
/// nothing to clamp onto — whose sentinel lay inside the gap is dropped
/// with it (issue #265, the `Paragraph::delete_text` rule), so no object
/// is left pointing at removed text (issue #305).
pub(crate) fn shift_overlays_after_removal(para: &mut Paragraph, from: u32, removed_len: u32) {
    let to = from + removed_len;
    let shift = |v: &mut u32| {
        if *v >= to {
            *v -= removed_len;
        } else if *v > from {
            *v = from;
        }
    };
    for s in &mut para.spans {
        shift(&mut s.start);
        shift(&mut s.end);
    }
    para.spans.retain(|s| s.start < s.end);
    para.inline_objects.retain_mut(|io| {
        if io.at >= to {
            io.at -= removed_len;
            true
        } else {
            io.at < from
        }
    });
    for h in &mut para.hyperlinks {
        shift(&mut h.start);
        shift(&mut h.end);
    }
    para.hyperlinks.retain(|h| h.start < h.end);
    for r in &mut para.revisions {
        shift(&mut r.start);
        shift(&mut r.end);
    }
    para.revisions.retain(|r| r.start < r.end);
    for f in &mut para.fields {
        shift(&mut f.start);
        shift(&mut f.end);
    }
    para.fields.retain(|f| f.start < f.end);
}

/// Issue #295 — an accepted formatting change keeps the new formatting
/// but drops its record: the `<w:rPrChange>` a source run carries in its
/// grab bag over `[s, e)` (`SpanStyle::for_typing` — the element a split
/// run carries on both halves goes from both). An engine-made change has
/// no such record.
fn drop_format_change_records(para: &mut Paragraph, s: u32, e: u32) {
    let (s, e) = (para.snap_offset(s), para.snap_offset(e));
    let recorded = para
        .spans
        .iter()
        .any(|r| r.start < e && s < r.end && r.style.for_typing() != r.style);
    if s < e && recorded {
        *para = para.restyle_with(s, e, |style| style.for_typing());
    }
}

/// Give bytes `[s, e)` of `para` the style `style` (a rejected
/// formatting change restores the recorded one).
pub(crate) fn restyle(para: &mut Paragraph, s: u32, e: u32, style: &SpanStyle) {
    let (s, e) = (para.snap_offset(s), para.snap_offset(e));
    if s >= e {
        return;
    }
    let mut spans = Vec::with_capacity(para.spans.len() + 2);
    for r in &para.spans {
        if r.end <= s || r.start >= e {
            spans.push(r.clone());
            continue;
        }
        if r.start < s {
            spans.push(StyleRun {
                end: s,
                ..r.clone()
            });
        }
        if r.end > e {
            spans.push(StyleRun {
                start: e,
                ..r.clone()
            });
        }
    }
    if *style != SpanStyle::default() {
        spans.push(StyleRun {
            start: s,
            end: e,
            style: style.clone(),
        });
    }
    spans.sort_by_key(|r| r.start);
    para.spans = spans;
    para.dirty = true;
}

/// The moves one resolution pass resolved: whether any, and their
/// `move_name`s (a single resolution resolves whole moves — both halves —
/// so its names are complete).
#[derive(Default)]
struct ResolvedMoves {
    any: bool,
    names: HashSet<String>,
}

impl ResolvedMoves {
    fn note(&mut self, r: &Revision) {
        if matches!(r.kind, RevisionKind::MoveFrom | RevisionKind::MoveTo) {
            self.any = true;
            if let Some(n) = &r.move_name {
                self.names.insert(n.clone());
            }
        }
    }
}

/// A positioned move-range marker: `(is moveFrom, is the range start)`.
fn move_range_role(xml: &[u8]) -> Option<(bool, bool)> {
    let tag = xml.strip_prefix(b"<w:move")?;
    let (from, rest) = if let Some(r) = tag.strip_prefix(b"FromRange") {
        (true, r)
    } else {
        (false, tag.strip_prefix(b"ToRange")?)
    };
    if rest.starts_with(b"Start") {
        Some((from, true))
    } else if rest.starts_with(b"End") {
        Some((from, false))
    } else {
        None
    }
}

/// The (entity-decoded) value of attribute `name` on the start tag `xml`
/// opens with.
fn xml_attr(xml: &[u8], name: &[u8]) -> Option<String> {
    let tag_end = xml.iter().position(|&b| b == b'>').unwrap_or(xml.len());
    let tag = &xml[..tag_end];
    let mut i = 0;
    while let Some(at) = tag[i..].windows(name.len()).position(|w| w == name) {
        let start = i + at;
        i = start + name.len();
        /* A whole attribute name: preceded by whitespace, followed by `=`. */
        if !tag[..start].last().is_some_and(u8::is_ascii_whitespace) {
            continue;
        }
        let rest = &tag[i..];
        let rest = rest.strip_prefix(b"=")?;
        let quote = *rest.first()?;
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        let value = &rest[1..];
        let close = value.iter().position(|&b| b == quote)?;
        let raw = String::from_utf8_lossy(&value[..close]);
        return Some(
            raw.replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&"),
        );
    }
    None
}
