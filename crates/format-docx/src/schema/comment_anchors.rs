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
//! Issue #282 — the same plan covers paragraphs written from their source
//! bytes:
//!
//! - a clean paragraph (or a clean table holding one) that a tree endpoint
//!   lands in, with no verbatim byte carrying it (a comment added to an
//!   untouched paragraph, a reply), gets the endpoint spliced INTO its
//!   source bytes ([`needs_patch`] / [`table_needs_patch`], the splice in
//!   `writer::patch_clean_paragraph` via [`super::anchor_patch`]) — the
//!   regenerate path runs in [`AnchorMode::Patch`] to say where;
//! - a deleted comment ([`engine::DocumentTree::deleted_comments`]) has its
//!   anchors stripped from the whole written body afterwards
//!   ([`strip_deleted`]) — clean paragraphs, clean tables, block-level
//!   fragments and the always-kept spans (a form field's content span, a
//!   run-level content control) alike. Only tombstoned ids are stripped,
//!   so an anchor the source already left dangling still round-trips.
//!
//! [`CommentPlan`] is computed once per `document.xml` write (from the tree)
//! and published for the duration of the write, the
//! `WRITE_INHERITED_DIRECTIONS` pattern — the paragraph serializer has no
//! document in hand.

use engine::{
    Block, CommentAnchor, CommentAnchorKind, DocumentTree, Paragraph, PathStep, SourceMarker, Table,
};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::cell::{Cell, RefCell};
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
    /// Issue #282 — tombstoned comments ([`DocumentTree::deleted_comments`]
    /// that are not live again): their anchors are stripped from the body.
    deleted: HashSet<u32>,
    /// Issue #282 — clean tables (written from their source bytes) holding
    /// a paragraph with an endpoint to splice in. Only the OUTERMOST clean
    /// table on an endpoint's path: its bytes contain the inner ones.
    patch_tables: HashSet<usize>,
    /// Issue #282 — the source `<w:document>` root attributes: a patched
    /// paragraph is re-read under them to verify the splice.
    root_attrs: Vec<(String, String)>,
}

fn para_key(p: &Paragraph) -> usize {
    p as *const Paragraph as usize
}

fn table_key(t: &Table) -> usize {
    t as *const Table as usize
}

/// The outermost table on `path` written from its source bytes (clean),
/// if any.
fn outermost_clean_table<'a>(doc: &'a DocumentTree, path: &engine::BlockPath) -> Option<&'a Table> {
    let mut prefix = engine::BlockPath::root();
    for step in &path.steps {
        prefix = prefix.push(*step);
        if let PathStep::Block(_) = step
            && let Some(t) = doc.table_at_path(&prefix)
            && !t.dirty
            && t.source_xml.is_some()
        {
            return Some(t);
        }
    }
    None
}

