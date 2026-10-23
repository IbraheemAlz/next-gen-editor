// Modified from WordCraft (https://github.com/storytold/wordcraft) at commit
// 3a4770ef8ce037d2650c93e2d5764536549357c7, file crates/docbin/src/lib.rs
// (the `walk` over the main document and its paragraph / run assembly).
// Copyright (c) 2026 ArtCraft Team and the WordCraft contributors.
// Licensed under the MIT License or the Apache License, Version 2.0, at your
// option. Changes: walks the pieces in CP order (no per-character search);
// one FKP run's character formatting is cached; the piece Prm applies to the
// paragraph mark's properties too (2.4.6.2); fields keep their cached result
// as plain text; special characters map onto this engine's model through the
// `engine::text_input` rules (line break U+2028, page break U+000C, nothing
// XML 1.0 cannot carry); inline objects, notes, tables and lists are counted
// for the import report instead of modelled; bounded by a character budget
// and a paragraph cap. See THIRD-PARTY-NOTICES.md at the repository root.

//! The main-document walk: characters of CPs `[0, ccpText)` in order,
//! split into paragraphs at their marks, each character carrying its direct
//! formatting ([MS-DOC] 2.4.1 Retrieving Text, 2.4.2 Determining Paragraph
//! Boundaries, 2.4.6.1 Direct Character Formatting, 2.4.6.2 Direct
//! Paragraph Formatting).

use engine::text_input::{LINE_BREAK, PAGE_BREAK, REPLACEMENT};
use engine::{Paragraph, StyleRun};

use crate::error::DocError;
use crate::fkp::Bins;
use crate::piece::{Piece, PieceTable, PrmRef, char_in_piece};
use crate::props::{CharProps, ParaProps};
use crate::sprm::{Prl, SGC_CHARACTER, SGC_PARAGRAPH, prls, sprm_of_isprm};

/// Most characters materialised from one document, whatever its stream
/// claims: well above any real document (Word's own text limit for a .doc
/// is far lower in practice), small enough for the worker's memory budget.
pub const MAX_TEXT_CHARS: u64 = 32 << 20;
/// Most paragraphs one import creates (each one costs a model paragraph;
/// a 500-page document has about ten thousand).
pub const MAX_PARAGRAPHS: usize = 200_000;
/// Fields nest at most this deep before the extra levels are only counted.
const MAX_FIELD_DEPTH: usize = 64;

/// Everything the walk reads.
pub(crate) struct Walk<'a> {
    pub(crate) word: &'a [u8],
    pub(crate) pieces: &'a PieceTable,
    pub(crate) chpx: &'a Bins,
    pub(crate) papx: &'a Bins,
    /// The CPs at which sections end, ascending: the section mark (0x0C)
    /// just before each is a paragraph mark, not a page break.
    pub(crate) sect_ends: &'a [u32],
}

/// What the walk could not model, counted for the import report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Skipped {
    pub(crate) table_paragraphs: u32,
    pub(crate) list_paragraphs: u32,
    pub(crate) objects: u32,
    pub(crate) fields: u32,
}

/// A paragraph and the CP just past its mark.
pub(crate) struct ParaOut {
    pub(crate) paragraph: Paragraph,
    pub(crate) end_cp: u32,
}

/// One paragraph being assembled.
#[derive(Default)]
struct ParaBuild {
    text: String,
    /// `(start, end, props)` byte ranges in `text`, in order, adjacent
    /// equal ones merged.
    runs: Vec<(u32, u32, CharProps)>,
}

impl ParaBuild {
    fn push(&mut self, c: char, props: &CharProps) {
        let start = self.text.len() as u32;
        self.text.push(c);
        let end = self.text.len() as u32;
        match self.runs.last_mut() {
            Some((_, e, p)) if *e == start && p == props => *e = end,
            _ => self.runs.push((start, end, props.clone())),
        }
    }

