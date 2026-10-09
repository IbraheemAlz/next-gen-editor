//! Issue #360 — tagged PDF (ISO 32000-1 §14.7 logical structure, §14.8
//! tagged PDF): the marked-content recorder the content emitters drive and
//! the structure tree planned from it.
//!
//! **Recording.** With tagging on, every piece of page content is either
//! *real content* inside a `BDC … EMC` sequence carrying an `/MCID`, or an
//! *artifact* inside `/Artifact BMC … EMC` (shading, borders, highlights,
//! underlines, tab leaders, shape outlines, the note separator) or
//! `/Artifact <</Type /Pagination /Subtype /Header|/Footer>> BDC … EMC`
//! (header / footer bands — page numbers included — and table header rows
//! repeated on a continuation page). Each MCID's owner is recorded:
//! a paragraph's content element ([`Owner::Para`]), a list label
//! ([`Owner::Label`]), a hyperlink span ([`Owner::Link`] — the text
//! switches sequences glyph by glyph where a link starts or ends, so the
//! `Link` element holds exactly its own text) or a picture
//! ([`Owner::Figure`]). A paragraph split across pages simply owns
//! sequences on both pages.
//!
//! **The tree** is planned once the contents are final, from the document
//! model's side table (document order) rather than the paint order:
//! `Document` > `P` / `H1`–`H6` / `L` > `LI` > `Lbl` + `LBody` / `Table` >
//! `TR` > `TH` / `TD` (with `/RowSpan`, `/ColSpan`, `/Scope /Column`) /
//! `Figure` (`/Alt`) / `Link` (its text + an `OBJR` per annotation). Notes
//! and text-box stories follow the body (their side-table ids do). Every
//! table cell of the model becomes a `TH` / `TD` (empty when its
//! paragraphs paint nothing) so rows stay regular; a cell holding one
//! plain paragraph carries that paragraph's content directly (no `P`
//! layer), as does a list item's `LBody`.
//!
//! **Heading levels** are normalized for PDF/UA (ISO 14289-1 §7.4.2: the
//! first heading is `H1`, a level is never skipped): a heading nested
//! under *n* shallower headings becomes `H(n+1)`, capped at `H6`.

use crate::semantic::{AnnotPlan, CellSemantics, Collected, ObjectSemantics, SemCtx};
use pdf_writer::types::{ArtifactSubtype, ArtifactType, ListNumbering, TableHeaderScope};
use pdf_writer::writers::{StructElement, StructTreeRoot};
use pdf_writer::{Content, Name, Pdf, Ref, TextStr};
use std::collections::HashMap;

/// Who owns a marked-content sequence.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Owner {
    /// A paragraph's content element (`P`, `Hn`, or a list item's
    /// `LBody`).
    Para(u32),
    /// A list item's label (`Lbl`).
    Label(u32),
    /// Hyperlink `n` of a paragraph (`Link`; `Span` when no annotation
    /// was planned for it — a dangling bookmark, a refused URI, PDF/X-3).
    Link(u32, u32),
    /// The `n`-th figure of the export.
    Figure(u32),
}

/// One recorded child of an owner, in paint order. `byte` is the child's
/// position in the paragraph's source text (a sequence's smallest glyph
/// byte, a hyperlink's start, a picture's sentinel): the structure tree
/// orders a paragraph's children by it, so reading order is logical even
/// where the paint order is not (an inline picture paints before the
/// text it sits in; an RTL line paints its logical end first).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kid {
    Mcr { page: usize, mcid: i32, byte: u32 },
    Child { owner: Owner, byte: u32 },
}

impl Kid {
    fn byte(&self) -> u32 {
        match *self {
            Kid::Mcr { byte, .. } | Kid::Child { byte, .. } => byte,
        }
    }
}

/// A recorded figure: its alternate description (its parent paragraph
/// already lists it as a [`Kid::Child`]).
#[derive(Debug, Clone)]
struct FigureMeta {
    alt: String,
}

/// What an artifact sequence marks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ArtifactKind {
    /// Shading, borders, highlights, decorations, leaders, outlines.
    Decoration,
    /// A header band.
    Header,
    /// A footer band.
    Footer,
    /// Pagination without a subtype (a repeated table header row).
    Pagination,
    /// A layout artifact (the note separator rule).
    Layout,
}

/// How a picture was marked ([`SemCtx::figure_begin`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FigureMark {
    /// Untagged export, or inside an artifact: nothing written.
    None,
    /// Decorative: an artifact.
    Artifact,
    /// A `Figure` sequence.
    Figure,
}

