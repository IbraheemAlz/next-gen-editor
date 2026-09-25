//! Issue #120 — block-level passthrough: the markup a `<w:body>` (or a
//! `<w:tc>`, `<w:hdr>`, `<w:footnote>`) carries BETWEEN and AROUND its
//! paragraphs and tables, which the typed model does not represent.
//!
//! Three shapes, all lost on a zero-edit resave before this module:
//!
//! - **envelopes** — a `<w:sdt>` content control (or `<w:customXml>`)
//!   wrapping a run of blocks: `<w:sdt><w:sdtPr>…</w:sdtPr><w:sdtContent>`
//!   before the first inner block, `</w:sdtContent></w:sdt>` after the
//!   last. The inner blocks stay first-class body blocks (layout and
//!   editing untouched); the envelope becomes an [`BodyFragment::Open`] on
//!   the first inner block and a [`BodyFragment::Close`] on the last;
//! - **markers** — `<w:bookmarkStart/>`, `<w:bookmarkEnd/>`,
//!   `<w:commentRangeStart/>`, `<w:proofErr/>`, … sitting between two
//!   blocks: a [`BodyFragment::Verbatim`] on the following block;
//! - **whitespace** — the text between blocks of a pretty-printed part:
//!   also `Verbatim`.
//!
//! [`BlockEnvelopes`] is the reader-side tracker both the body walker
//! (`parts::document`) and the cell walker (`parts::table`) drive with
//! the same five calls: `open_container` / `close_container` around an
//! envelope element, `note_block_start` / `note_block_end` around every
//! block, `take_before` when a block is pushed. The writer-side mirror is
//! [`EnvelopeStack`]: it emits the fragments and keeps every envelope
//! well-formed no matter what an edit did to its ends — an envelope whose
//! closer was lost (its last paragraph merged away) closes at the end of
//! the container, a closer whose opener was lost is skipped, and a
//! duplicated opener (a split paragraph) opens once.

use engine::{Block, BodyFragment, BodyPassthrough, TableCell, TableRow};

/// Anything a [`BlockEnvelopes`] tracker attaches passthrough markup to:
/// a block of a block container (`<w:body>`, `<w:tc>`), and — issue
/// #248 — a row of a `<w:tbl>` or a cell of a `<w:tr>` (a `<w:sdt>`
/// wrapping table rows / cells, whitespace and markers between them).
pub trait PassthroughSlot {
    fn passthrough_slot(&mut self) -> &mut Option<Box<BodyPassthrough>>;
}

impl PassthroughSlot for Block {
    fn passthrough_slot(&mut self) -> &mut Option<Box<BodyPassthrough>> {
        self.body_xml_mut()
    }
}

impl PassthroughSlot for TableRow {
    fn passthrough_slot(&mut self) -> &mut Option<Box<BodyPassthrough>> {
        &mut self
            .source_markup
            .get_or_insert_with(Default::default)
            .body_xml
    }
}

impl PassthroughSlot for TableCell {
    fn passthrough_slot(&mut self) -> &mut Option<Box<BodyPassthrough>> {
        &mut self
            .source_markup
            .get_or_insert_with(Default::default)
            .body_xml
    }
}

/// One block-level container (`<w:sdt>` / `<w:customXml>`) the reader is
/// currently inside.
#[derive(Debug)]
struct OpenContainer {
    id: u32,
    /// Offset of the container's `<`.
    start: usize,
    /// Index of the `Open` placeholder in `pending` at open time.
    placeholder: usize,
    /// `blocks.len()` at open time — the first inner block lands here.
    blocks_at_open: usize,
    /// Offset of the first inner block's (or nested container's) `<`.
    first_block_start: Option<usize>,
    /// Offset just past the last inner block's (or nested container's)
    /// end tag.
    last_block_end: Option<usize>,
    /// `pending.len()` right after the last inner block (or nested
    /// container) ended: anything captured after that point lies inside
    /// the closer's bytes and is dropped when the container closes.
    pending_at_last_end: usize,
}

/// Reader-side tracker of block-level passthrough markup for ONE block
/// container. Create one per `<w:body>` / `<w:tc>`.
#[derive(Debug, Default)]
pub struct BlockEnvelopes {
    /// Fragments waiting for the next block to attach to (its `before`).
    pending: Vec<BodyFragment>,
    stack: Vec<OpenContainer>,
    next_id: u32,
}