    fn finish(self, props: engine::ParaProperties) -> Paragraph {
        let mut spans: Vec<StyleRun> = Vec::new();
        for (start, end, p) in self.runs {
            let style = p.into_span_style();
            if style == engine::SpanStyle::default() {
                continue;
            }
            match spans.last_mut() {
                Some(last) if last.end == start && last.style == style => last.end = end,
                _ => spans.push(StyleRun { start, end, style }),
            }
        }
        Paragraph {
            text: self.text,
            spans,
            direct_overrides: props.clone(),
            props,
            ..Paragraph::default()
        }
    }
}

/// Apply a piece's `Prm` sprms of group `sgc` through `f`.
fn for_prm_sprms(prm: PrmRef<'_>, sgc: u8, mut f: impl FnMut(&Prl)) {
    match prm {
        PrmRef::None => {}
        PrmRef::Short { isprm, val } => {
            if let Some(op) = sprm_of_isprm(isprm) {
                let operand = [val];
                let p = Prl {
                    op,
                    operand: &operand,
                };
                if p.sgc() == sgc {
                    f(&p);
                }
            }
        }
        PrmRef::Grpprl(g) => prls(g).filter(|p| p.sgc() == sgc).for_each(|p| f(&p)),
    }
}

/// The character formatting of one FKP run inside one piece, cached while
/// consecutive characters stay inside it.
struct CharCache {
    piece: usize,
    fc_start: u32,
    fc_end: u32,
    props: CharProps,
}