/// Every block of `blocks`, parents first. A table the writer re-emits
/// from its source bytes (clean) is not descended into: `f` sees its bytes
/// whole. A regenerated table's row / cell passthrough markup (issue #248
/// — whitespace, range markers, row / cell `sdt` ends between them) goes
/// to `frag`, then its cells are walked.
fn walk_blocks<'a>(
    blocks: impl IntoIterator<Item = &'a Block>,
    f: &mut dyn FnMut(&'a Block),
    frag: &mut dyn FnMut(&'a engine::BodyPassthrough),
) {
    for b in blocks {
        f(b);
        if let Block::Table(t) = b
            && (t.dirty || t.source_xml.is_none())
        {
            for row in &t.rows {
                if let Some(bx) = row
                    .source_markup
                    .as_deref()
                    .and_then(|m| m.body_xml.as_deref())
                {
                    frag(bx);
                }
                for cell in &row.cells {
                    if let Some(bx) = cell
                        .source_markup
                        .as_deref()
                        .and_then(|m| m.body_xml.as_deref())
                    {
                        frag(bx);
                    }
                    walk_blocks(&cell.blocks, f, frag);
                }
            }
        }
    }
}

/// Every comment anchor in the `Verbatim` fragments of `bx`.
fn scan_passthrough(bx: &engine::BodyPassthrough, out: &mut HashSet<(CommentAnchorKind, u32)>) {
    for frag in bx.before.iter().chain(bx.after.iter()) {
        if let engine::BodyFragment::Verbatim { xml } = frag {
            scan_fragment(xml, out);
        }
    }
}

/// Cheap pre-filter before [`scan_fragment`].
fn mentions_comment(xml: &[u8]) -> bool {
    xml.windows(9).any(|w| w == b"w:comment")
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
    /// The plan for writing `doc`'s body (`root_attrs`: the source root's
    /// attributes, see [`Self::root_attrs`]).
    pub fn for_document(doc: &DocumentTree, root_attrs: &[(String, String)]) -> Self {
        let mut plan = Self {
            live: doc.comment_defs.keys().copied().collect(),
            root_attrs: root_attrs.to_vec(),
            ..Self::default()
        };
        /* Issue #282 — endpoints inside a clean table, keyed by that
        table: spliced into its bytes unless a verbatim byte carries them. */
        let mut in_clean_tables: Vec<(usize, CommentAnchorKind, u32)> = Vec::new();
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
                    if let Some(t) = outermost_clean_table(doc, &pos.path) {
                        in_clean_tables.push((table_key(t), kind, r.id));
                    }
                }
            }
        }
        plan.deleted = doc
            .deleted_comments
            .iter()
            .copied()
            .filter(|id| !plan.live.contains(id))
            .collect();
        for anchors in plan.by_para.values_mut() {
            anchors.sort_by_key(|a| (a.at, a.rank));
        }
        /* What verbatim bytes already carry. */
        let present = RefCell::new(HashSet::new());
        walk_blocks(
            doc.blocks.iter(),
            &mut |b| {
                let mut present = present.borrow_mut();
                let present = &mut *present;
                if let Some(bx) = b.body_xml() {
                    scan_passthrough(bx, present);
                }
                let p = match b {
                    Block::Paragraph(p) => p,
                    Block::Table(t) => {
                        /* A clean table re-emits its source bytes whole. */
                        if !t.dirty
                            && let Some(src) = t.source_xml.as_deref()
                            && mentions_comment(src)
                        {
                            scan_fragment(src, present);
                        }
                        return;
                    }
                };
                /* A clean paragraph re-emits its source bytes whole. */
                if !p.dirty
                    && let Some(src) = p.source_xml.as_deref()
                {
                    if mentions_comment(src) {
                        scan_fragment(src, present);
                    }
                    return;
                }
                let Some(m) = p.source_markup.as_deref() else {
                    return;
                };
                let valid = m.offsets_valid(p.text.len());
                for mk in &m.markers {
                    match mk.comment {
                        Some(c) if valid && plan.verified(p, mk.at, c) => {
                            present.insert((c.kind, c.id));
                        }
                        /* Issues #244 / #245 — markup the writer always keeps
                        (a form field's `begin … end` span, a content control's
                        ends) may hold anchors of its own. */
                        None if mk.role.must_survive() && mentions_comment(&mk.xml) => {
                            scan_fragment(&mk.xml, present);
                        }
                        _ => {}
                    }
                }
            },
            &mut |bx| scan_passthrough(bx, &mut present.borrow_mut()),
        );
        plan.present = present.into_inner();
        plan.patch_tables = in_clean_tables
            .into_iter()
            .filter(|(_, kind, id)| !plan.present.contains(&(*kind, *id)))
            .map(|(t, _, _)| t)
            .collect();
        plan
    }

    /// Issue #282 — `true` when `p`, written from its source bytes, has a
    /// tree endpoint no verbatim byte carries (see [`needs_patch`]).
    pub fn needs_patch(&self, p: &Paragraph) -> bool {
        !p.dirty && p.source_xml.is_some() && !self.missing(p).is_empty()
    }

    /// The tree endpoints of `p` no verbatim byte carries.
    fn missing(&self, p: &Paragraph) -> Vec<TreeAnchor> {
        self.by_para
            .get(&para_key(p))
            .map(|v| {
                v.iter()
                    .filter(|a| !self.present.contains(&(a.kind, a.id)))
                    .copied()
                    .collect()
            })
            .unwrap_or_default()
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
    static ANCHOR_MODE: Cell<AnchorMode> = const { Cell::new(AnchorMode::Normal) };
}