/// The marked-content record of one content-building round.
#[derive(Default)]
pub(crate) struct TagState {
    /// Per page, the owner of each MCID (the parent tree's arrays).
    mcids: Vec<Vec<Owner>>,
    kids: HashMap<Owner, Vec<Kid>>,
    figures: Vec<FigureMeta>,
    /// The first page each paragraph got tagged content on.
    first_page: HashMap<u32, usize>,
    artifact_depth: u32,
    open: Option<Owner>,
    text_para: Option<u32>,
}

/// Structure types this exporter writes (all standard PDF 1.7 types, so
/// no role map is needed).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Document,
    P,
    H(u8),
    L,
    LI,
    Lbl,
    LBody,
    Table,
    TR,
    TH,
    TD,
    Figure,
    Link,
    Span,
}

impl Role {
    fn name(self) -> &'static [u8] {
        match self {
            Role::Document => b"Document",
            Role::P => b"P",
            Role::H(1) => b"H1",
            Role::H(2) => b"H2",
            Role::H(3) => b"H3",
            Role::H(4) => b"H4",
            Role::H(5) => b"H5",
            Role::H(_) => b"H6",
            Role::L => b"L",
            Role::LI => b"LI",
            Role::Lbl => b"Lbl",
            Role::LBody => b"LBody",
            Role::Table => b"Table",
            Role::TR => b"TR",
            Role::TH => b"TH",
            Role::TD => b"TD",
            Role::Figure => b"Figure",
            Role::Link => b"Link",
            Role::Span => b"Span",
        }
    }
}

/// Every heading paragraph's normalized structure level (see the module
/// docs). Headings with an empty title paint nothing and are skipped, so
/// they cannot open a level nobody sees.
pub(crate) fn heading_roles(sem: &crate::PdfSemantics) -> HashMap<u32, u8> {
    let mut out = HashMap::new();
    let mut open: Vec<u8> = Vec::new();
    for (i, p) in sem.paragraphs.iter().enumerate() {
        let Some(level) = p.heading else { continue };
        if p.title.trim().is_empty() {
            continue;
        }
        while open.last().is_some_and(|&top| top >= level) {
            open.pop();
        }
        out.insert(i as u32, (open.len() as u8 + 1).min(6));
        open.push(level);
    }
    out
}

/// `/ListNumbering` from a resolved marker's text.
fn list_numbering(marker: &str) -> ListNumbering {
    let core: String = marker
        .trim()
        .trim_matches(|c: char| matches!(c, '.' | ')' | '(' | ']' | '[' | ':' | '-'))
        .to_string();
    let Some(last) = core.chars().last() else {
        return ListNumbering::None;
    };
    let roman = |s: &str, set: &str| !s.is_empty() && s.chars().all(|c| set.contains(c));
    if last.is_ascii_digit() || last.is_numeric() {
        ListNumbering::Decimal
    } else if roman(&core, "ivxlcdm") {
        ListNumbering::LowerRoman
    } else if roman(&core, "IVXLCDM") {
        ListNumbering::UpperRoman
    } else if core.chars().all(|c| c.is_ascii_lowercase()) {
        ListNumbering::LowerAlpha
    } else if core.chars().all(|c| c.is_ascii_uppercase()) {
        ListNumbering::UpperAlpha
    } else if matches!(last, '○' | 'o' | '◦') {
        ListNumbering::Circle
    } else if matches!(last, '■' | '▪' | '§' | '\u{F0A7}') {
        ListNumbering::Square
    } else {
        ListNumbering::Disc
    }
}

/* ================================================================
The recorder — called by the content emitters.
================================================================ */

