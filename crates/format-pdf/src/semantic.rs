//! Issue #360 — what the document model knows beyond the glyphs: headings
//! (the PDF document outline), and the per-export collector that records
//! where they landed.
//!
//! The layout box tree carries geometry only. [`PdfSemantics`] is the
//! side table the caller (engine-wasm) builds from `engine::DocumentTree`,
//! indexed exactly like `para_texts` — by `ParagraphBox::
//! source_paragraph_id` — so a paragraph split across pages (head + tail
//! share the id) resolves to one entry. An empty table is the pre-#360
//! export: [`crate::export_pdf_with_media`] passes one and its output is
//! byte-identical.
//!
//! [`SemCtx`] rides the content emitters' `Res` (issue #360's only hook
//! into the paint walk): every paragraph the page emitters place reports
//! its absolute top-left once, so the outline's `/Dest` points at the
//! laid-out paragraph rather than a re-derived position.

use pdf_writer::{Pdf, Ref, TextStr};
use std::cell::RefCell;
use std::collections::HashMap;

/// Issue #360 — the document-model facts about one source paragraph that
/// the PDF needs and the layout box tree does not carry. Indexed by
/// `source_paragraph_id` in [`PdfSemantics::paragraphs`]; a missing entry
/// is the default (an ordinary body paragraph).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParagraphSemantics {
    /// 1-based outline level (`Heading 1` → 1, … `Heading 9` → 9, or the
    /// resolved `<w:outlineLvl>` + 1) — one `/Outlines` entry per heading.
    pub heading: Option<u8>,
    /// The heading's display text (tabs / breaks folded to spaces) — the
    /// outline entry title. A heading with an empty title gets no entry
    /// (Word parity: empty headings are not bookmarked).
    pub title: String,
}

/// Issue #360 — the semantic side table [`crate::export_pdf_document`]
/// consumes. `Default` is "nothing known": no outline, the pre-#360
/// output byte for byte.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdfSemantics {
    /// Indexed by `ParagraphBox::source_paragraph_id` (the same flat
    /// table `para_texts` is).
    pub paragraphs: Vec<ParagraphSemantics>,
}

impl PdfSemantics {
    /// The entry for `id`, when there is one.
    pub(crate) fn paragraph(&self, id: u32) -> Option<&ParagraphSemantics> {
        if id == layout::ParagraphBox::NO_SOURCE_ID {
            return None;
        }
        self.paragraphs.get(id as usize)
    }
}

/// Where a paragraph's first laid-out fragment landed: page index and the
/// PDF-space (`y` up) top-left corner of its box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Placement {
    pub page: usize,
    pub x: f32,
    pub top: f32,
}

#[derive(Default)]
struct State {
    page: usize,
    page_h: f32,
    /// First placement of every heading paragraph, by source id.
    placements: HashMap<u32, Placement>,
}

/// Issue #360 — the collector the content emitters report into. One per
/// content-building round (the issue #327 subset loop may rebuild the
/// contents; a fresh context per round keeps the record exact).
pub(crate) struct SemCtx<'s> {
    sem: &'s PdfSemantics,
    st: RefCell<State>,
}

impl<'s> SemCtx<'s> {
    pub fn new(sem: &'s PdfSemantics) -> Self {
        Self {
            sem,
            st: RefCell::new(State::default()),
        }
    }

    /// The page emitters are about to build page `index` (`page_h` tall).
    pub fn begin_page(&self, index: usize, page_h: f32) {
        let mut st = self.st.borrow_mut();
        st.page = index;
        st.page_h = page_h;
    }

    /// A paragraph box is being emitted with its layout-space (`y` down)
    /// top-left at `(x, top)`. Records the first fragment of a heading.
    pub fn paragraph_placed(&self, para: &layout::ParagraphBox, x: f32, top: f32) {
        let Some(meta) = self.sem.paragraph(para.source_paragraph_id) else {
            return;
        };
        if meta.heading.is_none() {
            return;
        }
        let mut st = self.st.borrow_mut();
        let placement = Placement {
            page: st.page,
            x,
            top: st.page_h - top,
        };
        st.placements
            .entry(para.source_paragraph_id)
            .or_insert(placement);
    }

    /// Close the round: what the writers need once every page is built.
    pub fn finish(self) -> Collected<'s> {
        Collected {
            sem: self.sem,
            placements: self.st.into_inner().placements,
        }
    }
}