impl Walk<'_> {
    /// The direct character formatting at byte `fc` of piece `pi`: the
    /// FKP's Chpx, then the piece's Prm (2.4.6.1).
    fn char_props(
        &self,
        cache: &mut Option<CharCache>,
        pi: usize,
        piece: &Piece,
        fc: u32,
    ) -> CharProps {
        if let Some(c) = cache
            && c.piece == pi
            && (c.fc_start..c.fc_end).contains(&fc)
        {
            return c.props.clone();
        }
        let mut props = CharProps::default();
        let run = self.chpx.lookup(self.word, fc);
        if let Some(run) = run {
            for p in prls(run.grpprl).filter(|p| p.sgc() == SGC_CHARACTER) {
                props.apply(&p);
            }
        }
        for_prm_sprms(self.pieces.prm(piece), SGC_CHARACTER, |p| props.apply(p));
        let (fc_start, fc_end) = run.map_or((fc, fc + 1), |r| (r.fc_start, r.fc_end));
        *cache = Some(CharCache {
            piece: pi,
            fc_start,
            fc_end,
            props: props.clone(),
        });
        props
    }

    /// The direct paragraph formatting of the paragraph whose mark is the
    /// byte at `fc_mark` in `piece`: the FKP's PapxInFkp, then the piece's
    /// Prm (2.4.6.2).
    fn para_props(&self, piece: &Piece, fc_mark: u32) -> ParaProps {
        let mut pp = ParaProps::default();
        if let Some(run) = self.papx.lookup(self.word, fc_mark) {
            for p in prls(run.grpprl).filter(|p| p.sgc() == SGC_PARAGRAPH) {
                pp.apply(&p);
            }
        }
        for_prm_sprms(self.pieces.prm(piece), SGC_PARAGRAPH, |p| pp.apply(p));
        pp.finish()
    }

    /// Walk CPs `[0, ccp_text)`.
    pub(crate) fn run(&self, ccp_text: u32) -> Result<(Vec<ParaOut>, Skipped), DocError> {
        let budget_max = (self.word.len() as u64).min(MAX_TEXT_CHARS);
        let mut budget = budget_max;
        let mut out: Vec<ParaOut> = Vec::new();
        let mut skipped = Skipped::default();
        let mut pb = ParaBuild::default();
        let mut cache: Option<CharCache> = None;
        /* One entry per open field: `true` once its separator was seen
        (the cached result shows), `false` in its instruction part. */
        let mut fields: Vec<bool> = Vec::new();
        let mut overflow_depth = 0usize;
        let mut cp_reached = 0u32;
        for (pi, piece) in self.pieces.pieces().iter().enumerate() {
            if piece.cp_start >= ccp_text {
                break;
            }
            let end = piece.cp_end.min(ccp_text);
            let mut cp = piece.cp_start;
            while cp < end {
                /* A piece whose bytes run past the stream end ends here:
                the rest of it is not in the file. */
                let Some((c, units)) = char_in_piece(self.word, piece, cp - piece.cp_start) else {
                    break;
                };
                if budget == 0 {
                    return Err(DocError::TooLarge {
                        what: "the document text (characters, bounded by the WordDocument stream)",
                        max: budget_max,
                    });
                }
                budget -= 1;
                let fc = piece.fc_of(cp).unwrap_or(0);
                let section_mark =
                    c == '\u{C}' && self.sect_ends.binary_search(&cp.saturating_add(1)).is_ok();
                if c == '\r' || c == '\u{7}' || section_mark {
                    /* 0x0D ends a paragraph, 0x07 a table cell or row
                    (tables are not modelled: the cell's text becomes a
                    paragraph), a section mark the section's last
                    paragraph. */
                    let pp = self.para_props(piece, fc);
                    let is_row_end = pp.row_end && pb.text.is_empty();
                    if pp.in_table || c == '\u{7}' {
                        skipped.table_paragraphs = skipped.table_paragraphs.saturating_add(1);
                    }
                    if pp.in_list {
                        skipped.list_paragraphs = skipped.list_paragraphs.saturating_add(1);
                    }
                    /* A field still in its instruction at the paragraph end
                    is closed here (the .docx reader's #350 rule): a stray
                    begin character must not hide every later paragraph. */
                    if let Some(i) = fields.iter().position(|shown| !shown) {
                        fields.truncate(i);
                        overflow_depth = 0;
                    }
                    let built = std::mem::take(&mut pb);
                    /* A table row's end mark holds no text: it is the row,
                    not a paragraph. */
                    if !is_row_end {
                        if out.len() >= MAX_PARAGRAPHS {
                            return Err(DocError::TooLarge {
                                what: "the number of paragraphs",
                                max: MAX_PARAGRAPHS as u64,
                            });
                        }
                        out.push(ParaOut {
                            paragraph: built.finish(pp.props),
                            end_cp: cp + 1,
                        });
                    }
                    cp += units;
                    cp_reached = cp;
                    continue;
                }
                /* Fields (2.8.25 Plcfld): 0x13 begin, 0x14 separator, 0x15
                end. The instruction is hidden, the cached result kept as
                plain text. */
                match c {
                    '\u{13}' => {
                        skipped.fields = skipped.fields.saturating_add(1);
                        if fields.len() < MAX_FIELD_DEPTH {
                            fields.push(false);
                        } else {
                            overflow_depth += 1;
                        }
                    }
                    '\u{14}' => {
                        if overflow_depth == 0
                            && let Some(f) = fields.last_mut()
                        {
                            *f = true;
                        }
                    }
                    '\u{15}' => {
                        if overflow_depth > 0 {
                            overflow_depth -= 1;
                        } else {
                            fields.pop();
                        }
                    }
                    _ if overflow_depth > 0 || fields.iter().any(|shown| !shown) => {}
                    /* Pictures (0x01) and drawn objects (0x08) are not
                    imported; note (0x02) and comment (0x05) references
                    belong to stories that are not imported either. */
                    '\u{1}' | '\u{8}' => skipped.objects = skipped.objects.saturating_add(1),
                    c => {
                        let mapped = match c {
                            '\t' => Some('\t'),
                            '\u{B}' | '\n' => Some(LINE_BREAK),
                            '\u{C}' => Some(PAGE_BREAK),
                            '\u{1E}' => Some('\u{2011}'),
                            '\u{1F}' => Some('\u{AD}'),
                            '\u{FFFE}' | '\u{FFFF}' => Some(REPLACEMENT),
                            c if (c as u32) < 0x20 => None,
                            c => Some(c),
                        };
                        if let Some(m) = mapped {
                            let props = self.char_props(&mut cache, pi, piece, fc);
                            pb.push(m, &props);
                        }
                    }
                }
                cp += units;
                cp_reached = cp;
            }
        }
        /* Text after the last mark (a damaged document: the main text
        always ends with one) still becomes a paragraph. */
        if !pb.text.is_empty() {
            out.push(ParaOut {
                paragraph: pb.finish(engine::ParaProperties::default()),
                end_cp: cp_reached,
            });
        }
        Ok((out, skipped))
    }
}