impl SemCtx<'_> {
    /// `true` when content written now is real content to tag.
    pub(crate) fn tagging(&self) -> bool {
        self.tagged && self.st.borrow().tag.artifact_depth == 0
    }

    /// Open an artifact sequence (no-op when untagged; nested artifacts
    /// write nothing — the outermost one already covers them).
    pub(crate) fn begin_artifact(&self, c: &mut Content, kind: ArtifactKind) {
        if !self.tagged {
            return;
        }
        let mut st = self.st.borrow_mut();
        st.tag.artifact_depth += 1;
        if st.tag.artifact_depth > 1 {
            return;
        }
        let (ty, sub) = match kind {
            ArtifactKind::Decoration => {
                c.begin_marked_content(Name(b"Artifact"));
                return;
            }
            ArtifactKind::Header => (ArtifactType::Pagination, Some(ArtifactSubtype::Header)),
            ArtifactKind::Footer => (ArtifactType::Pagination, Some(ArtifactSubtype::Footer)),
            ArtifactKind::Pagination => (ArtifactType::Pagination, None),
            ArtifactKind::Layout => (ArtifactType::Layout, None),
        };
        let mut mc = c.begin_marked_content_with_properties(Name(b"Artifact"));
        let mut art = mc.properties().artifact();
        art.kind(ty);
        if let Some(sub) = sub {
            art.subtype(sub);
        }
    }

    /// Close the artifact sequence [`Self::begin_artifact`] opened.
    pub(crate) fn end_artifact(&self, c: &mut Content) {
        if !self.tagged {
            return;
        }
        let mut st = self.st.borrow_mut();
        st.tag.artifact_depth -= 1;
        if st.tag.artifact_depth == 0 {
            c.end_marked_content();
        }
    }

    /// The structure type of paragraph `id`'s content element.
    fn para_role(&self, id: u32) -> Role {
        if let Some(&n) = self.heading_roles.get(&id) {
            Role::H(n)
        } else if self.sem.paragraph(id).is_some_and(|m| m.list.is_some()) {
            Role::LBody
        } else {
            Role::P
        }
    }

    fn owner_role(&self, owner: Owner) -> Role {
        match owner {
            Owner::Para(id) => self.para_role(id),
            Owner::Label(_) => Role::Lbl,
            Owner::Link(..) => Role::Link,
            Owner::Figure(_) => Role::Figure,
        }
    }

    /// Begin a tagged sequence owned by `owner` on the current page;
    /// `byte` is its initial source position ([`Kid`]).
    fn open_mc(&self, c: &mut Content, owner: Owner, byte: u32) {
        let role = self.owner_role(owner);
        let mut st = self.st.borrow_mut();
        let page = st.page;
        let tag = &mut st.tag;
        if tag.mcids.len() <= page {
            tag.mcids.resize_with(page + 1, Vec::new);
        }
        let mcid = tag.mcids[page].len() as i32;
        tag.mcids[page].push(owner);
        tag.kids
            .entry(owner)
            .or_default()
            .push(Kid::Mcr { page, mcid, byte });
        if let Owner::Para(id) | Owner::Label(id) = owner {
            tag.first_page.entry(id).or_insert(page);
        }
        tag.open = Some(owner);
        c.begin_marked_content_with_properties(Name(role.name()))
            .properties()
            .identify(mcid);
    }

    /// End the open tagged sequence, if any.
    fn close_mc(&self, c: &mut Content) {
        if self.st.borrow_mut().tag.open.take().is_some() {
            c.end_marked_content();
        }
    }

    /// A list marker (or a numbered heading's number) is about to show
    /// for paragraph `id`. `true` when a sequence was opened (close it
    /// with [`Self::label_end`]).
    pub(crate) fn label_begin(&self, c: &mut Content, id: u32) -> bool {
        if !self.tagging() {
            return false;
        }
        let owner = if self.para_role(id) == Role::LBody {
            Owner::Label(id)
        } else {
            Owner::Para(id)
        };
        /* A label precedes the paragraph's text. */
        self.open_mc(c, owner, 0);
        true
    }

    /// Close the label sequence.
    pub(crate) fn label_end(&self, c: &mut Content) {
        self.close_mc(c);
    }

    /// Paragraph `id`'s line glyphs are about to show (inside its text
    /// object): [`Self::glyph`] opens sequences lazily from here on.
    pub(crate) fn text_begin(&self, id: u32) {
        if self.tagging() {
            self.st.borrow_mut().tag.text_para = Some(id);
        }
    }

    /// A glyph whose source byte is `byte` is about to show: switch to the
    /// sequence of the hyperlink containing it (or of the paragraph).
    pub(crate) fn glyph(&self, c: &mut Content, byte: u32) {
        let (id, open) = {
            let st = self.st.borrow();
            match st.tag.text_para {
                Some(id) => (id, st.tag.open),
                None => return,
            }
        };
        let link = self.sem.paragraph(id).and_then(|m| {
            m.links
                .iter()
                .enumerate()
                .find(|(_, l)| l.start <= byte && byte < l.end)
                .map(|(i, l)| (i as u32, l.start))
        });
        let owner = link.map_or(Owner::Para(id), |(i, _)| Owner::Link(id, i));
        if open != Some(owner) {
            self.close_mc(c);
            if let Some((_, start)) = link {
                let mut st = self.st.borrow_mut();
                let kids = st.tag.kids.entry(Owner::Para(id)).or_default();
                if !kids
                    .iter()
                    .any(|k| matches!(*k, Kid::Child { owner: o, .. } if o == owner))
                {
                    kids.push(Kid::Child { owner, byte: start });
                }
            }
            self.open_mc(c, owner, byte);
        }
        /* The open sequence is its owner's newest kid. */
        let mut st = self.st.borrow_mut();
        if let Some(Kid::Mcr { byte: b, .. }) =
            st.tag.kids.get_mut(&owner).and_then(|k| k.last_mut())
        {
            *b = (*b).min(byte);
        }
    }

    /// The paragraph's line glyphs are done: close the open sequence
    /// (before the text object's `ET`).
    pub(crate) fn text_end(&self, c: &mut Content) {
        if !self.tagged {
            return;
        }
        self.close_mc(c);
        self.st.borrow_mut().tag.text_para = None;
    }

    /// A picture is about to paint. `parent` is the paragraph it belongs
    /// to (inline, or a float's anchor), `at` its sentinel's byte in that
    /// paragraph and `meta` its side-table entry.
    pub(crate) fn figure_begin(
        &self,
        c: &mut Content,
        parent: Option<u32>,
        at: u32,
        meta: Option<&ObjectSemantics>,
    ) -> FigureMark {
        if !self.tagging() {
            return FigureMark::None;
        }
        if meta.is_some_and(|m| m.decorative) {
            self.begin_artifact(c, ArtifactKind::Decoration);
            return FigureMark::Artifact;
        }
        let alt = meta
            .and_then(|m| m.alt.as_deref())
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .unwrap_or("Image")
            .to_string();
        let owner = {
            let mut st = self.st.borrow_mut();
            let n = st.tag.figures.len() as u32;
            st.tag.figures.push(FigureMeta { alt });
            let owner = Owner::Figure(n);
            if let Some(pid) = parent {
                st.tag
                    .kids
                    .entry(Owner::Para(pid))
                    .or_default()
                    .push(Kid::Child { owner, byte: at });
            }
            owner
        };
        self.open_mc(c, owner, at);
        FigureMark::Figure
    }

    /// Close what [`Self::figure_begin`] opened.
    pub(crate) fn figure_end(&self, c: &mut Content, mark: FigureMark) {
        match mark {
            FigureMark::None => {}
            FigureMark::Artifact => self.end_artifact(c),
            FigureMark::Figure => self.close_mc(c),
        }
    }

    /// `true` when paragraph `id` already got tagged content on an
    /// earlier page — a table header row repeated on a continuation page
    /// is then an artifact, not a second copy of its text.
    pub(crate) fn already_tagged_before(&self, id: u32) -> bool {
        if !self.tagging() {
            return false;
        }
        let st = self.st.borrow();
        st.tag.first_page.get(&id).is_some_and(|&p| p < st.page)
    }
}