/// What one content-building round collected, for the object writers.
pub(crate) struct Collected<'s> {
    sem: &'s PdfSemantics,
    placements: HashMap<u32, Placement>,
}

/// One outline entry before its objects are written.
struct OutlineEntry {
    level: u8,
    title: String,
    dest: Placement,
}

impl Collected<'_> {
    /// Every heading that reached a page, in document order (source id
    /// order — the body walk that numbers paragraphs is document order).
    fn outline_entries(&self) -> Vec<OutlineEntry> {
        let mut ids: Vec<u32> = self.placements.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
            .filter_map(|id| {
                let meta = self.sem.paragraph(id)?;
                let level = meta.heading?.clamp(1, 9);
                let title = meta.title.trim();
                (!title.is_empty()).then(|| OutlineEntry {
                    level,
                    title: title.to_string(),
                    dest: self.placements[&id],
                })
            })
            .collect()
    }

    /// Allocate the `/Outlines` tree's objects — `None` when there is no
    /// heading (nothing allocated: the pre-#360 object numbering). The
    /// catalog needs the root ref before the items are written, so
    /// planning and writing ([`OutlinePlan::write`]) are separate steps.
    pub fn plan_outline(&self, alloc: &mut impl FnMut() -> Ref) -> Option<OutlinePlan> {
        let entries = self.outline_entries();
        if entries.is_empty() {
            return None;
        }
        let root = alloc();
        let refs: Vec<Ref> = entries.iter().map(|_| alloc()).collect();
        Some(OutlinePlan {
            root,
            refs,
            entries,
        })
    }
}

/// An allocated, not yet written, document outline.
pub(crate) struct OutlinePlan {
    pub root: Ref,
    refs: Vec<Ref>,
    entries: Vec<OutlineEntry>,
}

impl OutlinePlan {
    /// Write the `/Outlines` tree (ISO 32000-1 §12.3.3): one item per
    /// heading, nested by level (a deeper level becomes a child of the
    /// nearest shallower heading before it; a skipped level nests
    /// directly), every item open, `/Dest [page /XYZ left top 0]` at
    /// the paragraph's laid-out top-left.
    pub fn write(&self, pdf: &mut Pdf, page_refs: &[Ref]) {
        let (entries, refs, root) = (&self.entries, &self.refs, self.root);
        /* Parent of each entry: the nearest earlier entry with a smaller
        level (a stack of open ancestors), else the root. */
        let mut parent: Vec<Option<usize>> = Vec::with_capacity(entries.len());
        let mut stack: Vec<usize> = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            while stack
                .last()
                .is_some_and(|&top| entries[top].level >= e.level)
            {
                stack.pop();
            }
            parent.push(stack.last().copied());
            stack.push(i);
        }
        let children = |p: Option<usize>| -> Vec<usize> {
            (0..entries.len()).filter(|&i| parent[i] == p).collect()
        };
        /* Every item is open, so `/Count` is the number of descendants. */
        let mut descendants = vec![0_i32; entries.len()];
        for i in (0..entries.len()).rev() {
            if let Some(p) = parent[i] {
                descendants[p] += 1 + descendants[i];
            }
        }
        let top = children(None);
        {
            let mut outline = pdf.outline(root);
            outline.first(refs[top[0]]);
            outline.last(refs[*top.last().expect("non-empty")]);
            outline.count(entries.len() as i32);
        }
        for (i, e) in entries.iter().enumerate() {
            let siblings = children(parent[i]);
            let pos = siblings.iter().position(|&s| s == i).expect("self");
            let kids = children(Some(i));
            let mut item = pdf.outline_item(refs[i]);
            item.title(TextStr(&e.title));
            item.parent(parent[i].map_or(root, |p| refs[p]));
            if pos > 0 {
                item.prev(refs[siblings[pos - 1]]);
            }
            if let Some(&next) = siblings.get(pos + 1) {
                item.next(refs[next]);
            }
            if let (Some(&first), Some(&last)) = (kids.first(), kids.last()) {
                item.first(refs[first]);
                item.last(refs[last]);
                item.count(descendants[i]);
            }
            item.dest()
                .page(page_refs[e.dest.page])
                .xyz(e.dest.x, e.dest.top, None);
        }
    }
}
