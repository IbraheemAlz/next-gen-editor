//! Issue #81 — Table of Contents.
//!
//! A TOC is a complex field (`TOC \o "1-3" \h \z \u`) whose RESULT is a
//! run of paragraphs — one per collected heading — rather than a string.
//! The engine represents it with the multi-paragraph field overlays
//! ([`crate::FieldSpan`]): a `Head` on the first result paragraph (the
//! `begin` + instruction + `separate`) and a `Tail` on the last (the
//! `end`). Every sibling paragraph between them belongs to the result.
//!
//! This module owns the model-level half of the feature:
//!
//! 1. **Regions** — [`DocumentTree::toc_regions`] pairs each Head with the
//!    next Tail among the following top-level siblings (document order,
//!    no index side table, so edits inside the result never desync it).
//! 2. **Heading collection** — [`DocumentTree::toc_headings`] walks the
//!    body paragraphs outside every TOC and resolves each one's outline
//!    level through the paragraph style cascade (`<w:outlineLvl>`, with a
//!    `heading N` style-name fallback), the direct outline level (`\u`)
//!    and custom style mappings (`\t`).
//! 3. **Regeneration** — [`DocumentTree::regenerate_tocs`] replaces each
//!    region with freshly synthesized `TOC 1..9` entry paragraphs: the
//!    heading text, a right-aligned dot-leader tab at the column's right
//!    edge, the page number (supplied by the caller — the engine's
//!    layout post-pass resolves heading pages from a real pagination),
//!    and with `\h` an internal hyperlink to a `_Toc*` bookmark stamped
//!    on the heading plus a nested `PAGEREF` field over the number (the
//!    shape Word writes).
//! 4. **Insertion** — [`DocumentTree::insert_toc_at`] drops an empty TOC
//!    stub at the caret for the caller to regenerate.
//!
//! Page numbers are addressed by *content ordinal*: the index of a
//! top-level block in [`DocumentTree::toc_content_blocks`] (every body
//! block outside a TOC region). Regeneration only rewrites regions, so
//! a heading's ordinal is identical before and after — which is what
//! lets the layout post-pass compare page vectors across rounds.

use crate::{
    Block, DocumentTree, Field, FieldSpan, Hyperlink, Indent, LogicalPos, ParaProperties,
    Paragraph, PathStep, Spacing, TabKind, TabLeader, TabStop, TocSwitches, TypedField,
};

/// One TOC region in the body: blocks `first..=last` hold the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocRegion {
    /// Top-level block of the Head paragraph.
    pub first: u32,
    /// Top-level block of the Tail paragraph (`== first` for a
    /// one-paragraph result, or when the Tail is missing).
    pub last: u32,
    /// Index of the Head overlay in the first paragraph's `fields`.
    pub field_index: usize,
    /// The TOC field code.
    pub instruction: String,
    /// `false` when no matching Tail was found (the field's `end` was
    /// deleted). An open region is never regenerated: without its end
    /// the engine cannot tell the stale result from the user's own text,
    /// and regeneration must never drop content.
    pub closed: bool,
}

/// One heading a TOC collects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocHeading {
    /// Index into [`DocumentTree::toc_content_blocks`].
    pub content_ordinal: usize,
    /// Top-level block index in the tree it was collected from.
    pub block: u32,
    /// 1-based TOC level.
    pub level: u8,
    /// Entry text (tabs / breaks folded to spaces, objects dropped).
    pub text: String,
    /// The heading's existing `_Toc*` bookmark, if any.
    pub bookmark: Option<String>,
}

/// One synthesized entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    pub level: u8,
    pub text: String,
    /// `None` when the level hides numbers (`\n`) or no layout was
    /// available to resolve one.
    pub page: Option<String>,
    /// `_Toc*` bookmark the entry links to (`\h`).
    pub anchor: Option<String>,
}

/// Word's placeholder when a TOC collects nothing.
pub const NO_ENTRIES_TEXT: &str = "No table of contents entries found.";

/// Word's `TOC N` style indent step: 11 pt (220 twips) per level.
const TOC_LEVEL_INDENT_TWIPS: i32 = 220;
/// Word's `TOC N` spacing-after (5 pt).
const TOC_SPACING_AFTER_TWIPS: i32 = 100;

/// `true` for the Head of a multi-paragraph `TOC` field. Other
/// multi-paragraph fields (a long `IF`, a citation `ADDIN`) keep their
/// Head/Tail overlays for the round-trip but are never regenerated.
fn is_toc_head(f: &Field) -> bool {
    f.span == Some(FieldSpan::Head) && f.keyword() == "TOC"
}

/// `true` when a bookmark name is a TOC heading anchor.
pub fn is_toc_bookmark(name: &str) -> bool {
    name.starts_with("_Toc")
}

/// 1-based level of a built-in heading style addressed by id or name
/// (`Heading1`, `heading 1`, `Heading 1`). Word's stock heading styles
/// carry `<w:outlineLvl>`; this fallback covers stylesheets (and
/// engine-created documents) that reference them without defining them.
fn heading_style_level(id_or_name: &str) -> Option<u8> {
    let s: String = id_or_name
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    let n = s.strip_prefix("heading")?.parse::<u8>().ok()?;
    (1..=9).contains(&n).then_some(n)
}