/* ================================================================
The structure tree.
================================================================ */

#[derive(Debug, Clone, Copy)]
enum NKid {
    Mcr { page: usize, mcid: i32 },
    Objr { page: usize, annot: Ref },
    Node(usize),
}

#[derive(Debug, Clone, Copy)]
enum Attr {
    None,
    List(ListNumbering),
    Cell {
        header: bool,
        row_span: u32,
        col_span: u32,
    },
}

#[derive(Debug)]
struct Node {
    role: Role,
    parent: usize,
    kids: Vec<NKid>,
    alt: Option<String>,
    lang: Option<String>,
    attr: Attr,
}

/// An allocated, not yet written, structure tree.
pub(crate) struct StructPlan {
    pub root: Ref,
    refs: Vec<Ref>,
    nodes: Vec<Node>,
    /// Per page, the node owning each MCID.
    page_mcids: Vec<Vec<usize>>,
    /// `(parent-tree key, node)` per annotation, in [`AnnotPlan`] order.
    annot_nodes: Vec<Option<(i32, usize)>>,
    next_key: i32,
}

impl StructPlan {
    /// The `/StructParents` key of page `index` (`None`: no tagged
    /// content on it).
    pub fn page_key(&self, index: usize) -> Option<i32> {
        self.page_mcids
            .get(index)
            .is_some_and(|m| !m.is_empty())
            .then_some(index as i32)
    }

    /// The `/StructParent` key of annotation `index`.
    pub fn annot_key(&self, index: usize) -> Option<i32> {
        self.annot_nodes
            .get(index)
            .copied()
            .flatten()
            .map(|(k, _)| k)
    }
}

struct Builder<'a, 'c> {
    col: &'a Collected<'c>,
    nodes: Vec<Node>,
    owner_node: HashMap<Owner, usize>,
    /// Annotation indices per `(paragraph, link)`.
    annots_of: HashMap<(u32, u32), Vec<usize>>,
    annots: &'a [AnnotPlan],
    annot_node: Vec<Option<usize>>,
    root_lang: &'a str,
}

impl Builder<'_, '_> {
    fn kids(&self, owner: Owner) -> &[Kid] {
        self.col.tag.kids.get(&owner).map_or(&[][..], Vec::as_slice)
    }

