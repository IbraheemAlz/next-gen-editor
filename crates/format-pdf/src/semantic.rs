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

use pdf_writer::types::{ActionType, AnnotationFlags, AnnotationType};
use pdf_writer::writers::Destination;
use pdf_writer::{Name, Pdf, Rect, Ref, Str, TextStr};
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
    /// Hyperlink ranges over the paragraph's source text (`<w:hyperlink>`,
    /// `engine::Hyperlink`), non-overlapping — one `/Link` annotation per
    /// laid-out line each range touches.
    pub links: Vec<LinkSpan>,
    /// Bookmark names anchored in this paragraph (`<w:bookmarkStart
    /// w:name>`) — the destinations of internal links. A bookmark resolves
    /// to the top-left of the paragraph's first laid-out fragment.
    pub bookmarks: Vec<String>,
}

/// Issue #360 — one hyperlink range: `[start, end)` byte offsets into the
/// paragraph's source text (the `para_texts` entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkSpan {
    pub start: u32,
    pub end: u32,
    pub target: LinkTarget,
}

/// Issue #360 — where a hyperlink goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// An external target (the relationship's `Target`): a `/URI` action.
    Uri(String),
    /// An internal target (`<w:hyperlink w:anchor>`): a `/Dest` at the
    /// bookmark's paragraph. A name no paragraph carries gets no
    /// annotation (never a dead link).
    Bookmark(String),
}

/// Issue #360 — the semantic side table [`crate::export_pdf_document`]
/// consumes. `Default` is "nothing known": no outline, the pre-#360
/// output byte for byte.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdfSemantics {
    /// Indexed by `ParagraphBox::source_paragraph_id` (the same flat
    /// table `para_texts` is).
    pub paragraphs: Vec<ParagraphSemantics>,
    /// Document information (`docProps/core.xml`).
    pub metadata: DocumentMetadata,
}

/// Issue #360 — document information from the source package's core
/// properties (`docProps/core.xml`). Every field is optional; a document
/// with none of title / author / subject / keywords writes no `/Info`
/// (X-3 excepted — it always has one) and an unchanged XMP packet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentMetadata {
    /// `dc:title` → `/Title` + XMP `dc:title`.
    pub title: Option<String>,
    /// `dc:creator` → `/Author` + XMP `dc:creator`.
    pub author: Option<String>,
    /// `dc:subject` → `/Subject` + XMP `dc:description` (the ISO 19005
    /// Info ↔ XMP pairing).
    pub subject: Option<String>,
    /// `cp:keywords` → `/Keywords` + XMP `pdf:Keywords`.
    pub keywords: Option<String>,
    /// The document's natural language (BCP 47 — `dc:language`, else the
    /// `<w:docDefaults>` `w:lang`): a tagged export's catalog `/Lang`.
    pub lang: Option<String>,
}

impl DocumentMetadata {
    /// Whether any `/Info` entry exists.
    pub(crate) fn has_info(&self) -> bool {
        self.title.is_some()
            || self.author.is_some()
            || self.subject.is_some()
            || self.keywords.is_some()
    }

    /// Every value with control characters (not representable in the XMP
    /// packet's XML) turned into spaces and trimmed, blanks dropped — so
    /// `/Info` and XMP carry the same string.
    pub(crate) fn cleaned(&self) -> Self {
        let clean = |v: &Option<String>| {
            v.as_deref().and_then(|s| {
                let t: String = s
                    .chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect();
                let t = t.trim();
                (!t.is_empty()).then(|| t.to_string())
            })
        };
        Self {
            title: clean(&self.title),
            author: clean(&self.author),
            subject: clean(&self.subject),
            keywords: clean(&self.keywords),
            lang: clean(&self.lang),
        }
    }
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

/// One laid-out line's share of a hyperlink: the union of the link's
/// glyph boxes on that line, PDF space `[x0, y0, x1, y1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LinkHit {
    pub para: u32,
    pub link: usize,
    pub page: usize,
    pub rect: [f32; 4],
}

#[derive(Default)]
struct State {
    page: usize,
    page_h: f32,
    /// First placement of every heading / bookmarked paragraph, by
    /// source id.
    placements: HashMap<u32, Placement>,
    /// Every line × link rectangle, in paint order.
    links: Vec<LinkHit>,
}