/// Fold a heading paragraph's text into entry text.
fn entry_text(p: &Paragraph) -> String {
    let mut out = String::with_capacity(p.text.len());
    for ch in p.text.chars() {
        match ch {
            '\t' | '\u{2028}' | '\u{000C}' | '\n' => out.push(' '),
            '\u{FFFC}' => {}
            c => out.push(c),
        }
    }
    out.trim().to_string()
}

impl DocumentTree {
    /// Every TOC region in the top-level body, in document order.
    pub fn toc_regions(&self) -> Vec<TocRegion> {
        let mut out = Vec::new();
        let n = self.blocks.len();
        let mut i = 0usize;
        while i < n {
            let Some(Block::Paragraph(p)) = self.blocks.get(i) else {
                i += 1;
                continue;
            };
            let Some((fi, head)) = p.fields.iter().enumerate().find(|(_, f)| is_toc_head(f)) else {
                i += 1;
                continue;
            };
            /* Same-paragraph Tail (a one-paragraph result). */
            let same = p
                .fields
                .iter()
                .any(|f| f.span == Some(FieldSpan::Tail) && f.end >= head.start);
            let mut last = i;
            let mut closed = same;
            if !same {
                let mut j = i + 1;
                while j < n {
                    match self.blocks.get(j) {
                        Some(Block::Paragraph(q)) => {
                            if q.fields.iter().any(|f| f.span == Some(FieldSpan::Head)) {
                                /* Another TOC begins before this one
                                closed — stop (orphan Head). */
                                break;
                            }
                            if q.fields.iter().any(|f| f.span == Some(FieldSpan::Tail)) {
                                last = j;
                                closed = true;
                                break;
                            }
                        }
                        /* A table never sits inside a TOC result; stop
                        rather than swallow it. */
                        _ => break,
                    }
                    j += 1;
                }
            }
            out.push(TocRegion {
                first: i as u32,
                last: last as u32,
                field_index: fi,
                instruction: head.instruction.clone(),
                closed,
            });
            i = last + 1;
        }
        out
    }

    /// `true` when the body holds at least one TOC.
    pub fn has_toc(&self) -> bool {
        self.blocks
            .iter()
            .any(|b| matches!(b, Block::Paragraph(p) if p.fields.iter().any(is_toc_head)))
    }

    /// Top-level block indices OUTSIDE every TOC region, in order — the
    /// ordinal space [`TocHeading::content_ordinal`] indexes.
    pub fn toc_content_blocks(&self) -> Vec<u32> {
        let regions = self.toc_regions();
        (0..self.blocks.len() as u32)
            .filter(|b| !regions.iter().any(|r| r.first <= *b && *b <= r.last))
            .collect()
    }

    /// 1-based TOC level of `p` under `switches`, or `None` when this
    /// TOC does not collect it. Custom styles (`\t`) win; then the
    /// outline level — the direct `<w:outlineLvl>` when `\u` is on, else
    /// the style cascade's (with the `heading N` name fallback) — filtered
    /// by the `\o` range.
    pub fn toc_level_of(&self, p: &Paragraph, switches: &TocSwitches) -> Option<u8> {
        let style_id = p.style_id.as_deref();
        let style_name = style_id
            .and_then(|id| self.styles.get(id))
            .map(|s| s.name.as_str());
        for (name, lvl) in &switches.custom_styles {
            if style_id.is_some_and(|id| id.eq_ignore_ascii_case(name))
                || style_name.is_some_and(|n| n.eq_ignore_ascii_case(name))
            {
                return Some(*lvl);
            }
        }
        let style_level = self
            .resolve_style_cascade(style_id)
            .outline_level
            .filter(|l| *l < 9)
            .map(|l| l + 1)
            .or_else(|| style_id.and_then(heading_style_level))
            .or_else(|| style_name.and_then(heading_style_level));
        let direct_level = p
            .direct_overrides
            .outline_level
            .filter(|l| *l < 9)
            .map(|l| l + 1);
        let level = if switches.use_outline_levels {
            direct_level.or(style_level)
        } else if switches.outline_levels.is_some() {
            style_level
        } else {
            None
        }?;
        let (lo, hi) = switches.outline_levels.unwrap_or((1, 9));
        (lo..=hi).contains(&level).then_some(level)
    }

    /// Headings a TOC with `switches` collects, in document order.
    /// Paragraphs inside any TOC region never contribute; empty headings
    /// are skipped (Word parity). Table-cell headings are not collected.
    pub fn toc_headings(&self, switches: &TocSwitches) -> Vec<TocHeading> {
        let mut out = Vec::new();
        for (ord, b) in self.toc_content_blocks().into_iter().enumerate() {
            let Some(Block::Paragraph(p)) = self.blocks.get(b as usize) else {
                continue;
            };
            let Some(level) = self.toc_level_of(p, switches) else {
                continue;
            };
            let text = entry_text(p);
            if text.is_empty() {
                continue;
            }
            out.push(TocHeading {
                content_ordinal: ord,
                block: b,
                level,
                text,
                bookmark: p
                    .bookmarks
                    .iter()
                    .find(|b| is_toc_bookmark(&b.name))
                    .map(|b| b.name.clone()),
            });
        }
        out
    }