    fn present(&self, id: u32) -> bool {
        !self.kids(Owner::Para(id)).is_empty() || !self.kids(Owner::Label(id)).is_empty()
    }

    fn cells(&self, id: u32) -> &[CellSemantics] {
        self.col.sem.paragraph(id).map_or(&[][..], |m| &m.cells)
    }

    fn heading(&self, id: u32) -> Option<u8> {
        self.col.heading_roles.get(&id).copied()
    }

    fn is_list_item(&self, id: u32) -> bool {
        self.heading(id).is_none() && self.col.sem.paragraph(id).is_some_and(|m| m.list.is_some())
    }

    fn level(&self, id: u32) -> u8 {
        self.col
            .sem
            .paragraph(id)
            .and_then(|m| m.list.as_ref())
            .map_or(0, |l| l.level)
    }

    /// The paragraph's `/Lang` when it differs from the document's.
    fn lang(&self, id: u32) -> Option<String> {
        self.col
            .sem
            .paragraph(id)
            .and_then(|m| m.lang.as_deref())
            .filter(|l| !l.is_empty() && !l.eq_ignore_ascii_case(self.root_lang))
            .map(str::to_string)
    }

    fn add(&mut self, role: Role, parent: usize, attr: Attr) -> usize {
        let n = self.nodes.len();
        self.nodes.push(Node {
            role,
            parent,
            kids: Vec::new(),
            alt: None,
            lang: None,
            attr,
        });
        if n != parent {
            self.nodes[parent].kids.push(NKid::Node(n));
        }
        n
    }

    /// Every recorded MCR of `owner` into `node`.
    fn attach_mcrs(&mut self, node: usize, owner: Owner) {
        self.owner_node.insert(owner, node);
        let mcrs: Vec<NKid> = self
            .kids(owner)
            .iter()
            .filter_map(|k| match *k {
                Kid::Mcr { page, mcid, .. } => Some(NKid::Mcr { page, mcid }),
                Kid::Child { .. } => None,
            })
            .collect();
        self.nodes[node].kids.extend(mcrs);
    }

    /// Paragraph `id`'s content (its sequences, links, inline figures)
    /// into `node`, plus any link of it that only has annotations.
    fn attach_para(&mut self, node: usize, id: u32) {
        self.owner_node.insert(Owner::Para(id), node);
        let mut kids: Vec<Kid> = self.kids(Owner::Para(id)).to_vec();
        /* Logical reading order (stable: equal positions keep paint
        order — a sequence split across pages, a label before text). */
        kids.sort_by_key(Kid::byte);
        for kid in kids {
            match kid {
                Kid::Mcr { page, mcid, .. } => {
                    self.nodes[node].kids.push(NKid::Mcr { page, mcid });
                }
                Kid::Child {
                    owner: owner @ Owner::Link(..),
                    ..
                } => self.link(owner, node),
                Kid::Child {
                    owner: owner @ Owner::Figure(_),
                    ..
                } => self.figure(owner, node),
                Kid::Child { .. } => {}
            }
        }
        let mut annotation_only: Vec<u32> = self
            .annots_of
            .keys()
            .filter(|(p, l)| *p == id && !self.owner_node.contains_key(&Owner::Link(*p, *l)))
            .map(|&(_, l)| l)
            .collect();
        annotation_only.sort_unstable();
        for l in annotation_only {
            self.link(Owner::Link(id, l), node);
        }
    }

    fn link(&mut self, owner: Owner, parent: usize) {
        let Owner::Link(p, l) = owner else { return };
        if self.owner_node.contains_key(&owner) {
            return;
        }
        let annots = self.annots_of.get(&(p, l)).cloned().unwrap_or_default();
        let role = if annots.is_empty() {
            Role::Span
        } else {
            Role::Link
        };
        let node = self.add(role, parent, Attr::None);
        self.attach_mcrs(node, owner);
        for i in annots {
            let a = &self.annots[i];
            self.nodes[node].kids.push(NKid::Objr {
                page: a.page,
                annot: a.id,
            });
            self.annot_node[i] = Some(node);
        }
    }

    fn figure(&mut self, owner: Owner, parent: usize) {
        let Owner::Figure(n) = owner else { return };
        if self.owner_node.contains_key(&owner) {
            return;
        }
        let node = self.add(Role::Figure, parent, Attr::None);
        self.nodes[node].alt = self.col.tag.figures.get(n as usize).map(|f| f.alt.clone());
        self.attach_mcrs(node, owner);
    }