/// Issue #282 — what the anchor hooks do for the paragraph being
/// serialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorMode {
    /// A regenerated paragraph: comment-anchor markers replay only when
    /// verified, missing tree endpoints are synthesized (issue #243).
    Normal,
    /// A clean paragraph regenerated as it stands — every recorded marker
    /// kept, nothing synthesized: the baseline a patch is diffed against.
    Verbatim,
    /// A clean paragraph plus its missing tree endpoints: every recorded
    /// marker kept (its offset is the source's own), only endpoints no
    /// verbatim byte carries synthesized (with their reference runs).
    Patch,
}

/// Run `f` with the anchor hooks in `mode`, restoring the previous mode.
pub fn with_mode<R>(mode: AnchorMode, f: impl FnOnce() -> R) -> R {
    struct Restore(AnchorMode);
    impl Drop for Restore {
        fn drop(&mut self) {
            ANCHOR_MODE.with(|m| m.set(self.0));
        }
    }
    let _restore = Restore(ANCHOR_MODE.with(|m| m.replace(mode)));
    f()
}

fn mode() -> AnchorMode {
    ANCHOR_MODE.with(Cell::get)
}

/// Issue #282 — `true` when clean paragraph `p` has a tree endpoint no
/// verbatim byte carries (it must be patched, not replayed).
pub fn needs_patch(p: &Paragraph) -> bool {
    WRITE_COMMENT_PLAN.with(|c| {
        c.borrow()
            .as_ref()
            .is_some_and(|plan| !plan.missing(p).is_empty())
    })
}

/// Issue #282 — `true` when clean table `t` holds a paragraph that
/// [`needs_patch`].
pub fn table_needs_patch(t: &Table) -> bool {
    WRITE_COMMENT_PLAN.with(|c| {
        c.borrow()
            .as_ref()
            .is_some_and(|plan| plan.patch_tables.contains(&table_key(t)))
    })
}