    /// Regenerate every TOC region. `page_of(content_ordinal)` supplies
    /// the displayed page number of a heading (the engine's layout
    /// post-pass). Returns the new tree and whether anything changed —
    /// a TOC whose regenerated result equals its current one is left
    /// byte-stable (no dirtying).
    pub fn regenerate_tocs(
        &self,
        page_of: &dyn Fn(usize) -> Option<String>,
    ) -> (DocumentTree, bool) {
        let regions = self.toc_regions();
        if regions.is_empty() {
            return (self.clone(), false);
        }
        /* 1. `\h` TOCs link to `_Toc*` bookmarks: stamp one on every
        collected heading that lacks it. Deterministic names so a second
        regeneration is a no-op. */
        let mut doc = self.clone();
        let mut changed = false;
        let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
        for b in doc.blocks.iter() {
            if let Block::Paragraph(p) = b {
                taken.extend(p.bookmarks.iter().map(|b| b.name.clone()));
            }
        }
        let mut next_id: u64 = 1;
        for r in &regions {
            let sw = TocSwitches::from_instruction(&crate::FieldInstruction::parse(&r.instruction));
            if !sw.hyperlinks {
                continue;
            }
            for h in doc.toc_headings(&sw) {
                if h.bookmark.is_some() {
                    continue;
                }
                let name = loop {
                    let cand = format!("_Toc{}", 100_000_000 + next_id);
                    next_id += 1;
                    if !taken.contains(&cand) {
                        break cand;
                    }
                };
                taken.insert(name.clone());
                if let Some(Block::Paragraph(p)) = doc.blocks.get(h.block as usize) {
                    let mut np = p.clone();
                    np.bookmarks.push(crate::Bookmark { name, id: None });
                    np.dirty = true;
                    np.source_xml = None;
                    doc.blocks.set(h.block as usize, Block::Paragraph(np));
                    changed = true;
                }
            }
        }
        /* 2. Replace each region (last first, so earlier indices hold). */
        for r in regions.iter().rev().filter(|r| r.closed) {
            let sw = TocSwitches::from_instruction(&crate::FieldInstruction::parse(&r.instruction));
            let entries: Vec<TocEntry> = doc
                .toc_headings(&sw)
                .into_iter()
                .map(|h| TocEntry {
                    level: h.level,
                    page: if sw.shows_page_number(h.level) {
                        page_of(h.content_ordinal)
                    } else {
                        None
                    },
                    anchor: if sw.hyperlinks { h.bookmark } else { None },
                    text: h.text,
                })
                .collect();
            let fresh = doc.toc_result_paragraphs(r, &sw, &entries);
            let old: Vec<&Paragraph> = (r.first..=r.last)
                .filter_map(|b| match doc.blocks.get(b as usize) {
                    Some(Block::Paragraph(p)) => Some(p),
                    _ => None,
                })
                .collect();
            if same_result(&old, &fresh) {
                continue;
            }
            let inserted = fresh.len() as u32;
            let mut blocks = doc.blocks.clone();
            for _ in r.first..=r.last {
                blocks.remove(r.first as usize);
            }
            for (k, p) in fresh.into_iter().enumerate() {
                blocks.insert(r.first as usize + k, Block::Paragraph(p));
            }
            doc.blocks = blocks;
            /* Issue #152 — the result grew / shrank: blocks after it
            moved, anchors inside it land on the fresh result. */
            doc.remap_block_splice(&[], r.first, r.last - r.first + 1, inserted);
            changed = true;
        }
        (doc, changed)
    }