impl BlockEnvelopes {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` while inside an envelope element (so its property children
    /// — `<w:sdtPr>`, `<w:sdtEndPr>`, `<w:customXmlPr>` — can be skipped).
    pub fn in_container(&self) -> bool {
        !self.stack.is_empty()
    }

    /// A self-contained block-level fragment (`<w:bookmarkStart …/>`,
    /// inter-block whitespace): attaches before the next block.
    pub fn push_verbatim(&mut self, xml: Vec<u8>) {
        if !xml.is_empty() {
            self.pending.push(BodyFragment::Verbatim { xml });
        }
    }

    /// An envelope element opened at byte offset `start`.
    pub fn open_container(&mut self, start: usize) {
        if let Some(parent) = self.stack.last_mut() {
            parent.first_block_start.get_or_insert(start);
        }
        let id = self.next_id;
        self.next_id += 1;
        let placeholder = self.pending.len();
        /* The opener's bytes are known only once the first inner block
        starts and the closer's once the envelope ends; the placeholder
        keeps the fragment's position among the pending markers. */
        self.pending.push(BodyFragment::Open {
            id,
            open_xml: Vec::new(),
            close_xml: Vec::new(),
        });
        self.stack.push(OpenContainer {
            id,
            start,
            placeholder,
            blocks_at_open: usize::MAX,
            first_block_start: None,
            last_block_end: None,
            pending_at_last_end: placeholder + 1,
        });
    }

    /// Record the block count of the container the envelope lives in —
    /// called right after [`Self::open_container`] with the current
    /// `blocks.len()` (kept separate so the walkers do not need the
    /// block list at open time).
    pub fn set_blocks_at_open(&mut self, blocks_len: usize) {
        if let Some(top) = self.stack.last_mut()
            && top.blocks_at_open == usize::MAX
        {
            top.blocks_at_open = blocks_len;
        }
    }

    /// A block (`<w:p>` / `<w:tbl>`) starts at byte offset `start`.
    pub fn note_block_start(&mut self, start: usize) {
        /* Every container still waiting for its first inner block gets
        this one. Whatever was captured since the OUTERMOST of them opened
        (whitespace between `<w:sdt>`, `<w:sdtPr>`, `<w:sdtContent>` and
        the block) lies inside those containers' openers and would be
        emitted twice: drop it, keeping only the `Open` placeholders. */
        let mut earliest: Option<usize> = None;
        for c in self.stack.iter_mut().rev() {
            if c.first_block_start.is_some() {
                break;
            }
            c.first_block_start = Some(start);
            earliest = Some(c.placeholder);
        }
        if let Some(from) = earliest {
            /* Blank rather than remove: the placeholder indices recorded
            on the stack must stay valid. `take_before` drops the blanks. */
            for f in self.pending.iter_mut().skip(from) {
                if !matches!(f, BodyFragment::Open { .. }) {
                    *f = BodyFragment::Verbatim { xml: Vec::new() };
                }
            }
        }
    }

    /// The block being pushed takes every pending fragment as its
    /// `before` markup.
    pub fn take_before(&mut self) -> Option<Box<BodyPassthrough>> {
        let before: Vec<BodyFragment> = std::mem::take(&mut self.pending)
            .into_iter()
            .filter(|f| !matches!(f, BodyFragment::Verbatim { xml } if xml.is_empty()))
            .collect();
        if before.is_empty() {
            return None;
        }
        Some(Box::new(BodyPassthrough {
            before,
            after: Vec::new(),
        }))
    }

    /// A block ended at byte offset `end` (just past its end tag).
    pub fn note_block_end(&mut self, end: usize) {
        if let Some(top) = self.stack.last_mut() {
            top.last_block_end = Some(end);
            top.pending_at_last_end = self.pending.len();
        }
    }

    /// The innermost envelope element ended at byte offset `end`. `blocks`
    /// is the container's block list the inner blocks were pushed to.
    pub fn close_container<T: PassthroughSlot>(
        &mut self,
        xml: &[u8],
        end: usize,
        blocks: &mut [T],
    ) {
        let Some(top) = self.stack.pop() else {
            return;
        };
        let inner_blocks = top.blocks_at_open != usize::MAX && blocks.len() > top.blocks_at_open;
        if inner_blocks {
            /* Whatever was captured after the last inner block ended
            (whitespace before `</w:sdtContent>`) lies inside the closer's
            bytes. */
            self.pending.truncate(top.pending_at_last_end);
        }
        if let Some(parent) = self.stack.last_mut() {
            parent.last_block_end = Some(end);
            parent.pending_at_last_end = self.pending.len();
        }
        let bounds = match (top.first_block_start, top.last_block_end) {
            (Some(first), Some(last)) if inner_blocks && top.start <= first && last <= end => {
                Some((first, last))
            }
            _ => None,
        };
        match bounds {
            Some((first, last)) => {
                let open_xml = xml[top.start..first].to_vec();
                let close_xml = xml[last..end].to_vec();
                /* Patch the placeholder that the first inner block drained
                into its `before`. */
                let patched = blocks
                    .get_mut(top.blocks_at_open)
                    .and_then(|b| b.passthrough_slot().as_deref_mut())
                    .and_then(|bx| {
                        bx.before.iter_mut().find_map(|f| match f {
                            BodyFragment::Open { id, .. } if *id == top.id => Some(f),
                            _ => None,
                        })
                    });
                match patched {
                    Some(slot) => {
                        *slot = BodyFragment::Open {
                            id: top.id,
                            open_xml,
                            close_xml: close_xml.clone(),
                        };
                        if let Some(last_block) = blocks.last_mut() {
                            last_block
                                .passthrough_slot()
                                .get_or_insert_with(Default::default)
                                .after
                                .push(BodyFragment::Close { id: top.id });
                        }
                    }
                    /* The placeholder is not where it should be (cannot
                    happen with the walkers' call discipline); keep the
                    whole element opaquely rather than half of it. */
                    None => {
                        self.replace_placeholder_with_verbatim(top.placeholder, xml, top.start, end)
                    }
                }
            }
            None => self.replace_placeholder_with_verbatim(top.placeholder, xml, top.start, end),
        }
    }

    /// No block was pushed inside the envelope (an empty content control):
    /// the whole element is one verbatim fragment, replacing its
    /// placeholder and anything captured after it.
    fn replace_placeholder_with_verbatim(
        &mut self,
        placeholder: usize,
        xml: &[u8],
        start: usize,
        end: usize,
    ) {
        if placeholder <= self.pending.len() {
            self.pending.truncate(placeholder);
        }
        if start < end && end <= xml.len() {
            self.pending.push(BodyFragment::Verbatim {
                xml: xml[start..end].to_vec(),
            });
        }
    }

    /// The container ended: whatever is still pending (markers or
    /// whitespace after the last block) attaches AFTER the last block.
    /// Unclosed envelopes degrade to their verbatim placeholder-less
    /// markers: nothing is emitted for them.
    pub fn finish<T: PassthroughSlot>(&mut self, blocks: &mut [T]) {
        self.stack.clear();
        let pending: Vec<BodyFragment> = std::mem::take(&mut self.pending)
            .into_iter()
            .filter(|f| matches!(f, BodyFragment::Verbatim { .. }))
            .collect();
        if pending.is_empty() {
            return;
        }
        if let Some(last) = blocks.last_mut() {
            last.passthrough_slot()
                .get_or_insert_with(Default::default)
                .after
                .extend(pending);
        }
    }
}

/// Writer-side mirror: emits a block's `before` / `after` fragments and
/// keeps the open envelopes balanced.
#[derive(Debug, Default)]
pub struct EnvelopeStack {
    open: Vec<(u32, Vec<u8>)>,
}

impl EnvelopeStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Emit `fragments` in order. An `Open` already on the stack (a
    /// duplicated opener) is ignored; a `Close` whose envelope is open
    /// closes it AND every envelope opened inside it (their own closers
    /// were lost); a `Close` whose envelope is not open is skipped.
    pub fn emit(&mut self, fragments: &[BodyFragment], out: &mut String) {
        for f in fragments {
            match f {
                BodyFragment::Verbatim { xml } => push_bytes(xml, out),
                BodyFragment::Open {
                    id,
                    open_xml,
                    close_xml,
                } => {
                    if open_xml.is_empty() || self.open.iter().any(|(i, _)| i == id) {
                        continue;
                    }
                    push_bytes(open_xml, out);
                    self.open.push((*id, close_xml.clone()));
                }
                BodyFragment::Close { id } => {
                    if let Some(pos) = self.open.iter().rposition(|(i, _)| i == id) {
                        while self.open.len() > pos {
                            if let Some((_, close)) = self.open.pop() {
                                push_bytes(&close, out);
                            }
                        }
                    }
                }
            }
        }
    }

    /// The container is ending: close every envelope still open, innermost
    /// first.
    pub fn finish(&mut self, out: &mut String) {
        while let Some((_, close)) = self.open.pop() {
            push_bytes(&close, out);
        }
    }
}

fn push_bytes(xml: &[u8], out: &mut String) {
    if let Ok(s) = std::str::from_utf8(xml) {
        out.push_str(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::Paragraph;

    fn para(text: &str) -> Block {
        Block::Paragraph(Paragraph {
            text: text.into(),
            ..Default::default()
        })
    }

    /// Drive the tracker the way the body walker does over
    /// `<w:bookmarkStart/><w:sdt><w:sdtPr/><w:sdtContent><w:p/><w:p/></w:sdtContent></w:sdt><w:bookmarkEnd/>`.
    #[test]
    fn envelope_opens_on_first_inner_block_and_closes_on_last() {
        let xml = b"<w:bookmarkStart w:id=\"0\" w:name=\"b\"/><w:sdt><w:sdtPr/><w:sdtContent><w:p/><w:p/></w:sdtContent></w:sdt><w:bookmarkEnd w:id=\"0\"/>";
        let bm_end = "<w:bookmarkStart w:id=\"0\" w:name=\"b\"/>".len();
        let sdt_start = bm_end;
        let content_open_end = sdt_start + "<w:sdt><w:sdtPr/><w:sdtContent>".len();
        let p1_start = content_open_end;
        let p1_end = p1_start + "<w:p/>".len();
        let p2_start = p1_end;
        let p2_end = p2_start + "<w:p/>".len();
        let sdt_end = p2_end + "</w:sdtContent></w:sdt>".len();

        let mut blocks: Vec<Block> = Vec::new();
        let mut env = BlockEnvelopes::new();
        env.push_verbatim(xml[..bm_end].to_vec());
        env.open_container(sdt_start);
        env.set_blocks_at_open(blocks.len());
        env.note_block_start(p1_start);
        let mut p1 = para("one");
        *p1.body_xml_mut() = env.take_before();
        blocks.push(p1);
        env.note_block_end(p1_end);
        env.note_block_start(p2_start);
        let mut p2 = para("two");
        *p2.body_xml_mut() = env.take_before();
        blocks.push(p2);
        env.note_block_end(p2_end);
        env.close_container(xml, sdt_end, &mut blocks);
        env.push_verbatim(xml[sdt_end..].to_vec());
        env.finish(&mut blocks);

        let b0 = blocks[0].body_xml().expect("first block markup");
        assert_eq!(b0.before.len(), 2);
        assert!(
            matches!(&b0.before[0], BodyFragment::Verbatim { xml } if xml.starts_with(b"<w:bookmarkStart"))
        );
        match &b0.before[1] {
            BodyFragment::Open {
                id,
                open_xml,
                close_xml,
            } => {
                assert_eq!(*id, 0);
                assert_eq!(open_xml, b"<w:sdt><w:sdtPr/><w:sdtContent>");
                assert_eq!(close_xml, b"</w:sdtContent></w:sdt>");
            }
            other => panic!("expected Open, got {other:?}"),
        }
        assert!(b0.after.is_empty());
        let b1 = blocks[1].body_xml().expect("last block markup");
        assert!(b1.before.is_empty());
        assert_eq!(b1.after.len(), 2);
        assert!(matches!(&b1.after[0], BodyFragment::Close { id: 0 }));
        assert!(
            matches!(&b1.after[1], BodyFragment::Verbatim { xml } if xml.starts_with(b"<w:bookmarkEnd"))
        );

        /* The writer reproduces the source around the two blocks. */
        let mut out = String::new();
        let mut stack = EnvelopeStack::new();
        for b in &blocks {
            if let Some(bx) = b.body_xml() {
                stack.emit(&bx.before, &mut out);
            }
            out.push_str("<w:p/>");
            if let Some(bx) = b.body_xml() {
                stack.emit(&bx.after, &mut out);
            }
        }
        stack.finish(&mut out);
        assert_eq!(out.as_bytes(), xml);
    }

    #[test]
    fn empty_envelope_becomes_one_verbatim_fragment_and_nesting_patches_the_parent() {
        /* Outer wraps [inner-empty, p]. */
        let xml =
            b"<w:sdt><w:sdtContent><w:sdt><w:sdtContent/></w:sdt><w:p/></w:sdtContent></w:sdt>";
        let inner_start = "<w:sdt><w:sdtContent>".len();
        let inner_end = inner_start + "<w:sdt><w:sdtContent/></w:sdt>".len();
        let p_start = inner_end;
        let p_end = p_start + "<w:p/>".len();
        let outer_end = xml.len();

        let mut blocks: Vec<Block> = Vec::new();
        let mut env = BlockEnvelopes::new();
        env.open_container(0);
        env.set_blocks_at_open(blocks.len());
        env.open_container(inner_start);
        env.set_blocks_at_open(blocks.len());
        env.close_container(xml, inner_end, &mut blocks);
        env.note_block_start(p_start);
        let mut p = para("x");
        *p.body_xml_mut() = env.take_before();
        blocks.push(p);
        env.note_block_end(p_end);
        env.close_container(xml, outer_end, &mut blocks);
        env.finish(&mut blocks);

        let bx = blocks[0].body_xml().expect("markup");
        assert_eq!(bx.before.len(), 2, "{bx:?}");
        assert!(
            matches!(&bx.before[0], BodyFragment::Open { open_xml, .. } if open_xml == b"<w:sdt><w:sdtContent>")
        );
        assert!(
            matches!(&bx.before[1], BodyFragment::Verbatim { xml } if xml == b"<w:sdt><w:sdtContent/></w:sdt>")
        );
        assert!(matches!(&bx.after[..], [BodyFragment::Close { id: 0 }]));

        let mut out = String::new();
        let mut stack = EnvelopeStack::new();
        stack.emit(&bx.before, &mut out);
        out.push_str("<w:p/>");
        stack.emit(&bx.after, &mut out);
        stack.finish(&mut out);
        assert_eq!(out.as_bytes(), xml);
    }

    #[test]
    fn writer_stack_survives_lost_and_duplicated_ends() {
        let open = |id: u32| BodyFragment::Open {
            id,
            open_xml: format!("<o{id}>").into_bytes(),
            close_xml: format!("</o{id}>").into_bytes(),
        };
        let mut out = String::new();
        let mut stack = EnvelopeStack::new();
        /* Duplicated opener (split paragraph): opens once. */
        stack.emit(&[open(1)], &mut out);
        out.push('a');
        stack.emit(&[open(1)], &mut out);
        out.push('b');
        /* Nested envelope whose own closer is lost: closing the outer
        closes it too, innermost first. */
        stack.emit(&[open(2)], &mut out);
        out.push('c');
        stack.emit(&[BodyFragment::Close { id: 1 }], &mut out);
        /* Closer without an opener: skipped. */
        stack.emit(&[BodyFragment::Close { id: 9 }], &mut out);
        /* Unclosed envelope at the end of the container. */
        stack.emit(&[open(3)], &mut out);
        out.push('d');
        stack.finish(&mut out);
        assert_eq!(out, "<o1>ab<o2>c</o2></o1><o3>d</o3>");
    }

    #[test]
    fn pending_markers_after_the_last_block_attach_after_it() {
        let mut blocks = vec![para("only")];
        let mut env = BlockEnvelopes::new();
        env.push_verbatim(b"<w:bookmarkEnd w:id=\"3\"/>".to_vec());
        env.push_verbatim(Vec::new());
        env.finish(&mut blocks);
        let bx = blocks[0].body_xml().expect("markup");
        assert!(bx.before.is_empty());
        assert!(
            matches!(&bx.after[..], [BodyFragment::Verbatim { xml }] if xml == b"<w:bookmarkEnd w:id=\"3\"/>")
        );
        /* No block at all: nothing to attach to, nothing panics. */
        let mut none: Vec<Block> = Vec::new();
        let mut env = BlockEnvelopes::new();
        env.push_verbatim(b"<w:bookmarkStart/>".to_vec());
        env.finish(&mut none);
        assert!(none.is_empty());
    }
}