/// Issue #282 — the source root attributes of the body being written
/// (empty outside a published write).
pub fn root_attrs() -> Vec<(String, String)> {
    WRITE_COMMENT_PLAN.with(|c| {
        c.borrow()
            .as_ref()
            .map(|plan| plan.root_attrs.clone())
            .unwrap_or_default()
    })
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
pub fn publish(doc: &DocumentTree, root_attrs: &[(String, String)]) -> CommentPlanScope {
    let plan = CommentPlan::for_document(doc, root_attrs);
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

/// The tree endpoints of `p` that must be synthesized (none in
/// [`AnchorMode::Verbatim`]).
pub fn paragraph_anchors(p: &Paragraph) -> ParagraphAnchors {
    if mode() == AnchorMode::Verbatim {
        return ParagraphAnchors {
            synthesize: Vec::new(),
        };
    }
    WRITE_COMMENT_PLAN.with(|c| ParagraphAnchors {
        synthesize: c
            .borrow()
            .as_ref()
            .map(|plan| plan.missing(p))
            .unwrap_or_default(),
    })
}

/// `true` when marker `mk` (at its offset in `p`) may be written: every
/// ordinary marker; a comment-anchor marker only when verified — or, for
/// a clean paragraph's source ([`AnchorMode::Verbatim`] /
/// [`AnchorMode::Patch`]), always (a tombstoned one is stripped from the
/// whole body afterwards, [`strip_deleted`]).
pub fn keep_marker(p: &Paragraph, mk: &SourceMarker) -> bool {
    let Some(c) = mk.comment else {
        return true;
    };
    if mode() != AnchorMode::Normal {
        return true;
    }
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

/// After the range END of comment `id` was written (`synthesized`: by the
/// planner, not replayed from a marker): synthesize its reference run
/// when no verbatim byte carries one and none was synthesized yet (Word's
/// `CommentReference` character style). A clean paragraph's source
/// ([`AnchorMode::Verbatim`] / [`AnchorMode::Patch`]) gains one only
/// behind a synthesized end — its own bytes stay as they are.
pub fn after_range_end(id: u32, synthesized: bool, out: &mut String) {
    match mode() {
        AnchorMode::Verbatim => return,
        AnchorMode::Patch if !synthesized => return,
        _ => {}
    }
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

/// Issue #282 — remove every anchor of a tombstoned comment from the
/// written `body`: `<w:commentRangeStart/>` / `<w:commentRangeEnd/>` and
/// `<w:commentReference/>` with a deleted `w:id`, and the run holding such
/// a reference when nothing else is left in it (`<w:rPr>` and whitespace
/// aside). Pure deletion — every other byte stays. A body that does not
/// parse is left alone (the well-formedness guard reports it).
pub fn strip_deleted(body: &mut String) {
    let deleted = WRITE_COMMENT_PLAN.with(|c| {
        c.borrow()
            .as_ref()
            .map(|plan| plan.deleted.clone())
            .unwrap_or_default()
    });
    if deleted.is_empty() || !mentions_comment(body.as_bytes()) {
        return;
    }
    if let Some(cuts) = deleted_anchor_spans(body.as_bytes(), &deleted) {
        let mut out = String::with_capacity(body.len());
        let mut cursor = 0;
        for (lo, hi) in cuts {
            out.push_str(&body[cursor..lo]);
            cursor = hi;
        }
        out.push_str(&body[cursor..]);
        *body = out;
    }
}

/// One open `<w:r>` of the [`deleted_anchor_spans`] walk.
struct RunFrame {
    start: usize,
    /// Doomed `<w:commentReference/>` spans inside it.
    refs: Vec<(usize, usize)>,
    /// Anything else inside it (outside `<w:rPr>`).
    other: bool,
    rpr_depth: u32,
}

/// The byte spans of `xml` to delete (sorted, disjoint): see
/// [`strip_deleted`]. `None` on a parse error.
pub fn deleted_anchor_spans(xml: &[u8], deleted: &HashSet<u32>) -> Option<Vec<(usize, usize)>> {
    let doomed_id = |e: &quick_xml::events::BytesStart| {
        e.attributes()
            .flatten()
            .find(|a| a.key.as_ref() == b"w:id")
            .and_then(|a| {
                std::str::from_utf8(&a.value)
                    .ok()?
                    .trim()
                    .parse::<u32>()
                    .ok()
            })
            .is_some_and(|id| deleted.contains(&id))
    };
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let mut runs: Vec<RunFrame> = Vec::new();
    /* A doomed range marker written as a start / end tag pair. */
    let mut open_marker: Option<(usize, Vec<u8>, u32)> = None;
    let mut prev = 0usize;
    loop {
        let event = reader.read_event_into(&mut buf).ok()?;
        let pos = reader.buffer_position() as usize;
        if let Some((start, name, depth)) = open_marker.as_mut() {
            match &event {
                Event::Start(_) => *depth += 1,
                Event::End(e) if *depth == 0 && e.name().as_ref() == name.as_slice() => {
                    cuts.push((*start, pos));
                    open_marker = None;
                }
                Event::End(_) => *depth -= 1,
                Event::Eof => return None,
                _ => {}
            }
            prev = pos;
            buf.clear();
            continue;
        }
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                let empty = matches!(event, Event::Empty(_));
                let name = e.name();
                let name = name.as_ref();
                let marker = matches!(name, b"w:commentRangeStart" | b"w:commentRangeEnd");
                if (marker || name == b"w:commentReference") && doomed_id(e) {
                    if name == b"w:commentReference"
                        && let Some(run) = runs.last_mut()
                        && run.rpr_depth == 0
                    {
                        if empty {
                            run.refs.push((prev, pos));
                        } else {
                            run.other = true;
                        }
                    } else if empty {
                        cuts.push((prev, pos));
                    } else {
                        open_marker = Some((prev, name.to_vec(), 0));
                    }
                } else if name == b"w:r" && !empty {
                    if let Some(run) = runs.last_mut() {
                        run.other = true;
                    }
                    runs.push(RunFrame {
                        start: prev,
                        refs: Vec::new(),
                        other: false,
                        rpr_depth: 0,
                    });
                } else if let Some(run) = runs.last_mut() {
                    if name == b"w:rPr" && !empty && run.rpr_depth == 0 {
                        run.rpr_depth = 1;
                    } else if run.rpr_depth > 0 {
                        if !empty {
                            run.rpr_depth += 1;
                        }
                    } else if name != b"w:rPr" {
                        run.other = true;
                    }
                }
            }
            Event::End(e) => {
                if e.name().as_ref() == b"w:r" {
                    if let Some(run) = runs.pop() {
                        if !run.refs.is_empty() && !run.other {
                            cuts.push((run.start, pos));
                        } else {
                            cuts.extend(run.refs);
                        }
                    }
                } else if let Some(run) = runs.last_mut()
                    && run.rpr_depth > 0
                {
                    run.rpr_depth -= 1;
                }
            }
            Event::Text(t) => {
                if let Some(run) = runs.last_mut()
                    && run.rpr_depth == 0
                    && !t.iter().all(u8::is_ascii_whitespace)
                {
                    run.other = true;
                }
            }
            Event::CData(_) => {
                if let Some(run) = runs.last_mut() {
                    run.other = true;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        prev = pos;
        buf.clear();
    }
    cuts.sort_unstable();
    /* Spans nest (a reference inside a removed run never is — the run
    takes its place); drop any overlap defensively. */
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(cuts.len());
    for (lo, hi) in cuts {
        match out.last() {
            Some(&(_, last_hi)) if lo < last_hi => {}
            _ => out.push((lo, hi)),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(xml: &str, ids: &[u32]) -> String {
        let deleted: HashSet<u32> = ids.iter().copied().collect();
        let cuts = deleted_anchor_spans(xml.as_bytes(), &deleted).expect("parses");
        let mut out = String::new();
        let mut cursor = 0;
        for (lo, hi) in cuts {
            out.push_str(&xml[cursor..lo]);
            cursor = hi;
        }
        out.push_str(&xml[cursor..]);
        out
    }

    #[test]
    fn strips_only_tombstoned_anchors_and_empty_reference_runs() {
        let xml = concat!(
            r#"<w:p><w:commentRangeStart w:id="1"/><w:commentRangeStart w:id="2"/>"#,
            r#"<w:r><w:t>x</w:t></w:r><w:commentRangeEnd w:id="1"/><w:commentRangeEnd w:id="2"/>"#,
            r#"<w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="1"/></w:r>"#,
            r#"<w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="2"/></w:r>"#,
            r#"<w:r><w:t>y</w:t><w:commentReference w:id="1"/></w:r></w:p>"#,
        );
        assert_eq!(
            strip(xml, &[1]),
            concat!(
                r#"<w:p><w:commentRangeStart w:id="2"/>"#,
                r#"<w:r><w:t>x</w:t></w:r><w:commentRangeEnd w:id="2"/>"#,
                r#"<w:r><w:rPr><w:rStyle w:val="CommentReference"/></w:rPr><w:commentReference w:id="2"/></w:r>"#,
                r#"<w:r><w:t>y</w:t></w:r></w:p>"#,
            )
        );
        assert_eq!(strip(xml, &[]), xml);
        /* Pretty-printed reference run and a start/end-tag marker. */
        let pretty = "<w:p>\n  <w:commentRangeStart w:id=\"3\"></w:commentRangeStart>\n  <w:r>\n    <w:commentReference w:id=\"3\"/>\n  </w:r>\n</w:p>";
        assert_eq!(strip(pretty, &[3]), "<w:p>\n  \n  \n</w:p>");
    }
}