    /// Build the result paragraphs for region `r`. Text before the
    /// Head's `begin` in the first paragraph and after the Tail's `end`
    /// in the last one is preserved around the result; a section marker
    /// riding the last paragraph's mark stays on the last paragraph.
    fn toc_result_paragraphs(
        &self,
        r: &TocRegion,
        sw: &TocSwitches,
        entries: &[TocEntry],
    ) -> Vec<Paragraph> {
        let first = match self.blocks.get(r.first as usize) {
            Some(Block::Paragraph(p)) => p.clone(),
            _ => Paragraph::default(),
        };
        let last = match self.blocks.get(r.last as usize) {
            Some(Block::Paragraph(p)) => p.clone(),
            _ => first.clone(),
        };
        let head_start = first
            .fields
            .get(r.field_index)
            .map(|f| f.start.min(first.text.len() as u32))
            .unwrap_or(0);
        let prefix = first
            .text
            .get(..head_start as usize)
            .unwrap_or("")
            .to_string();
        let suffix = if r.closed {
            last.fields
                .iter()
                .filter(|f| f.span == Some(FieldSpan::Tail))
                .map(|f| f.end)
                .max()
                .and_then(|e| last.text.get(e as usize..))
                .unwrap_or("")
                .to_string()
        } else {
            String::new()
        };
        let tab_pos = self.section_for_block(r.first).column_width_pt();
        let mut paras: Vec<Paragraph> = if entries.is_empty() {
            vec![Paragraph {
                text: NO_ENTRIES_TEXT.to_string(),
                dirty: true,
                ..Default::default()
            }]
        } else {
            entries
                .iter()
                .map(|e| self.toc_entry_paragraph(e, sw, tab_pos))
                .collect()
        };
        /* Prefix / suffix + the field ends. */
        let n = paras.len();
        {
            let p0 = &mut paras[0];
            if !prefix.is_empty() {
                *p0 = prefix_paragraph(p0, &prefix);
            }
            p0.fields.push(Field {
                start: prefix.len() as u32,
                end: p0.text.len() as u32,
                instruction: r.instruction.clone(),
                span: Some(FieldSpan::Head),
            });
        }
        {
            let pl = &mut paras[n - 1];
            let end = pl.text.len() as u32;
            pl.text.push_str(&suffix);
            pl.fields.push(Field {
                start: 0,
                end,
                instruction: String::new(),
                span: Some(FieldSpan::Tail),
            });
            pl.section_end = last.section_end.clone();
        }
        for p in paras.iter_mut() {
            p.fields.sort_by_key(|f| (f.start, f.span.is_none()));
        }
        paras
    }

    /// One `TOC N` entry paragraph.
    fn toc_entry_paragraph(&self, e: &TocEntry, sw: &TocSwitches, tab_pos_pt: f32) -> Paragraph {
        let style_id = format!("TOC{}", e.level);
        let style_defined = self.styles.contains_key(&style_id);
        let mut direct = ParaProperties {
            tab_stops: vec![TabStop {
                position_pt: tab_pos_pt,
                kind: TabKind::Right,
                leader: TabLeader::Dot,
            }],
            ..Default::default()
        };
        if !style_defined {
            /* The stylesheet lacks Word's `TOC N` styles — carry their
            geometry directly so the entry still indents per level. */
            direct.indent = Indent {
                start_twips: (e.level.saturating_sub(1) as i32) * TOC_LEVEL_INDENT_TWIPS,
                ..Default::default()
            };
            direct.spacing = Spacing {
                after_twips: TOC_SPACING_AFTER_TWIPS,
                ..Default::default()
            };
        }
        let mut text = e.text.clone();
        let show_number = sw.shows_page_number(e.level);
        let mut page_range: Option<(u32, u32)> = None;
        if show_number {
            text.push('\t');
            if let Some(pg) = &e.page {
                let s = text.len() as u32;
                text.push_str(pg);
                page_range = Some((s, text.len() as u32));
            }
        }
        let mut hyperlinks = Vec::new();
        let mut fields = Vec::new();
        if let Some(anchor) = &e.anchor {
            hyperlinks.push(Hyperlink {
                start: 0,
                end: text.len() as u32,
                target: format!("#{anchor}"),
            });
            if let Some((s, t)) = page_range {
                fields.push(Field {
                    start: s,
                    end: t,
                    instruction: format!("PAGEREF {anchor} \\h"),
                    span: None,
                });
            }
        }
        let props = self
            .resolve_style_cascade(Some(&style_id))
            .merged_with(direct.clone());
        Paragraph {
            text,
            props,
            style_id: Some(style_id),
            direct_overrides: direct,
            hyperlinks,
            fields,
            dirty: true,
            ..Default::default()
        }
    }

    /// Issue #81 — insert an empty TOC stub (Head + Tail on one empty
    /// paragraph carrying `switches`' instruction) at `at`: before the
    /// caret's paragraph when the caret is at its start, after it at its
    /// end, else the paragraph is split and the stub goes between.
    /// Top-level body only. Returns the tree and the stub's block index;
    /// the caller regenerates (with page numbers) afterwards.
    pub fn insert_toc_at(&self, at: &LogicalPos, switches: &TocSwitches) -> Option<(Self, u32)> {
        let [PathStep::Block(idx)] = at.path.steps.as_slice() else {
            return None;
        };
        let idx = *idx;
        let p = match self.blocks.get(idx as usize)? {
            Block::Paragraph(p) => p,
            Block::Table(_) => return None,
        };
        /* Never nest a TOC inside another TOC's result. */
        if self
            .toc_regions()
            .iter()
            .any(|r| r.first <= idx && idx <= r.last)
        {
            return None;
        }
        let len = p.text.len() as u32;
        let off = at.offset.min(len);
        let (mut doc, insert_at) = if off == 0 && len > 0 {
            (self.clone(), idx)
        } else if off >= len {
            (self.clone(), idx + 1)
        } else {
            (self.split_paragraph(at.clone()), idx + 1)
        };
        let stub = Paragraph {
            fields: vec![
                Field {
                    start: 0,
                    end: 0,
                    instruction: switches.to_instruction(),
                    span: Some(FieldSpan::Head),
                },
                Field {
                    start: 0,
                    end: 0,
                    instruction: String::new(),
                    span: Some(FieldSpan::Tail),
                },
            ],
            dirty: true,
            ..Default::default()
        };
        let mut blocks = doc.blocks.clone();
        let at_idx = (insert_at as usize).min(blocks.len());
        blocks.insert(at_idx, Block::Paragraph(stub));
        doc.blocks = blocks;
        /* Issue #152 — the stub is a new block (the split above already
        remapped through `split_paragraph`). */
        doc.remap_block_indices(at_idx as u32, 1);
        Some((doc, at_idx as u32))
    }

