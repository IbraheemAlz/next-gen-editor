//! Issues #262 / #247 — resolving tracked changes structurally: the
//! document-wide accept / reject ([`DocumentTree::resolve_all_revisions`])
//! and the paragraph-MARK revision (a tracked paragraph split or merge,
//! [`crate::Paragraph::mark_revision`]).
//!
//! Every text removal goes through [`Paragraph::splice_text`] and the
//! returned edit through [`DocumentTree::remap_text_edit_record`]; every
//! merge through [`DocumentTree::remap_paragraph_merge`] (or, for a head
//! that vanishes whole, [`DocumentTree::remap_block_splice`]) — so the
//! source markup and the comment anchors stay in step with the text
//! (issues #250 / #252 / #253).

use crate::text_remap::TextEdit;
use crate::{
    Block, BlockPath, DocumentTree, Hyperlink, Paragraph, PathStep, Revision, RevisionKind,
    SpanStyle, StyleRun, delete_block_at_path, mutate_paragraph_in_top, parent_container_snapshot,
    replace_block_in_top, shift_paragraph_offsets_after,
};

impl DocumentTree {
    /// `true` when any body paragraph (table cells included) carries a
    /// tracked change — text overlay or paragraph mark.
    pub fn has_revisions(&self) -> bool {
        let mut any = false;
        crate::fields::for_each_paragraph_deep(&self.blocks, &mut |_, p| {
            any |= !p.revisions.is_empty() || p.mark_revision.is_some();
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
    ///    loses its revision.
    /// 3. With every move resolved, the orphaned in-paragraph move-range
    ///    markers (`<w:moveFromRangeStart/>` …) are dropped.
    pub fn resolve_all_revisions(&self, accept: bool) -> Self {
        let mut out = self.clone();
        let mut had_move = false;
        let mut paths = Vec::new();
        crate::fields::for_each_paragraph_deep(&out.blocks, &mut |path, p| {
            had_move |= p
                .revisions
                .iter()
                .chain(p.mark_revision.as_ref())
                .any(|r| matches!(r.kind, RevisionKind::MoveFrom | RevisionKind::MoveTo));
            if !p.revisions.is_empty() {
                paths.push(path);
            }
        });
        for path in paths {
            let mut edits = Vec::new();
            let mut blocks = out.blocks.clone();
            let _ = mutate_paragraph_in_top(&mut blocks, &path, |para| {
                edits = resolve_text_revisions(para, accept);
            });
            out.blocks = blocks;
            for e in edits {
                out.remap_text_edit_record(&path, e);
            }
        }
        out.resolve_marks_in(&[], accept);
        if had_move {
            let mut marked = Vec::new();
            crate::fields::for_each_paragraph_deep(&out.blocks, &mut |path, p| {
                if p.source_markup
                    .as_deref()
                    .is_some_and(|m| m.markers.iter().any(|mk| is_move_range(&mk.xml)))
                {
                    marked.push(path);
                }
            });
            let mut blocks = out.blocks.clone();
            for path in marked {
                let _ = mutate_paragraph_in_top(&mut blocks, &path, |para| {
                    if let Some(m) = para.source_markup.as_deref_mut() {
                        m.markers.retain(|mk| !is_move_range(&mk.xml));
                    }
                });
            }
            out.blocks = blocks;
        }
        out.with_list_markers_refreshed()
    }

    /// Issue #262 — accept / reject the paragraph-mark revision of
    /// top-level paragraph `block` (the single-revision path; see
    /// [`Self::resolve_all_revisions`] for the semantics).
    pub(crate) fn resolve_mark_revision_at(&self, block: u32, accept: bool) -> Self {
        let mut out = self.clone();
        out.resolve_mark(&[], block, accept);
        out.with_list_markers_refreshed()
    }

    /// Resolve every paragraph-mark revision in the container `container`
    /// (the steps leading INTO a block list: empty = the body), deepest
    /// first, from the container's end.
    fn resolve_marks_in(&mut self, container: &[PathStep], accept: bool) {
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
                        self.resolve_marks_in(&inner, accept);
                    }
                }
                Some(Block::Paragraph(p)) if p.mark_revision.is_some() => {
                    self.resolve_mark(container, i, accept);
                }
                _ => {}
            }
        }
    }

    /// Resolve the mark revision of paragraph `i` of `container`.
    fn resolve_mark(&mut self, container: &[PathStep], i: u32, accept: bool) {
        let Some(blocks) = container_blocks(self, container) else {
            return;
        };
        let Some(Block::Paragraph(head)) = blocks.get(i as usize) else {
            return;
        };
        let Some(rev) = &head.mark_revision else {
            return;
        };
        let next = match blocks.get(i as usize + 1) {
            Some(Block::Paragraph(p)) => Some(p),
            _ => None,
        };
        let path = child(container, i);
        match next {
            Some(tail) if rev.kind.removes_text(accept) && head.section_end.is_none() => {
                let head_len = head.text.len() as u32;
                let mut top = self.blocks.clone();
                if vanishes_whole(head) {
                    /* The whole paragraph was the change (a deleted
                    paragraph, a rejected inserted one): the next one
                    survives untouched — clean source bytes included. */
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
            }
            _ => {
                let mut top = self.blocks.clone();
                let _ = mutate_paragraph_in_top(&mut top, &path, |p| p.mark_revision = None);
                self.blocks = top;
            }
        }
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
/// mark is gone. [`Paragraph::concat`] plus what it does not carry: both
/// sides' hyperlinks and revisions (shifted), the head's paragraph style —
/// or, when the head's text was removed whole, the tail's paragraph
/// properties (the surviving paragraph is the tail's).
fn merge_pair(head: &Paragraph, tail: &Paragraph) -> Paragraph {
    let shift = head.text.len() as u32;
    let mut m = head.concat(tail);
    m.hyperlinks = head.hyperlinks.clone();
    m.hyperlinks
        .extend(tail.hyperlinks.iter().map(|h| Hyperlink {
            start: h.start + shift,
            end: h.end + shift,
            target: h.target.clone(),
        }));
    m.revisions = head.revisions.clone();
    m.revisions.extend(tail.revisions.iter().map(|r| Revision {
        start: r.start + shift,
        end: r.end + shift,
        ..r.clone()
    }));
    if head.text.is_empty() {
        m.props = tail.props.clone();
        m.style_id = tail.style_id.clone();
        m.direct_overrides = tail.direct_overrides.clone();
        m.list_item = tail.list_item;
        m.resolved_marker = tail.resolved_marker.clone();
        m.resolved_list_indent = tail.resolved_list_indent;
        if let Some(mk) = m.source_markup.as_deref_mut() {
            let t = tail.source_markup.as_deref();
            mk.attrs = t.map(|t| t.attrs.clone()).unwrap_or_default();
            mk.ppr = t.and_then(|t| t.ppr.clone());
        }
    } else {
        m.style_id = head.style_id.clone();
        m.direct_overrides = head.direct_overrides.clone();
    }
    m
}

/// Resolve every text revision of `para` (see
/// [`DocumentTree::resolve_all_revisions`] step 1); returns the text
/// edits performed, in order, for the caller's anchor remap.
fn resolve_text_revisions(para: &mut Paragraph, accept: bool) -> Vec<TextEdit> {
    let revs = std::mem::take(&mut para.revisions);
    if !accept {
        for r in &revs {
            if r.kind == RevisionKind::FormatChange
                && let Some(prev) = &r.prev_attrs
            {
                restyle(para, r.start, r.end, prev);
            }
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

/// Remove bytes `[s, e)` of `para` with every overlay: inline objects
/// whose anchor was removed go with it, the rest shift.
pub(crate) fn remove_text(para: &mut Paragraph, s: u32, e: u32) -> TextEdit {
    para.inline_objects.retain(|o| o.at < s || o.at >= e);
    let edit = para.splice_text(s, e - s, "");
    shift_paragraph_offsets_after(para, edit.at, edit.removed);
    para.dirty = true;
    edit
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

fn is_move_range(xml: &[u8]) -> bool {
    xml.starts_with(b"<w:moveFromRange") || xml.starts_with(b"<w:moveToRange")
}