    fn paragraph(&mut self, id: u32, parent: usize) {
        let role = self.heading(id).map_or(Role::P, Role::H);
        let node = self.add(role, parent, Attr::None);
        self.nodes[node].lang = self.lang(id);
        self.attach_para(node, id);
    }

    /// Document-order paragraphs `ids` at table nesting `depth`.
    fn seq(&mut self, ids: &[u32], depth: usize, parent: usize) {
        let mut i = 0;
        while i < ids.len() {
            let id = ids[i];
            if let Some(cell) = self.cells(id).get(depth).copied() {
                let n = ids[i..]
                    .iter()
                    .take_while(|&&x| self.cells(x).get(depth).map(|c| c.table) == Some(cell.table))
                    .count();
                self.table(&ids[i..i + n], depth, parent);
                i += n;
            } else if self.is_list_item(id) {
                let n = ids[i..]
                    .iter()
                    .take_while(|&&x| self.cells(x).len() == depth && self.is_list_item(x))
                    .count();
                let items: Vec<u32> = ids[i..i + n]
                    .iter()
                    .copied()
                    .filter(|&x| self.present(x))
                    .collect();
                if !items.is_empty() {
                    self.list(&items, parent);
                }
                i += n;
            } else {
                if self.present(id) {
                    self.paragraph(id, parent);
                }
                i += 1;
            }
        }
    }

    /// One table: every model row × cell its paragraphs name (painted or
    /// not, so the rows stay regular), skipped entirely when nothing of
    /// it painted.
    fn table(&mut self, ids: &[u32], depth: usize, parent: usize) {
        if !ids.iter().any(|&id| self.present(id)) {
            return;
        }
        let table = self.add(Role::Table, parent, Attr::None);
        let mut i = 0;
        while i < ids.len() {
            let row = self.cells(ids[i])[depth].row;
            let n = ids[i..]
                .iter()
                .take_while(|&&x| self.cells(x)[depth].row == row)
                .count();
            let tr = self.add(Role::TR, table, Attr::None);
            let row_ids = &ids[i..i + n];
            let mut j = 0;
            while j < row_ids.len() {
                let cell = self.cells(row_ids[j])[depth];
                let m = row_ids[j..]
                    .iter()
                    .take_while(|&&x| self.cells(x)[depth].cell == cell.cell)
                    .count();
                let role = if cell.header { Role::TH } else { Role::TD };
                let td = self.add(
                    role,
                    tr,
                    Attr::Cell {
                        header: cell.header,
                        row_span: cell.row_span.max(1),
                        col_span: cell.col_span.max(1),
                    },
                );
                let cell_ids = &row_ids[j..j + m];
                let only = cell_ids[0];
                if cell_ids.len() == 1
                    && self.cells(only).len() == depth + 1
                    && self.heading(only).is_none()
                    && !self.is_list_item(only)
                    && self.lang(only).is_none()
                {
                    /* One plain paragraph: its content sits in the cell
                    directly (no `P` layer). */
                    if self.present(only) {
                        self.attach_para(td, only);
                    }
                } else {
                    self.seq(cell_ids, depth + 1, td);
                }
                j += m;
            }
            i += n;
        }
    }

    /// One list of painted items (`L`), nesting deeper levels inside the
    /// preceding item's `LBody`.
    fn list(&mut self, ids: &[u32], parent: usize) {
        let numbering = self
            .col
            .sem
            .paragraph(ids[0])
            .and_then(|m| m.list.as_ref())
            .map_or(ListNumbering::None, |l| list_numbering(&l.marker));
        let l = self.add(Role::L, parent, Attr::List(numbering));
        let base = ids.iter().map(|&id| self.level(id)).min().unwrap_or(0);
        let mut i = 0;
        while i < ids.len() {
            let id = ids[i];
            let li = self.add(Role::LI, l, Attr::None);
            if !self.kids(Owner::Label(id)).is_empty() {
                let lbl = self.add(Role::Lbl, li, Attr::None);
                self.attach_mcrs(lbl, Owner::Label(id));
            }
            let body = self.add(Role::LBody, li, Attr::None);
            self.nodes[body].lang = self.lang(id);
            self.attach_para(body, id);
            let nested = ids[i + 1..]
                .iter()
                .take_while(|&&x| self.level(x) > base)
                .count();
            if nested > 0 {
                self.list(&ids[i + 1..i + 1 + nested], body);
            }
            i += 1 + nested;
        }
    }
}