/// Issue #360 — the collector the content emitters report into. One per
/// content-building round (the issue #327 subset loop may rebuild the
/// contents; a fresh context per round keeps the record exact).
pub(crate) struct SemCtx<'s> {
    sem: &'s PdfSemantics,
    texts: &'s [&'s str],
    /// `false` for PDF/X-3 (no link annotations — see [`plan_links`]).
    ///
    /// [`plan_links`]: Collected::plan_links
    links: bool,
    st: RefCell<State>,
}

impl<'s> SemCtx<'s> {
    pub fn new(sem: &'s PdfSemantics, texts: &'s [&'s str], links: bool) -> Self {
        Self {
            sem,
            texts,
            links,
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
    /// top-left at `(x, top)`. Records the first fragment of a heading or
    /// bookmarked paragraph, and every line's share of each hyperlink:
    /// the union of the glyph boxes (pen position × `x_advance`, the line
    /// box's full height) whose source byte falls in the link — BiDi runs
    /// are visual, so this is a per-glyph test, not a range clip.
    pub fn paragraph_placed(&self, para: &layout::ParagraphBox, x: f32, top: f32) {
        let id = para.source_paragraph_id;
        let Some(meta) = self.sem.paragraph(id) else {
            return;
        };
        let mut st = self.st.borrow_mut();
        let (page, page_h) = (st.page, st.page_h);
        if meta.heading.is_some() || !meta.bookmarks.is_empty() {
            let placement = Placement {
                page,
                x,
                top: page_h - top,
            };
            st.placements.entry(id).or_insert(placement);
        }
        if !self.links || meta.links.is_empty() {
            return;
        }
        for line in &para.lines {
            let line_x = x + line.origin.x;
            let line_top = top + line.origin.y;
            /* Per link: [min x, max x] on this line. */
            let mut spans: Vec<Option<(f32, f32)>> = vec![None; meta.links.len()];
            let mut pen = 0.0_f32;
            for run in &line.runs {
                for g in &run.glyphs {
                    let at = run.source_range.start + g.cluster;
                    if let Some(i) = meta.links.iter().position(|l| l.start <= at && at < l.end)
                        && g.x_advance > 0.0
                    {
                        let (x0, x1) = (line_x + pen, line_x + pen + g.x_advance);
                        let span = spans[i].get_or_insert((x0, x1));
                        span.0 = span.0.min(x0);
                        span.1 = span.1.max(x1);
                    }
                    pen += g.x_advance;
                }
            }
            for (link, span) in spans.into_iter().enumerate() {
                if let Some((x0, x1)) = span {
                    st.links.push(LinkHit {
                        para: id,
                        link,
                        page,
                        rect: [x0, page_h - (line_top + line.height), x1, page_h - line_top],
                    });
                }
            }
        }
    }

    /// Close the round: what the writers need once every page is built.
    pub fn finish(self) -> Collected<'s> {
        let st = self.st.into_inner();
        Collected {
            sem: self.sem,
            texts: self.texts,
            placements: st.placements,
            links: st.links,
        }
    }
}

/// What one content-building round collected, for the object writers.
pub(crate) struct Collected<'s> {
    sem: &'s PdfSemantics,
    texts: &'s [&'s str],
    placements: HashMap<u32, Placement>,
    links: Vec<LinkHit>,
}

/// One planned `/Link` annotation.
pub(crate) struct AnnotPlan {
    pub id: Ref,
    pub page: usize,
    pub rect: [f32; 4],
    /// `/Contents` — the link's visible text (the target when it has none).
    pub contents: String,
    pub action: LinkAction,
}

/// What activating a planned link does.
pub(crate) enum LinkAction {
    /// `/A << /S /URI /URI (…) >>` — 7-bit ASCII, percent-encoded.
    Uri(Vec<u8>),
    /// `/Dest [page /XYZ left top 0]`.
    Dest(Placement),
}