    /// The TOC field of region `r` typed (convenience for callers that
    /// hold a region).
    pub fn toc_switches_of(r: &TocRegion) -> TocSwitches {
        match (Field {
            start: 0,
            end: 0,
            instruction: r.instruction.clone(),
            span: None,
        })
        .typed()
        {
            TypedField::Toc { switches } => switches,
            _ => TocSwitches::from_instruction(&crate::FieldInstruction::parse(&r.instruction)),
        }
    }
}

/// `p` with `prefix` prepended (overlays shifted right).
fn prefix_paragraph(p: &Paragraph, prefix: &str) -> Paragraph {
    let head = Paragraph {
        text: prefix.to_string(),
        ..Default::default()
    };
    let mut out = head.concat(p);
    /* `concat` keeps the head's (empty) formatting; the entry's
    paragraph properties must win. */
    out.props = p.props.clone();
    out.style_id = p.style_id.clone();
    out.direct_overrides = p.direct_overrides.clone();
    let shift = prefix.len() as u32;
    out.hyperlinks = p
        .hyperlinks
        .iter()
        .map(|h| Hyperlink {
            start: h.start + shift,
            end: h.end + shift,
            target: h.target.clone(),
        })
        .collect();
    out
}

/// Structural equality of an existing TOC result with a regenerated one:
/// the same paragraphs with the same text, style, field codes and link
/// targets. Equal results are left untouched (byte-stable save).
fn same_result(old: &[&Paragraph], fresh: &[Paragraph]) -> bool {
    if old.len() != fresh.len() {
        return false;
    }
    /* Canonical field order: the reader pushes a Head when its END is
    found (after the paragraph's local fields), regeneration sorts by
    offset — compare order-insensitively. */
    fn canon(p: &Paragraph) -> Vec<(Option<FieldSpan>, String, u32, u32)> {
        let mut v: Vec<_> = p
            .fields
            .iter()
            .map(|f| {
                let (s, e) = match f.span {
                    None => (f.start, f.end),
                    Some(FieldSpan::Head) => (f.start, 0),
                    Some(FieldSpan::Tail) => (0, f.end),
                };
                (f.span, f.instruction.trim().to_string(), s, e)
            })
            .collect();
        v.sort_by(|a, b| (a.2, a.3, &a.1).cmp(&(b.2, b.3, &b.1)));
        v
    }
    old.iter().zip(fresh).all(|(a, b)| {
        a.text == b.text
            && a.style_id == b.style_id
            && canon(a) == canon(b)
            && a.hyperlinks
                .iter()
                .map(|h| &h.target)
                .eq(b.hyperlinks.iter().map(|h| &h.target))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockPath, ParagraphStyle};

    fn heading(text: &str, level: u8) -> Paragraph {
        Paragraph {
            text: text.into(),
            style_id: Some(format!("Heading{level}")),
            ..Default::default()
        }
    }

    fn body(text: &str) -> Paragraph {
        Paragraph {
            text: text.into(),
            ..Default::default()
        }
    }

    /// Five headings over three levels, a TOC stub at the top.
    pub(crate) fn five_heading_doc() -> DocumentTree {
        let mut doc = DocumentTree::from_text("");
        let paras = vec![
            heading("Introduction", 1),
            body("Intro body."),
            heading("Background", 2),
            body("Background body."),
            heading("Details", 3),
            body("Details body."),
            heading("Method", 1),
            heading("Results", 2),
            body("Results body."),
        ];
        doc.blocks = paras.into_iter().map(Block::Paragraph).collect();
        let (doc, _) = doc
            .insert_toc_at(
                &LogicalPos::new(BlockPath::top(0), 0),
                &TocSwitches::default(),
            )
            .expect("stub");
        doc
    }

    #[test]
    fn switches_parse_and_render() {
        let ins = crate::FieldInstruction::parse(
            "TOC \\o \"2-4\" \\h \\z \\u \\n \"3-3\" \\t \"Title,1,Sub,2\"",
        );
        let sw = TocSwitches::from_instruction(&ins);
        assert_eq!(sw.outline_levels, Some((2, 4)));
        assert!(sw.hyperlinks && sw.hide_in_web && sw.use_outline_levels);
        assert_eq!(sw.no_page_numbers, Some((3, 3)));
        assert_eq!(
            sw.custom_styles,
            vec![("Title".to_string(), 1), ("Sub".to_string(), 2)]
        );
        assert!(!sw.shows_page_number(3));
        assert!(sw.shows_page_number(2));
        let again =
            TocSwitches::from_instruction(&crate::FieldInstruction::parse(&sw.to_instruction()));
        assert_eq!(again, sw);
        assert_eq!(
            TocSwitches::default().to_instruction(),
            "TOC \\o \"1-3\" \\h \\z \\u"
        );
        /* A bare `\n` hides every level. */
        let sw = TocSwitches::from_instruction(&crate::FieldInstruction::parse("TOC \\o \\n"));
        assert_eq!(sw.outline_levels, Some((1, 9)));
        assert!(!sw.shows_page_number(1));
    }

    #[test]
    fn regenerate_emits_one_entry_per_heading_with_leader_tab_and_pages() {
        let doc = five_heading_doc();
        assert!(doc.has_toc());
        let regions = doc.toc_regions();
        assert_eq!(regions.len(), 1);
        assert_eq!((regions[0].first, regions[0].last), (0, 0));
        let (out, changed) = doc.regenerate_tocs(&|ord| Some((ord + 1).to_string()));
        assert!(changed);
        let regions = out.toc_regions();
        assert_eq!(regions.len(), 1);
        assert_eq!((regions[0].first, regions[0].last), (0, 4));
        let texts: Vec<String> = (0..5)
            .map(|i| {
                out.paragraph_at_path(&BlockPath::top(i))
                    .unwrap()
                    .text
                    .clone()
            })
            .collect();
        /* Content ordinals: headings sit at ordinals 0,2,4,6,7. */
        assert_eq!(
            texts,
            vec![
                "Introduction\t1",
                "Background\t3",
                "Details\t5",
                "Method\t7",
                "Results\t8"
            ]
        );
        let styles: Vec<_> = (0..5)
            .map(|i| {
                out.paragraph_at_path(&BlockPath::top(i))
                    .unwrap()
                    .style_id
                    .clone()
                    .unwrap()
            })
            .collect();
        assert_eq!(styles, vec!["TOC1", "TOC2", "TOC3", "TOC1", "TOC2"]);
        let p1 = out.paragraph_at_path(&BlockPath::top(1)).unwrap();
        let stop = p1.props.tab_stops[0];
        assert_eq!(stop.kind, TabKind::Right);
        assert_eq!(stop.leader, TabLeader::Dot);
        assert!((stop.position_pt - out.section_for_block(0).column_width_pt()).abs() < 0.01);
        assert_eq!(p1.props.indent.start_twips, 220);
        /* `\h` — a link to a stamped heading bookmark + PAGEREF. */
        let target = p1.hyperlinks[0].target.clone();
        assert!(target.starts_with("#_Toc"));
        assert_eq!(
            p1.fields[0].instruction,
            format!("PAGEREF {} \\h", &target[1..])
        );
        let bg = out.paragraph_at_path(&BlockPath::top(7)).unwrap();
        assert_eq!(bg.text, "Background");
        assert_eq!(bg.bookmarks.len(), 1);
        assert_eq!(bg.bookmarks[0].name, target[1..]);
        /* Head on the first entry, Tail on the last. */
        let p0 = out.paragraph_at_path(&BlockPath::top(0)).unwrap();
        assert!(
            p0.fields.iter().any(|f| f.span == Some(FieldSpan::Head)
                && f.instruction == "TOC \\o \"1-3\" \\h \\z \\u")
        );
        let p4 = out.paragraph_at_path(&BlockPath::top(4)).unwrap();
        assert!(p4.fields.iter().any(|f| f.span == Some(FieldSpan::Tail)));
        /* Idempotent: a second regeneration with the same pages is a no-op. */
        let (again, changed) = out.regenerate_tocs(&|ord| Some((ord + 1).to_string()));
        assert!(!changed);
        assert_eq!(again.blocks.len(), out.blocks.len());
        /* A page shift is picked up. */
        let (shifted, changed) = out.regenerate_tocs(&|ord| Some((ord + 10).to_string()));
        assert!(changed);
        assert_eq!(
            shifted.paragraph_at_path(&BlockPath::top(4)).unwrap().text,
            "Results\t17"
        );
    }

    #[test]
    fn editing_a_heading_then_regenerating_updates_the_entry() {
        let (out, _) = five_heading_doc().regenerate_tocs(&|_| Some("1".into()));
        /* "Method" heading is block 11 (5 entries + 6th content block). */
        let p = out.paragraph_at_path(&BlockPath::top(11)).unwrap();
        assert_eq!(p.text, "Method");
        let edited = out.insert_text(LogicalPos::new(BlockPath::top(11), 6), "ology");
        let (re, changed) = edited.regenerate_tocs(&|_| Some("1".into()));
        assert!(changed);
        assert_eq!(
            re.paragraph_at_path(&BlockPath::top(3)).unwrap().text,
            "Methodology\t1"
        );
        assert_eq!(re.toc_regions()[0].last, 4);
    }

    #[test]
    fn outline_levels_filter_and_style_cascade() {
        let mut doc = five_heading_doc();
        /* A custom style whose cascade carries outline level 0 (level 1). */
        doc.styles.insert(
            "MyHead".into(),
            ParagraphStyle {
                id: "MyHead".into(),
                name: "My Head".into(),
                para: ParaProperties {
                    outline_level: Some(0),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        doc.blocks.push_back(Block::Paragraph(Paragraph {
            text: "Styled".into(),
            style_id: Some("MyHead".into()),
            ..Default::default()
        }));
        let sw = TocSwitches {
            outline_levels: Some((1, 2)),
            ..Default::default()
        };
        let got: Vec<(String, u8)> = doc
            .toc_headings(&sw)
            .into_iter()
            .map(|h| (h.text, h.level))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Introduction".to_string(), 1),
                ("Background".to_string(), 2),
                ("Method".to_string(), 1),
                ("Results".to_string(), 2),
                ("Styled".to_string(), 1),
            ]
        );
        /* `\u` picks up a body paragraph's DIRECT outline level. */
        let mut p = body("Direct");
        p.direct_overrides.outline_level = Some(1);
        doc.blocks.push_back(Block::Paragraph(p));
        assert!(
            doc.toc_headings(&sw)
                .iter()
                .any(|h| h.text == "Direct" && h.level == 2)
        );
        let no_u = TocSwitches {
            use_outline_levels: false,
            ..sw.clone()
        };
        assert!(!doc.toc_headings(&no_u).iter().any(|h| h.text == "Direct"));
        /* `\t` custom styles. */
        let t_only = TocSwitches {
            outline_levels: None,
            use_outline_levels: false,
            custom_styles: vec![("My Head".into(), 3)],
            ..Default::default()
        };
        let got = doc.toc_headings(&t_only);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].text.as_str(), got[0].level), ("Styled", 3));
    }

    #[test]
    fn regions_survive_structural_edits_and_orphans_degrade_safely() {
        let (out, _) = five_heading_doc().regenerate_tocs(&|_| Some("1".into()));
        /* Enter in the middle of the TOC keeps the region intact. */
        let split = out.split_paragraph(LogicalPos::new(BlockPath::top(2), 3));
        let r = &split.toc_regions()[0];
        assert_eq!((r.first, r.last, r.closed), (0, 5, true));
        /* Enter inside the FIRST entry: the Head stays on the left half. */
        let split = out.split_paragraph(LogicalPos::new(BlockPath::top(0), 4));
        let r = &split.toc_regions()[0];
        assert_eq!((r.first, r.last, r.closed), (0, 5, true));
        /* Regeneration collapses the stray paragraph back into 5 entries. */
        let (re, _) = split.regenerate_tocs(&|_| Some("1".into()));
        assert_eq!(re.toc_regions()[0].last, 4);
        assert_eq!(re.blocks.len(), out.blocks.len());
        /* Deleting the Tail orphans the Head: the region is open and
        regeneration leaves it alone — it can no longer tell the stale
        result from user text, and must never drop content. */
        let mut orphan = out.clone();
        let mut last = orphan
            .paragraph_at_path(&BlockPath::top(4))
            .unwrap()
            .clone();
        last.fields.retain(|f| f.span != Some(FieldSpan::Tail));
        orphan.blocks.set(4, Block::Paragraph(last));
        let r = &orphan.toc_regions()[0];
        assert_eq!((r.first, r.last, r.closed), (0, 0, false));
        let (re, changed) = orphan.regenerate_tocs(&|_| Some("9".into()));
        assert!(!changed);
        assert_eq!(re.blocks.len(), out.blocks.len());
        /* Selecting from mid-first-entry into the body and deleting
        merges an orphan Head into body text — which survives F9. */
        let merged = out.delete_range(
            LogicalPos::new(BlockPath::top(0), 4),
            LogicalPos::new(BlockPath::top(5), 2),
        );
        let (re, _) = merged.regenerate_tocs(&|_| Some("9".into()));
        assert_eq!(
            re.paragraph_at_path(&BlockPath::top(0)).unwrap().text,
            "Introduction"[..4].to_string() + &"Introduction"[2..]
        );
    }

    #[test]
    fn prefix_and_suffix_around_the_field_survive_regeneration() {
        let mut doc = DocumentTree::from_text("");
        doc.blocks = vec![
            Block::Paragraph(Paragraph {
                text: "Lead: old".into(),
                fields: vec![Field {
                    start: 6,
                    end: 9,
                    instruction: "TOC \\o \"1-1\"".into(),
                    span: Some(FieldSpan::Head),
                }],
                ..Default::default()
            }),
            Block::Paragraph(Paragraph {
                text: "old2 trailing".into(),
                fields: vec![Field {
                    start: 0,
                    end: 4,
                    instruction: String::new(),
                    span: Some(FieldSpan::Tail),
                }],
                ..Default::default()
            }),
            Block::Paragraph(heading("Only", 1)),
        ]
        .into_iter()
        .collect();
        let (out, changed) = doc.regenerate_tocs(&|_| Some("2".into()));
        assert!(changed);
        let p = out.paragraph_at_path(&BlockPath::top(0)).unwrap();
        assert_eq!(p.text, "Lead: Only\t2 trailing");
        let head = p
            .fields
            .iter()
            .find(|f| f.span == Some(FieldSpan::Head))
            .unwrap();
        assert_eq!(head.start, 6);
        let tail = p
            .fields
            .iter()
            .find(|f| f.span == Some(FieldSpan::Tail))
            .unwrap();
        assert_eq!(tail.end, "Lead: Only\t2".len() as u32);
        assert_eq!(out.blocks.len(), 2);
    }

    #[test]
    fn empty_toc_says_no_entries_and_insert_splits_mid_paragraph() {
        let doc = DocumentTree::from_text("abcdef");
        let (d, at) = doc
            .insert_toc_at(
                &LogicalPos::new(BlockPath::top(0), 3),
                &TocSwitches::default(),
            )
            .unwrap();
        assert_eq!(at, 1);
        assert_eq!(d.blocks.len(), 3);
        let (out, _) = d.regenerate_tocs(&|_| None);
        assert_eq!(
            out.paragraph_at_path(&BlockPath::top(1)).unwrap().text,
            NO_ENTRIES_TEXT
        );
        assert_eq!(
            out.paragraph_at_path(&BlockPath::top(0)).unwrap().text,
            "abc"
        );
        assert_eq!(
            out.paragraph_at_path(&BlockPath::top(2)).unwrap().text,
            "def"
        );
        /* A caret inside an existing TOC is refused. */
        assert!(
            out.insert_toc_at(
                &LogicalPos::new(BlockPath::top(1), 0),
                &TocSwitches::default()
            )
            .is_none()
        );
    }

    /// Issue #152 — inserting (and regenerating, growing and shrinking)
    /// a TOC above a commented paragraph keeps the comment on it.
    #[test]
    fn toc_insertion_and_regeneration_keep_comments_on_their_paragraph() {
        let mut doc = DocumentTree::from_text("");
        doc.blocks = vec![
            heading("Alpha", 1),
            body("commented body"),
            heading("Beta", 1),
        ]
        .into_iter()
        .map(Block::Paragraph)
        .collect();
        let (doc, _) = doc.insert_comment(
            LogicalPos::new(BlockPath::top(1), 0),
            LogicalPos::new(BlockPath::top(1), 9),
            "c".into(),
            "A".into(),
            "d".into(),
        );
        let covered = |d: &DocumentTree| {
            let r = &d.comment_ranges[0];
            d.text_range(r.start.clone(), r.end.clone())
        };
        assert_eq!(covered(&doc), "commented");
        let (stub, at) = doc
            .insert_toc_at(
                &LogicalPos::new(BlockPath::top(0), 0),
                &TocSwitches::default(),
            )
            .unwrap();
        assert_eq!(at, 0);
        assert_eq!(covered(&stub), "commented");
        /* Stub (1 block) → heading + two entries: the result grows. */
        let (grown, changed) = stub.regenerate_tocs(&|_| Some("1".into()));
        assert!(changed);
        assert!(grown.blocks.len() > stub.blocks.len());
        assert_eq!(covered(&grown), "commented");
        /* Drop a heading and regenerate: the result shrinks. */
        let last = grown.blocks.len() - 1;
        let mut fewer = grown.clone();
        fewer.blocks.remove(last);
        let (shrunk, changed) = fewer.regenerate_tocs(&|_| Some("1".into()));
        assert!(changed);
        assert!(shrunk.blocks.len() < fewer.blocks.len());
        assert_eq!(covered(&shrunk), "commented");
        /* Mid-paragraph insertion splits the caret paragraph first. */
        let (split, at) = doc
            .insert_toc_at(
                &LogicalPos::new(BlockPath::top(0), 2),
                &TocSwitches::default(),
            )
            .unwrap();
        assert_eq!(at, 1);
        assert_eq!(covered(&split), "commented");
    }

    #[test]
    fn toc_head_is_addressable_but_not_atomic() {
        let (out, _) = five_heading_doc().regenerate_tocs(&|_| Some("1".into()));
        let p = out.paragraph_at_path(&BlockPath::top(0)).unwrap();
        /* The caret may sit anywhere in the entry text … */
        assert!(p.field_strictly_containing(3).is_none());
        assert_eq!(p.snap_offset_out_of_fields(3, true), 3);
        /* … but addresses the TOC for Update TOC / edit switches. */
        let idx = p.field_index_at(3).unwrap();
        assert_eq!(p.fields[idx].keyword(), "TOC");
        assert!(matches!(p.fields[idx].typed(), TypedField::Toc { .. }));
        /* Code view shows the instruction over the first entry. */
        let cv = p.with_field_codes();
        assert!(cv.text.starts_with("{ TOC \\o \"1-3\" \\h \\z \\u }"));
        /* The F9 string restamp never touches span overlays. */
        let r = out.restamp_fields(&mut |site| (site.field.keyword() == "TOC").then(|| "X".into()));
        assert_eq!(
            r.paragraph_at_path(&BlockPath::top(0)).unwrap().text,
            p.text
        );
    }
}