impl Collected<'_> {
    /// Issue #360 — a title for a PDF/UA export of a document without a
    /// core-properties one: the first heading's text, else the first
    /// non-empty paragraph text, cut to at most 100 characters at a word
    /// boundary, else a fixed placeholder.
    pub fn fallback_title(&self) -> String {
        const MAX: usize = 100;
        let clean = |s: &str| -> String {
            let t: String = s
                .chars()
                .filter(|&c| c != '\u{FFFC}')
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
            if t.chars().count() <= MAX {
                return t;
            }
            let head: String = t.chars().take(MAX).collect();
            match head.rfind(' ') {
                Some(i) if i > 0 => head[..i].to_string(),
                _ => head,
            }
        };
        let heading = self
            .sem
            .paragraphs
            .iter()
            .filter(|p| p.heading.is_some())
            .map(|p| clean(&p.title))
            .find(|t| !t.is_empty());
        let text = || self.texts.iter().map(|t| clean(t)).find(|t| !t.is_empty());
        heading
            .or_else(text)
            .unwrap_or_else(|| "Untitled document".to_string())
    }

    /// Plan the structure tree from the recorded marked content (see the
    /// module docs) and allocate its objects. `root_lang` is the catalog
    /// `/Lang` (paragraphs in another language carry their own).
    pub fn plan_structure(
        &self,
        annots: &[AnnotPlan],
        page_count: usize,
        root_lang: &str,
        alloc: &mut impl FnMut() -> Ref,
    ) -> StructPlan {
        let mut annots_of: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
        for (i, a) in annots.iter().enumerate() {
            annots_of.entry((a.para, a.link)).or_default().push(i);
        }
        let mut b = Builder {
            col: self,
            nodes: Vec::new(),
            owner_node: HashMap::new(),
            annots_of,
            annots,
            annot_node: vec![None; annots.len()],
            root_lang,
        };
        let doc = b.add(Role::Document, 0, Attr::None);

        /* Document order: the side table, then any recorded paragraph
        beyond it (text-box stories). */
        let mut ids: Vec<u32> = (0..self.sem.paragraphs.len() as u32).collect();
        let mut extra: Vec<u32> = self
            .tag
            .kids
            .keys()
            .filter_map(|o| match *o {
                Owner::Para(id) | Owner::Label(id) if id as usize >= self.sem.paragraphs.len() => {
                    Some(id)
                }
                _ => None,
            })
            .collect();
        extra.sort_unstable();
        extra.dedup();
        ids.extend(extra);
        b.seq(&ids, 0, doc);

        /* Anything recorded but not yet placed (a float whose anchor is
        unknown, a paragraph outside every story) hangs off the document,
        so every MCID has a parent. */
        let mut orphans: Vec<Owner> = self
            .tag
            .kids
            .keys()
            .copied()
            .filter(|o| !b.owner_node.contains_key(o))
            .collect();
        orphans.sort_by_key(|o| match *o {
            Owner::Para(id) => (0, id, 0),
            Owner::Label(id) => (0, id, 1),
            Owner::Link(id, l) => (1, id, l),
            Owner::Figure(n) => (2, n, 0),
        });
        for o in orphans {
            if b.owner_node.contains_key(&o) {
                continue;
            }
            match o {
                Owner::Para(id) => {
                    let node = b.add(Role::P, doc, Attr::None);
                    b.attach_para(node, id);
                }
                Owner::Label(_) => {
                    let node = b.add(Role::P, doc, Attr::None);
                    b.attach_mcrs(node, o);
                }
                Owner::Link(..) => b.link(o, doc),
                Owner::Figure(_) => b.figure(o, doc),
            }
        }
        /* An annotation whose link never got an element (its paragraph
        painted nothing tagged): a `Link` holding just the annotation. */
        for (i, a) in annots.iter().enumerate() {
            let o = Owner::Link(a.para, a.link);
            if b.annot_node[i].is_none() && !b.owner_node.contains_key(&o) {
                b.link(o, doc);
            }
        }

        let page_mcids: Vec<Vec<usize>> = (0..page_count)
            .map(|p| {
                self.tag.mcids.get(p).map_or_else(Vec::new, |owners| {
                    owners.iter().map(|o| b.owner_node[o]).collect()
                })
            })
            .collect();
        let mut next_key = page_count as i32;
        let annot_nodes: Vec<Option<(i32, usize)>> = b
            .annot_node
            .iter()
            .map(|n| {
                n.map(|node| {
                    let key = next_key;
                    next_key += 1;
                    (key, node)
                })
            })
            .collect();
        let root = alloc();
        let refs: Vec<Ref> = b.nodes.iter().map(|_| alloc()).collect();
        StructPlan {
            root,
            refs,
            nodes: b.nodes,
            page_mcids,
            annot_nodes,
            next_key,
        }
    }
}