impl AnnotPlan {
    /// Write the `/Link` annotation (ISO 32000-1 §12.5.6.5): no border
    /// (`/Border [0 0 0]` — the text keeps its own styling), `/F 4`
    /// (Print — PDF/A requires it; never Hidden / NoView), `/Contents`,
    /// and a `/URI` action or a `/Dest`. No JavaScript, no appearance
    /// stream (a link is never drawn). `struct_parent` is the annotation's
    /// key in the structure parent tree when the export is tagged.
    pub fn write(&self, pdf: &mut Pdf, page_refs: &[Ref], struct_parent: Option<i32>) {
        let [x0, y0, x1, y1] = self.rect;
        let mut annot = pdf.annotation(self.id);
        annot.subtype(AnnotationType::Link);
        annot.rect(Rect::new(x0, y0, x1, y1));
        annot.border(0.0, 0.0, 0.0, None);
        annot.flags(AnnotationFlags::PRINT);
        annot.contents(TextStr(&self.contents));
        if let Some(key) = struct_parent {
            annot.struct_parent(key);
        }
        match &self.action {
            LinkAction::Uri(bytes) => {
                annot.action().action_type(ActionType::Uri).uri(Str(bytes));
            }
            LinkAction::Dest(p) => {
                annot
                    .insert(Name(b"Dest"))
                    .start::<Destination>()
                    .page(page_refs[p.page])
                    .xyz(p.x, p.top, None);
            }
        }
    }
}

/// Issue #360 — a hyperlink target as a PDF `/URI` byte string: trimmed,
/// every byte outside printable ASCII percent-encoded (ISO 32000-1 §12.6.4.7
/// — a URI is 7-bit ASCII). `None` for an empty target and for script
/// schemes (`javascript:`, `vbscript:`, `data:`) — PDF/A forbids
/// JavaScript, and a `javascript:` URI is JavaScript by another name.
pub(crate) fn sanitize_uri(target: &str) -> Option<Vec<u8>> {
    let t = target.trim();
    let lower: String = t
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .take(12)
        .collect::<String>()
        .to_ascii_lowercase();
    if t.is_empty()
        || ["javascript:", "vbscript:", "data:"]
            .iter()
            .any(|s| lower.starts_with(s))
    {
        return None;
    }
    let mut out = Vec::with_capacity(t.len());
    for &b in t.as_bytes() {
        if (0x21..0x7f).contains(&b) {
            out.push(b);
        } else {
            out.extend_from_slice(format!("%{b:02X}").as_bytes());
        }
    }
    Some(out)
}

/// One outline entry before its objects are written.
struct OutlineEntry {
    level: u8,
    title: String,
    dest: Placement,
}

impl Collected<'_> {
    /// Where bookmark `name` resolves: the first placed paragraph (in
    /// document order) carrying it.
    fn bookmark(&self, name: &str) -> Option<Placement> {
        let mut ids: Vec<u32> = self.placements.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter().find_map(|id| {
            let meta = self.sem.paragraph(id)?;
            meta.bookmarks
                .iter()
                .any(|b| b == name)
                .then(|| self.placements[&id])
        })
    }

    /// Allocate one `/Link` annotation per collected line × link, in paint
    /// order. A bookmark no placed paragraph carries, and a URI
    /// [`sanitize_uri`] refuses, plan nothing — the text stays, unlinked.
    pub fn plan_links(&self, alloc: &mut impl FnMut() -> Ref) -> Vec<AnnotPlan> {
        let mut out = Vec::new();
        for hit in &self.links {
            let Some(span) = self
                .sem
                .paragraph(hit.para)
                .and_then(|m| m.links.get(hit.link))
            else {
                continue;
            };
            let action = match &span.target {
                LinkTarget::Uri(uri) => match sanitize_uri(uri) {
                    Some(bytes) => LinkAction::Uri(bytes),
                    None => continue,
                },
                LinkTarget::Bookmark(name) => match self.bookmark(name) {
                    Some(p) => LinkAction::Dest(p),
                    None => continue,
                },
            };
            let text = self
                .texts
                .get(hit.para as usize)
                .and_then(|t| t.get(span.start as usize..span.end as usize))
                .unwrap_or("");
            let text: String = text
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .filter(|&c| c != '\u{FFFC}')
                .collect();
            let contents = match text.trim() {
                "" => match &span.target {
                    LinkTarget::Uri(u) => u.trim().to_string(),
                    LinkTarget::Bookmark(b) => b.clone(),
                },
                t => t.to_string(),
            };
            out.push(AnnotPlan {
                id: alloc(),
                page: hit.page,
                rect: hit.rect,
                contents,
                action,
            });
        }
        out
    }

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