impl StructPlan {
    /// Write the structure tree root (with its parent tree) and every
    /// structure element. Each element names the page of its first
    /// sequence (`/Pg`), so same-page sequences are bare MCIDs.
    pub fn write(&self, pdf: &mut Pdf, page_refs: &[Ref]) {
        {
            let mut root = pdf.indirect(self.root).start::<StructTreeRoot>();
            root.child(self.refs[0]);
            {
                let mut tree = root.insert(Name(b"ParentTree")).dict();
                let mut nums = tree.insert(Name(b"Nums")).array();
                for (page, nodes) in self.page_mcids.iter().enumerate() {
                    if nodes.is_empty() {
                        continue;
                    }
                    nums.item(page as i32);
                    nums.push()
                        .array()
                        .items(nodes.iter().map(|&n| self.refs[n]));
                }
                for &(key, node) in self.annot_nodes.iter().flatten() {
                    nums.item(key);
                    nums.item(self.refs[node]);
                }
            }
            root.parent_tree_next_key(self.next_key);
        }
        for (i, node) in self.nodes.iter().enumerate() {
            let mut e = pdf.indirect(self.refs[i]).start::<StructElement>();
            e.custom_kind(Name(node.role.name()));
            e.parent(if i == 0 {
                self.root
            } else {
                self.refs[node.parent]
            });
            let elem_page = node.kids.iter().find_map(|k| match *k {
                NKid::Mcr { page, .. } => Some(page),
                _ => None,
            });
            if let Some(page) = elem_page {
                e.page(page_refs[page]);
            }
            if let Some(alt) = &node.alt {
                e.alt(TextStr(alt));
            }
            if let Some(lang) = &node.lang {
                e.lang(TextStr(lang));
            }
            match node.attr {
                Attr::None => {}
                Attr::List(numbering) => {
                    e.attributes().push().list().list_numbering(numbering);
                }
                Attr::Cell {
                    header,
                    row_span,
                    col_span,
                } => {
                    if header || row_span > 1 || col_span > 1 {
                        let mut attrs = e.attributes();
                        let mut t = attrs.push().table();
                        if row_span > 1 {
                            t.row_span(row_span as i32);
                        }
                        if col_span > 1 {
                            t.col_span(col_span as i32);
                        }
                        if header {
                            t.scope(TableHeaderScope::Column);
                        }
                    }
                }
            }
            if node.kids.is_empty() {
                continue;
            }
            let mut kids = e.children();
            for kid in &node.kids {
                match *kid {
                    NKid::Mcr { page, mcid } if Some(page) == elem_page => {
                        kids.marked_content_id(mcid);
                    }
                    NKid::Mcr { page, mcid } => {
                        kids.marked_content_ref()
                            .page(page_refs[page])
                            .marked_content_id(mcid);
                    }
                    NKid::Objr { page, annot } => {
                        kids.object_ref().page(page_refs[page]).object(annot);
                    }
                    NKid::Node(n) => {
                        kids.struct_element(self.refs[n]);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ParagraphSemantics, PdfSemantics};

    #[test]
    fn heading_levels_start_at_one_and_never_skip() {
        let h = |level: u8| ParagraphSemantics {
            heading: Some(level),
            title: "t".into(),
            ..Default::default()
        };
        let sem = PdfSemantics {
            paragraphs: vec![
                h(2),
                h(4),
                ParagraphSemantics::default(),
                h(4),
                h(3),
                h(1),
                ParagraphSemantics {
                    heading: Some(2),
                    ..Default::default()
                },
                h(9),
            ],
            ..Default::default()
        };
        let roles = heading_roles(&sem);
        let got: Vec<(u32, u8)> = {
            let mut v: Vec<_> = roles.into_iter().collect();
            v.sort_unstable();
            v
        };
        assert_eq!(got, [(0, 1), (1, 2), (3, 2), (4, 2), (5, 1), (7, 2)]);
    }

    #[test]
    fn list_numbering_follows_the_marker() {
        assert_eq!(list_numbering("1."), ListNumbering::Decimal);
        assert_eq!(list_numbering("1.2.3"), ListNumbering::Decimal);
        assert_eq!(list_numbering("iv)"), ListNumbering::LowerRoman);
        assert_eq!(list_numbering("(B)"), ListNumbering::UpperAlpha);
        assert_eq!(list_numbering("a."), ListNumbering::LowerAlpha);
        assert_eq!(list_numbering("•"), ListNumbering::Disc);
        assert_eq!(list_numbering("o"), ListNumbering::LowerAlpha);
        assert_eq!(list_numbering("■"), ListNumbering::Square);
        assert_eq!(list_numbering("٣."), ListNumbering::Decimal);
        assert_eq!(list_numbering(""), ListNumbering::None);
    }
}
