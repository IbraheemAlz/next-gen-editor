//! Field state machine — issue #77 (field authoring v2).
//!
//! A complex field is a *sentinel triple* in the run stream (OOXML
//! `<w:fldChar>` begin / separate / end) whose instruction text lives
//! between `begin` and `separate` and whose cached **result** lives
//! between `separate` and `end`. The engine models that as
//! [`Field`] — an overlay over the result's byte range in
//! [`Paragraph::text`] plus the verbatim instruction string.
//!
//! This module is the single home for everything that *interprets* a
//! field, so the TOC epic (#81) can build on it without re-deriving
//! the plumbing:
//!
//! 1. **Instruction parsing** — [`FieldInstruction::parse`] tokenizes
//!    the code (`PAGE \* MERGEFORMAT`, `DATE \@ "d MMMM yyyy"`,
//!    `TOC \o "1-3" \h`) into keyword + positional args + `\x` switches,
//!    quote-aware. [`Field::typed`] dispatches the keyword into a
//!    [`TypedField`].
//! 2. **Environment-driven evaluation** — [`Field::evaluate_in`] turns
//!    a field + a [`FieldEnv`] (page context, render date/clock,
//!    document name, author) into the live result string, or `None`
//!    when the environment cannot resolve it (the cached result then
//!    stands). Page-dependent kinds resolve inside the paginator (which
//!    fills the page context); everything else resolves anywhere.
//! 3. **Field-code view** — [`Field::code_text`] renders the
//!    `{ INSTRUCTION }` form Alt+F9 displays; [`Paragraph::with_field_codes`]
//!    derives a display paragraph, and the two `*_offset_*` helpers map
//!    caret offsets between the derived (code) text and the source
//!    (result) text so navigation stays exact across the toggle.
//! 4. **Atomic ranges** — a field is one caret step: the snapping
//!    helpers on [`Paragraph`] push any offset strictly inside a result
//!    out to its boundary and widen ranges to whole fields.
//! 5. **Update pass** — [`DocumentTree::restamp_fields`] is the
//!    positional model-level update: every field site (body, header
//!    part, footer part) is visited in document order and its result
//!    replaced between the sentinels when the resolver returns a new
//!    value. F9 and the save-time restamp both run through it; a TOC
//!    update is the same pass with a resolver that regenerates the
//!    entry text.

use crate::{Block, BlockPath, DocumentTree, Field, LogicalPos, Paragraph, PathStep};
use im::Vector;

/* ================================================================
1. Instruction parsing
================================================================ */

/// One `\x` switch of a field instruction. `name` is the switch
/// character(s) as written (`*`, `@`, `#`, `o`, `h`, `p`, …); `arg` is
/// the token that follows it when that token is not itself a switch
/// (quotes stripped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSwitch {
    pub name: String,
    pub arg: Option<String>,
}

/// A tokenized field instruction.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldInstruction {
    /// Leading keyword, upper-cased (`PAGE`, `DATE`, `TOC`, …). A
    /// formula field (`= SUM(ABOVE)`) reports `=`.
    pub keyword: String,
    /// Positional (non-switch) arguments after the keyword, quotes
    /// stripped — `REF bookmark`, `MERGEFIELD Name`.
    pub args: Vec<String>,
    /// Switches in source order.
    pub switches: Vec<FieldSwitch>,
}

impl FieldInstruction {
    /// Tokenize `text`. Whitespace separates tokens; a double-quoted
    /// run is one token with the quotes stripped; a token starting
    /// with `\` opens a switch that absorbs the following token as its
    /// argument unless that token is itself a switch. A leading `=`
    /// glued to the first token (`=SUM(ABOVE)`) yields keyword `=`.
    pub fn parse(text: &str) -> Self {
        let tokens = tokenize(text);
        let mut out = FieldInstruction::default();
        let mut iter = tokens.into_iter().peekable();
        match iter.next() {
            Some(Token::Word(w)) if w.starts_with('=') => {
                out.keyword = "=".to_string();
                let rest = w[1..].to_string();
                if !rest.is_empty() {
                    out.args.push(rest);
                }
            }
            Some(Token::Word(w)) => out.keyword = w.to_ascii_uppercase(),
            Some(Token::Switch(s)) => {
                /* Instruction that starts with a switch — no keyword.
                Keep the switch so the code view round-trips it. */
                out.switches.push(FieldSwitch { name: s, arg: None });
            }
            None => {}
        }
        while let Some(tok) = iter.next() {
            match tok {
                Token::Word(w) => out.args.push(w),
                Token::Switch(name) => {
                    let arg = match iter.peek() {
                        Some(Token::Word(_)) => match iter.next() {
                            Some(Token::Word(w)) => Some(w),
                            _ => None,
                        },
                        _ => None,
                    };
                    out.switches.push(FieldSwitch { name, arg });
                }
            }
        }
        out
    }

    /// First switch named `name` (`"*"`, `"@"`, `"o"`, …).
    pub fn switch(&self, name: &str) -> Option<&FieldSwitch> {
        self.switches.iter().find(|s| s.name == name)
    }

    pub fn has_switch(&self, name: &str) -> bool {
        self.switch(name).is_some()
    }

    /// Argument of the first switch named `name`, if present.
    pub fn switch_arg(&self, name: &str) -> Option<&str> {
        self.switch(name).and_then(|s| s.arg.as_deref())
    }
}

enum Token {
    Word(String),
    Switch(String),
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '"' {
            chars.next();
            let mut s = String::new();
            for ch in chars.by_ref() {
                if ch == '"' {
                    break;
                }
                s.push(ch);
            }
            out.push(Token::Word(s));
            continue;
        }
        if c == '\\' {
            chars.next();
            let mut name = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_whitespace() || ch == '"' {
                    break;
                }
                name.push(ch);
                chars.next();
            }
            out.push(Token::Switch(name));
            continue;
        }
        let mut w = String::new();
        while let Some(&ch) = chars.peek() {
            if ch.is_whitespace() || ch == '"' {
                break;
            }
            w.push(ch);
            chars.next();
        }
        out.push(Token::Word(w));
    }
    out
}

/// Typed dispatch of the instruction keyword. Kinds the engine does not
/// operate on (REF, TOC, formulas, …) map to [`TypedField::Other`] with
/// the keyword preserved so the code view and the writer round-trip
/// them verbatim; #81 adds `Toc` here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypedField {
    Page,
    NumPages,
    Date { picture: Option<String> },
    Time { picture: Option<String> },
    FileName { with_path: bool },
    Author,
    Other { keyword: String },
}

impl Field {
    /// Parse the instruction (see [`FieldInstruction::parse`]).
    pub fn instruction_parsed(&self) -> FieldInstruction {
        FieldInstruction::parse(&self.instruction)
    }

    /// Dispatch the keyword into a [`TypedField`].
    pub fn typed(&self) -> TypedField {
        let ins = self.instruction_parsed();
        match ins.keyword.as_str() {
            "PAGE" => TypedField::Page,
            "NUMPAGES" => TypedField::NumPages,
            "DATE" => TypedField::Date {
                picture: ins.switch_arg("@").map(str::to_string),
            },
            "TIME" => TypedField::Time {
                picture: ins.switch_arg("@").map(str::to_string),
            },
            "FILENAME" => TypedField::FileName {
                with_path: ins.has_switch("p"),
            },
            "AUTHOR" => TypedField::Author,
            other => TypedField::Other {
                keyword: other.to_string(),
            },
        }
    }

    /// Alt+F9 display form: `{ PAGE \* MERGEFORMAT }`.
    pub fn code_text(&self) -> String {
        format!("{{ {} }}", self.instruction)
    }

    /// Live result for `env`, or `None` when the environment cannot
    /// resolve this kind (the cached result stands). This is THE
    /// evaluator — the paginator, F9 and the save restamp all route
    /// through it.
    pub fn evaluate_in(&self, env: &FieldEnv) -> Option<String> {
        match self.typed() {
            TypedField::Page => env.page.as_ref().and_then(|p| p.current.clone()),
            TypedField::NumPages => env.page.as_ref().and_then(|p| p.total).map(|t| t.to_string()),
            TypedField::Date { picture } => env.date.map(|_| {
                render_date_time_picture(
                    picture.as_deref().unwrap_or("M/d/yyyy"),
                    env.date,
                    env.clock,
                )
            }),
            TypedField::Time { picture } => env.clock.map(|_| {
                render_date_time_picture(
                    picture.as_deref().unwrap_or("h:mm am/pm"),
                    env.date,
                    env.clock,
                )
            }),
            TypedField::FileName { .. } => env.document_name.clone(),
            TypedField::Author => env.author.clone(),
            TypedField::Other { .. } => None,
        }
    }
}

/* ================================================================
2. Evaluation environment
================================================================ */

/// Page context the paginator supplies while flushing a page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PageContext {
    /// The page's FORMATTED number (`pgNumType` applied) — `None`
    /// outside a page flush.
    pub current: Option<String>,
    /// Total page count — only known after the whole document has
    /// paginated (`Paginator::finish`).
    pub total: Option<u32>,
}

/// Everything a field evaluation can depend on. Fully owned so it can
/// be stored on a paginator / engine and cloned into a page flush.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldEnv {
    pub page: Option<PageContext>,
    /// Render date `(year, month, day)` — shell-injected; the engine
    /// core never reads a wall clock.
    pub date: Option<(i32, u32, u32)>,
    /// Render clock `(hour 0–23, minute)`.
    pub clock: Option<(u32, u32)>,
    /// Document name as opened / to be saved (`FILENAME`).
    pub document_name: Option<String>,
    /// `docProps/core.xml` `dc:creator` (`AUTHOR`).
    pub author: Option<String>,
}

impl FieldEnv {
    /// Copy of `self` with the page context replaced.
    pub fn with_page(&self, current: Option<String>, total: Option<u32>) -> FieldEnv {
        let mut env = self.clone();
        env.page = Some(PageContext { current, total });
        env
    }
}

/// Render a date/time through Word's picture language. Date tokens
/// (`yyyy`, `yy`, `MM`, `M`, `dd`, `d`) need `date`; time tokens
/// (`HH`, `H`, `hh`, `h`, `mm`, `m`, `am/pm`, `AM/PM`) need `clock`.
/// A token whose input is absent passes through verbatim, as does any
/// unrecognized character. Longest match, case-sensitive per Word
/// (`M` = month, `m` = minute; `H` = 24-hour, `h` = 12-hour).
pub fn render_date_time_picture(
    picture: &str,
    date: Option<(i32, u32, u32)>,
    clock: Option<(u32, u32)>,
) -> String {
    let mut out = String::with_capacity(picture.len() + 8);
    let mut i = 0;
    let bytes = picture.as_bytes();
    let h12 = |h: u32| -> u32 {
        let r = h % 12;
        if r == 0 { 12 } else { r }
    };
    while i < bytes.len() {
        let rest = &picture[i..];
        macro_rules! emit {
            ($tok:expr, $val:expr) => {{
                if rest.starts_with($tok) {
                    match $val {
                        Some(s) => out.push_str(&s),
                        None => out.push_str($tok),
                    }
                    i += $tok.len();
                    continue;
                }
            }};
        }
        emit!("yyyy", date.map(|(y, _, _)| format!("{y:04}")));
        emit!("yy", date.map(|(y, _, _)| format!("{:02}", y.rem_euclid(100))));
        emit!("MM", date.map(|(_, m, _)| format!("{m:02}")));
        emit!("M", date.map(|(_, m, _)| m.to_string()));
        emit!("dd", date.map(|(_, _, d)| format!("{d:02}")));
        emit!("d", date.map(|(_, _, d)| d.to_string()));
        emit!("HH", clock.map(|(h, _)| format!("{h:02}")));
        emit!("H", clock.map(|(h, _)| h.to_string()));
        emit!("hh", clock.map(|(h, _)| format!("{:02}", h12(h))));
        emit!("h", clock.map(|(h, _)| h12(h).to_string()));
        emit!("mm", clock.map(|(_, m)| format!("{m:02}")));
        emit!("m", clock.map(|(_, m)| m.to_string()));
        emit!(
            "am/pm",
            clock.map(|(h, _)| if h < 12 { "am".to_string() } else { "pm".to_string() })
        );
        emit!(
            "AM/PM",
            clock.map(|(h, _)| if h < 12 { "AM".to_string() } else { "PM".to_string() })
        );
        let ch = rest.chars().next().expect("non-empty rest");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/* ================================================================
3. Field-code view + 4. atomic ranges — paragraph helpers
================================================================ */

impl Paragraph {
    /// Field whose result range strictly contains `off`
    /// (`start < off < end`).
    pub fn field_strictly_containing(&self, off: u32) -> Option<&Field> {
        self.fields.iter().find(|f| f.start < off && off < f.end)
    }

    /// Index of the field a caret at `off` addresses: strictly inside
    /// wins, then the field ending exactly at `off` (caret just after
    /// it), then the field starting at `off`.
    pub fn field_index_at(&self, off: u32) -> Option<usize> {
        self.fields
            .iter()
            .position(|f| f.start < off && off < f.end)
            .or_else(|| self.fields.iter().position(|f| f.end == off && f.start < f.end))
            .or_else(|| self.fields.iter().position(|f| f.start == off && f.start < f.end))
    }

    /// Field whose result range is exactly `[lo, hi)`.
    pub fn field_exactly(&self, lo: u32, hi: u32) -> Option<&Field> {
        self.fields.iter().find(|f| f.start == lo && f.end == hi)
    }

    /// Push an offset strictly inside a result out to the field's
    /// boundary — `forward` picks the end, otherwise the start.
    pub fn snap_offset_out_of_fields(&self, off: u32, forward: bool) -> u32 {
        match self.field_strictly_containing(off) {
            Some(f) if forward => f.end,
            Some(f) => f.start,
            None => off,
        }
    }

    /// Push an offset strictly inside a result out to the NEARER
    /// boundary (ties go to the start).
    pub fn snap_offset_nearest(&self, off: u32) -> u32 {
        match self.field_strictly_containing(off) {
            Some(f) if off - f.start > f.end - off => f.end,
            Some(f) => f.start,
            None => off,
        }
    }

    /// Widen `[lo, hi)` so it never partially covers a field.
    pub fn expand_range_over_fields(&self, lo: u32, hi: u32) -> (u32, u32) {
        let mut lo = lo;
        let mut hi = hi;
        for f in &self.fields {
            if f.start < lo && lo < f.end {
                lo = f.start;
            }
            if f.start < hi && hi < f.end {
                hi = f.end;
            }
        }
        (lo, hi)
    }

    /// Derived display paragraph for the field-code view: every result
    /// range replaced by [`Field::code_text`], with the overlays
    /// remapped by [`Paragraph::with_spliced_range`] (the field's own
    /// range covers the code text exactly).
    pub fn with_field_codes(&self) -> Paragraph {
        if self.fields.is_empty() {
            return self.clone();
        }
        let mut order: Vec<usize> = (0..self.fields.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(self.fields[i].start));
        let mut para = self.clone();
        for i in order {
            let f = &self.fields[i];
            let code = f.code_text();
            para = para.with_spliced_range(f.start, f.end, &code);
        }
        para
    }

    /// Replace the instruction of the `index`-th field. The cached
    /// result is untouched (Word keeps the stale result until the next
    /// update); the paragraph is dirtied so the writer re-emits the
    /// `<w:instrText>`.
    pub fn with_field_instruction(&self, index: usize, instruction: &str) -> Paragraph {
        let mut para = self.clone();
        if let Some(f) = para.fields.get_mut(index) {
            f.instruction = instruction.trim().to_string();
            para.dirty = true;
            para.source_xml = None;
        }
        para
    }

    /// Cumulative byte delta of the code view relative to the source
    /// text, walked in field order. Used by the two offset mappers.
    fn code_view_deltas(&self) -> Vec<(u32, u32, u32)> {
        /* (source_start, source_end, code_len) sorted by start. */
        let mut v: Vec<(u32, u32, u32)> = self
            .fields
            .iter()
            .filter(|f| f.start < f.end)
            .map(|f| (f.start, f.end, f.code_text().len() as u32))
            .collect();
        v.sort_by_key(|t| t.0);
        v
    }

    /// Map an offset in the SOURCE (result) text to the field-code
    /// view. Offsets strictly inside a result land on the code span's
    /// end (the atomic invariant makes the interior unreachable).
    pub fn source_offset_to_code_view(&self, off: u32) -> u32 {
        let mut acc: i64 = 0;
        for (s, e, c) in self.code_view_deltas() {
            if off <= s {
                break;
            }
            if off < e {
                return (s as i64 + acc + c as i64) as u32;
            }
            acc += c as i64 - (e - s) as i64;
        }
        (off as i64 + acc).max(0) as u32
    }

    /// Map an offset in the field-code view back to the source text.
    /// Offsets strictly inside a code span land on the field's end.
    pub fn code_view_offset_to_source(&self, off: u32) -> u32 {
        let mut acc: i64 = 0;
        for (s, e, c) in self.code_view_deltas() {
            let ds = s as i64 + acc;
            if (off as i64) <= ds {
                break;
            }
            if (off as i64) < ds + c as i64 {
                return e;
            }
            acc += c as i64 - (e - s) as i64;
        }
        (off as i64 - acc).max(0) as u32
    }
}

/* ================================================================
5. Update pass — document helpers
================================================================ */

/// Which story a field site lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldStory<'a> {
    Body,
    Header(&'a str),
    Footer(&'a str),
}

/// One field visited by [`DocumentTree::restamp_fields`].
#[derive(Debug, Clone)]
pub struct FieldSite<'a> {
    pub story: FieldStory<'a>,
    /// Story-rooted path of the owning paragraph (body paths for
    /// `Body`, part-block paths for header/footer parts).
    pub path: &'a BlockPath,
    /// Index into `paragraph.fields`.
    pub index: usize,
    pub field: &'a Field,
    pub paragraph: &'a Paragraph,
}

impl DocumentTree {
    /// `true` when any paragraph anywhere (body, cells, referenced or
    /// unreferenced parts) carries a field overlay.
    pub fn has_any_fields(&self) -> bool {
        let mut found = false;
        for_each_paragraph_deep(&self.blocks, &mut |_, p| found |= !p.fields.is_empty());
        for blocks in self.headers.values().chain(self.footers.values()) {
            for_each_paragraph_deep_slice(blocks, &mut |_, p| found |= !p.fields.is_empty());
        }
        found
    }

    /// Derived document for the field-code view — every paragraph in
    /// the body, in table cells and in every part swapped for
    /// [`Paragraph::with_field_codes`]. Layout + geometry run on this
    /// tree while Alt+F9 is on; it is never stored or saved.
    pub fn to_code_view(&self) -> DocumentTree {
        let mut next = self.clone();
        next.blocks = map_paragraphs_deep(&self.blocks, &|p| p.with_field_codes());
        for (_, blocks) in next.headers.iter_mut() {
            *blocks = map_paragraphs_deep_slice(blocks, &|p| p.with_field_codes());
        }
        for (_, blocks) in next.footers.iter_mut() {
            *blocks = map_paragraphs_deep_slice(blocks, &|p| p.with_field_codes());
        }
        next
    }

    /// Replace the instruction of the field a caret at `at` addresses
    /// (see [`Paragraph::field_index_at`]). `None` when `at` addresses
    /// no field or the instruction is blank.
    pub fn set_field_instruction(&self, at: LogicalPos, instruction: &str) -> Option<Self> {
        if instruction.trim().is_empty() {
            return None;
        }
        let para = self.paragraph_at_path(&at.path)?;
        let index = para.field_index_at(at.offset)?;
        let mut blocks = self.blocks.clone();
        crate::mutate_paragraph_in_top(&mut blocks, &at.path, |p| {
            *p = p.with_field_instruction(index, instruction);
        })?;
        let mut next = self.clone();
        next.blocks = blocks;
        Some(next)
    }

    /// The positional update pass. Visits every field site in document
    /// order (body first, then header parts, then footer parts — parts
    /// in rid order) and asks `resolve` for a new result; a `Some`
    /// that differs from the cached result is spliced between the
    /// sentinels (the paragraph is dirtied, the part marked for
    /// re-emission). Paragraphs whose every field resolves to its
    /// cached value are left untouched — byte-stable for the writer.
    pub fn restamp_fields(
        &self,
        resolve: &mut impl FnMut(FieldSite<'_>) -> Option<String>,
    ) -> DocumentTree {
        let mut next = self.clone();
        let body = restamp_blocks(&self.blocks, &mut |path, index, field, para| {
            resolve(FieldSite {
                story: FieldStory::Body,
                path,
                index,
                field,
                paragraph: para,
            })
        });
        if let Some(body) = body {
            next.blocks = body;
        }
        let mut header_rids: Vec<&String> = self.headers.keys().collect();
        header_rids.sort();
        for rid in header_rids {
            let blocks = &self.headers[rid];
            let src: Vector<Block> = blocks.iter().cloned().collect();
            let changed = restamp_blocks(&src, &mut |path, index, field, para| {
                resolve(FieldSite {
                    story: FieldStory::Header(rid),
                    path,
                    index,
                    field,
                    paragraph: para,
                })
            });
            if let Some(changed) = changed {
                next = next.with_updated_header_part(rid, changed.into_iter().collect());
            }
        }
        let mut footer_rids: Vec<&String> = self.footers.keys().collect();
        footer_rids.sort();
        for rid in footer_rids {
            let blocks = &self.footers[rid];
            let src: Vector<Block> = blocks.iter().cloned().collect();
            let changed = restamp_blocks(&src, &mut |path, index, field, para| {
                resolve(FieldSite {
                    story: FieldStory::Footer(rid),
                    path,
                    index,
                    field,
                    paragraph: para,
                })
            });
            if let Some(changed) = changed {
                next = next.with_updated_footer_part(rid, changed.into_iter().collect());
            }
        }
        next
    }

    /// Convenience: [`Self::restamp_fields`] with every non-page kind
    /// resolved against `env` and page kinds left alone.
    pub fn restamp_fields_with_env(&self, env: &FieldEnv) -> DocumentTree {
        let mut env = env.clone();
        env.page = None;
        self.restamp_fields(&mut |site| site.field.evaluate_in(&env))
    }
}

/// Restamp every paragraph in `blocks` (top level + one cell level, the
/// depth the overlay walks use). Returns `None` when nothing changed.
fn restamp_blocks(
    blocks: &Vector<Block>,
    resolve: &mut impl FnMut(&BlockPath, usize, &Field, &Paragraph) -> Option<String>,
) -> Option<Vector<Block>> {
    let mut changed_any = false;
    let mut out = blocks.clone();
    for (bi, block) in blocks.iter().enumerate() {
        match block {
            Block::Paragraph(p) => {
                let path = BlockPath::top(bi as u32);
                if let Some(np) = restamp_paragraph(p, &path, resolve) {
                    out.set(bi, Block::Paragraph(np));
                    changed_any = true;
                }
            }
            Block::Table(t) => {
                let mut table = t.clone();
                let mut table_changed = false;
                for (ri, row) in t.rows.iter().enumerate() {
                    for (ci, cell) in row.cells.iter().enumerate() {
                        for (pi, nested) in cell.blocks.iter().enumerate() {
                            let Block::Paragraph(p) = nested else {
                                continue;
                            };
                            let path = BlockPath {
                                steps: vec![
                                    PathStep::Block(bi as u32),
                                    PathStep::Cell {
                                        row: ri as u32,
                                        col: ci as u32,
                                    },
                                    PathStep::Block(pi as u32),
                                ],
                            };
                            if let Some(np) = restamp_paragraph(p, &path, resolve) {
                                table.rows[ri].cells[ci].blocks[pi] = Block::Paragraph(np);
                                table_changed = true;
                            }
                        }
                    }
                }
                if table_changed {
                    table.dirty = true;
                    table.source_xml = None;
                    out.set(bi, Block::Table(table));
                    changed_any = true;
                }
            }
        }
    }
    changed_any.then_some(out)
}

fn restamp_paragraph(
    p: &Paragraph,
    path: &BlockPath,
    resolve: &mut impl FnMut(&BlockPath, usize, &Field, &Paragraph) -> Option<String>,
) -> Option<Paragraph> {
    if p.fields.is_empty() {
        return None;
    }
    /* Resolve against the ORIGINAL paragraph (stable indices + ranges),
    then splice highest-offset first so earlier ranges stay valid. */
    let mut subs: Vec<(usize, String)> = Vec::new();
    for (i, f) in p.fields.iter().enumerate() {
        if f.start >= f.end {
            continue;
        }
        if let Some(v) = resolve(path, i, f, p)
            && p.text.get(f.start as usize..f.end as usize) != Some(v.as_str())
        {
            subs.push((i, v));
        }
    }
    if subs.is_empty() {
        return None;
    }
    subs.sort_by_key(|(i, _)| std::cmp::Reverse(p.fields[*i].start));
    let mut para = p.clone();
    for (i, v) in subs {
        let f = &p.fields[i];
        para = para.with_spliced_range(f.start, f.end, &v);
    }
    para.dirty = true;
    para.source_xml = None;
    Some(para)
}

/// Depth-first visit of every paragraph in `blocks` (top level + one
/// cell level), with its story-rooted path.
pub fn for_each_paragraph_deep(blocks: &Vector<Block>, f: &mut impl FnMut(BlockPath, &Paragraph)) {
    for (bi, block) in blocks.iter().enumerate() {
        visit_block(bi as u32, block, f);
    }
}

fn for_each_paragraph_deep_slice(blocks: &[Block], f: &mut impl FnMut(BlockPath, &Paragraph)) {
    for (bi, block) in blocks.iter().enumerate() {
        visit_block(bi as u32, block, f);
    }
}

fn visit_block(bi: u32, block: &Block, f: &mut impl FnMut(BlockPath, &Paragraph)) {
    match block {
        Block::Paragraph(p) => f(BlockPath::top(bi), p),
        Block::Table(t) => {
            for (ri, row) in t.rows.iter().enumerate() {
                for (ci, cell) in row.cells.iter().enumerate() {
                    for (pi, nested) in cell.blocks.iter().enumerate() {
                        if let Block::Paragraph(p) = nested {
                            f(
                                BlockPath {
                                    steps: vec![
                                        PathStep::Block(bi),
                                        PathStep::Cell {
                                            row: ri as u32,
                                            col: ci as u32,
                                        },
                                        PathStep::Block(pi as u32),
                                    ],
                                },
                                p,
                            );
                        }
                    }
                }
            }
        }
    }
}

fn map_paragraphs_deep(blocks: &Vector<Block>, f: &impl Fn(&Paragraph) -> Paragraph) -> Vector<Block> {
    blocks.iter().map(|b| map_block(b, f)).collect()
}

fn map_paragraphs_deep_slice(blocks: &[Block], f: &impl Fn(&Paragraph) -> Paragraph) -> Vec<Block> {
    blocks.iter().map(|b| map_block(b, f)).collect()
}

fn map_block(block: &Block, f: &impl Fn(&Paragraph) -> Paragraph) -> Block {
    match block {
        Block::Paragraph(p) => Block::Paragraph(f(p)),
        Block::Table(t) => {
            let mut table = t.clone();
            for row in table.rows.iter_mut() {
                for cell in row.cells.iter_mut() {
                    for nested in cell.blocks.iter_mut() {
                        if let Block::Paragraph(p) = nested {
                            *p = f(p);
                        }
                    }
                }
            }
            Block::Table(table)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(start: u32, end: u32, instr: &str) -> Field {
        Field {
            start,
            end,
            instruction: instr.into(),
        }
    }

    #[test]
    fn instruction_parser_tokenizes_keyword_args_and_switches() {
        let ins = FieldInstruction::parse(" PAGE \\* MERGEFORMAT ");
        assert_eq!(ins.keyword, "PAGE");
        assert!(ins.args.is_empty());
        assert_eq!(ins.switch_arg("*"), Some("MERGEFORMAT"));

        let ins = FieldInstruction::parse("DATE \\@ \"d MMMM yyyy\" \\* MERGEFORMAT");
        assert_eq!(ins.keyword, "DATE");
        assert_eq!(ins.switch_arg("@"), Some("d MMMM yyyy"));
        assert_eq!(ins.switch_arg("*"), Some("MERGEFORMAT"));

        let ins = FieldInstruction::parse("TOC \\o \"1-3\" \\h \\z \\u");
        assert_eq!(ins.keyword, "TOC");
        assert_eq!(ins.switch_arg("o"), Some("1-3"));
        assert!(ins.has_switch("h"));
        assert!(ins.has_switch("z"));
        assert_eq!(ins.switch("u").and_then(|s| s.arg.clone()), None);

        let ins = FieldInstruction::parse("REF my_bookmark \\h");
        assert_eq!(ins.args, vec!["my_bookmark".to_string()]);

        let ins = FieldInstruction::parse("=SUM(ABOVE) \\# \"0.00\"");
        assert_eq!(ins.keyword, "=");
        assert_eq!(ins.args, vec!["SUM(ABOVE)".to_string()]);
        assert_eq!(ins.switch_arg("#"), Some("0.00"));

        let ins = FieldInstruction::parse("filename \\p");
        assert_eq!(ins.keyword, "FILENAME");
        assert!(ins.has_switch("p"));
    }

    #[test]
    fn typed_dispatch_and_code_text() {
        assert_eq!(field(0, 1, "PAGE").typed(), TypedField::Page);
        assert_eq!(field(0, 1, "NUMPAGES \\* Arabic").typed(), TypedField::NumPages);
        assert_eq!(
            field(0, 1, "DATE \\@ \"yyyy\"").typed(),
            TypedField::Date {
                picture: Some("yyyy".into())
            }
        );
        assert_eq!(field(0, 1, "TIME").typed(), TypedField::Time { picture: None });
        assert_eq!(
            field(0, 1, "FILENAME \\p").typed(),
            TypedField::FileName { with_path: true }
        );
        assert_eq!(field(0, 1, "AUTHOR").typed(), TypedField::Author);
        assert_eq!(
            field(0, 1, "TOC \\o \"1-3\"").typed(),
            TypedField::Other {
                keyword: "TOC".into()
            }
        );
        assert_eq!(
            field(0, 1, "PAGE \\* MERGEFORMAT").code_text(),
            "{ PAGE \\* MERGEFORMAT }"
        );
    }

    #[test]
    fn evaluate_in_resolves_every_kind_from_the_environment() {
        let env = FieldEnv {
            page: Some(PageContext {
                current: Some("iv".into()),
                total: Some(12),
            }),
            date: Some((2026, 9, 1)),
            clock: Some((14, 5)),
            document_name: Some("report.docx".into()),
            author: Some("Ibrahim".into()),
        };
        assert_eq!(field(0, 1, "PAGE").evaluate_in(&env).as_deref(), Some("iv"));
        assert_eq!(field(0, 1, "NUMPAGES").evaluate_in(&env).as_deref(), Some("12"));
        assert_eq!(field(0, 1, "DATE").evaluate_in(&env).as_deref(), Some("9/1/2026"));
        assert_eq!(field(0, 1, "TIME").evaluate_in(&env).as_deref(), Some("2:05 pm"));
        assert_eq!(
            field(0, 1, "TIME \\@ \"HH:mm\"").evaluate_in(&env).as_deref(),
            Some("14:05")
        );
        assert_eq!(
            field(0, 1, "FILENAME").evaluate_in(&env).as_deref(),
            Some("report.docx")
        );
        assert_eq!(field(0, 1, "AUTHOR").evaluate_in(&env).as_deref(), Some("Ibrahim"));
        assert_eq!(field(0, 1, "REF x").evaluate_in(&env), None);

        /* Missing inputs leave the cached result standing. */
        let empty = FieldEnv::default();
        assert_eq!(field(0, 1, "PAGE").evaluate_in(&empty), None);
        assert_eq!(field(0, 1, "DATE").evaluate_in(&empty), None);
        assert_eq!(field(0, 1, "TIME").evaluate_in(&empty), None);
        assert_eq!(field(0, 1, "FILENAME").evaluate_in(&empty), None);
        assert_eq!(field(0, 1, "AUTHOR").evaluate_in(&empty), None);
    }

    #[test]
    fn date_time_picture_tokens() {
        let d = Some((2026, 7, 5));
        let c = Some((9, 7));
        assert_eq!(render_date_time_picture("h:mm am/pm", d, c), "9:07 am");
        assert_eq!(render_date_time_picture("hh:mm AM/PM", d, Some((0, 0))), "12:00 AM");
        assert_eq!(render_date_time_picture("H:m", d, Some((23, 9))), "23:9");
        assert_eq!(render_date_time_picture("d/M/yyyy H:mm", d, c), "5/7/2026 9:07");
        /* Time tokens without a clock pass through verbatim. */
        assert_eq!(render_date_time_picture("d/M/yyyy HH:mm", d, None), "5/7/2026 HH:mm");
        /* Date tokens without a date pass through verbatim. */
        assert_eq!(render_date_time_picture("yyyy h", None, c), "yyyy 9");
    }

    #[test]
    fn atomic_snapping_and_range_expansion() {
        let p = Paragraph {
            text: "Page 12 of 34".into(),
            fields: vec![field(5, 7, "PAGE"), field(11, 13, "NUMPAGES")],
            ..Default::default()
        };
        assert_eq!(p.field_strictly_containing(6).map(|f| f.start), Some(5));
        assert!(p.field_strictly_containing(5).is_none());
        assert!(p.field_strictly_containing(7).is_none());
        assert_eq!(p.snap_offset_out_of_fields(6, true), 7);
        assert_eq!(p.snap_offset_out_of_fields(6, false), 5);
        assert_eq!(p.snap_offset_out_of_fields(3, true), 3);
        assert_eq!(p.snap_offset_nearest(12), 11);
        assert_eq!(p.expand_range_over_fields(6, 12), (5, 13));
        assert_eq!(p.expand_range_over_fields(0, 5), (0, 5));
        assert_eq!(p.field_index_at(6), Some(0));
        assert_eq!(p.field_index_at(7), Some(0));
        assert_eq!(p.field_index_at(11), Some(1));
        assert_eq!(p.field_index_at(2), None);
        assert!(p.field_exactly(5, 7).is_some());
        assert!(p.field_exactly(5, 8).is_none());
    }

    #[test]
    fn code_view_derivation_and_offset_maps_are_inverse_outside_fields() {
        let p = Paragraph {
            text: "Page 12 of 34".into(),
            fields: vec![field(5, 7, "PAGE"), field(11, 13, "NUMPAGES")],
            ..Default::default()
        };
        let cv = p.with_field_codes();
        assert_eq!(cv.text, "Page { PAGE } of { NUMPAGES }");
        assert_eq!((cv.fields[0].start, cv.fields[0].end), (5, 13));
        assert_eq!((cv.fields[1].start, cv.fields[1].end), (17, 29));
        for off in [0u32, 3, 5, 7, 8, 11, 13] {
            let d = p.source_offset_to_code_view(off);
            assert_eq!(p.code_view_offset_to_source(d), off, "round trip at {off}");
        }
        assert_eq!(p.source_offset_to_code_view(7), 13);
        assert_eq!(p.source_offset_to_code_view(13), 29);
        assert_eq!(p.code_view_offset_to_source(17), 11);
        /* Strictly-inside offsets land on the far boundary. */
        assert_eq!(p.source_offset_to_code_view(6), 13);
        assert_eq!(p.code_view_offset_to_source(9), 7);
        /* A field-free paragraph is the identity. */
        let plain = Paragraph {
            text: "abc".into(),
            ..Default::default()
        };
        assert_eq!(plain.with_field_codes().text, "abc");
        assert_eq!(plain.source_offset_to_code_view(2), 2);
    }

    #[test]
    fn restamp_pass_visits_body_and_parts_and_stays_byte_stable_when_unchanged() {
        let mut doc = DocumentTree::from_text("Page 9 end");
        doc.blocks[0] = Block::Paragraph(Paragraph {
            text: "Page 9 end".into(),
            fields: vec![field(5, 6, "PAGE")],
            source_xml: Some(b"<w:p/>".to_vec()),
            ..Default::default()
        });
        let doc = doc.with_updated_footer_part(
            "rF",
            vec![Block::Paragraph(Paragraph {
                text: "by X".into(),
                fields: vec![field(3, 4, "AUTHOR")],
                ..Default::default()
            })],
        );
        let mut visited: Vec<String> = Vec::new();
        let out = doc.restamp_fields(&mut |site| {
            visited.push(format!("{:?}/{}", site.story, site.index));
            match site.field.keyword().as_str() {
                "PAGE" => Some("9".into()), /* unchanged → no dirtying */
                "AUTHOR" => Some("Ibrahim".into()),
                _ => None,
            }
        });
        assert_eq!(visited, vec!["Body/0", "Footer(\"rF\")/0"]);
        let body = out.paragraph_at_path(&BlockPath::top(0)).unwrap();
        assert_eq!(body.text, "Page 9 end");
        assert!(!body.dirty, "an unchanged result must not dirty the paragraph");
        assert!(body.source_xml.is_some());
        let Block::Paragraph(fp) = &out.footers["rF"][0] else {
            panic!()
        };
        assert_eq!(fp.text, "by Ibrahim");
        assert_eq!((fp.fields[0].start, fp.fields[0].end), (3, 10));
        assert!(fp.dirty);

        /* Environment convenience: FILENAME/AUTHOR/DATE resolve, PAGE is left alone. */
        let env = FieldEnv {
            author: Some("Zed".into()),
            ..Default::default()
        };
        let out2 = doc.restamp_fields_with_env(&env);
        let Block::Paragraph(fp2) = &out2.footers["rF"][0] else {
            panic!()
        };
        assert_eq!(fp2.text, "by Zed");
        assert_eq!(
            out2.paragraph_at_path(&BlockPath::top(0)).unwrap().text,
            "Page 9 end"
        );
    }

    #[test]
    fn set_field_instruction_replaces_code_and_keeps_result() {
        let mut doc = DocumentTree::from_text("Page 9 end");
        doc.blocks[0] = Block::Paragraph(Paragraph {
            text: "Page 9 end".into(),
            fields: vec![field(5, 6, "PAGE")],
            ..Default::default()
        });
        let at = LogicalPos::new(BlockPath::top(0), 6);
        let out = doc
            .set_field_instruction(at.clone(), " NUMPAGES ")
            .expect("field at caret");
        let p = out.paragraph_at_path(&BlockPath::top(0)).unwrap();
        assert_eq!(p.text, "Page 9 end");
        assert_eq!(p.fields[0].instruction, "NUMPAGES");
        assert!(p.dirty);
        assert!(doc.set_field_instruction(at.clone(), "   ").is_none());
        assert!(
            doc.set_field_instruction(LogicalPos::new(BlockPath::top(0), 2), "X")
                .is_none()
        );
    }

    #[test]
    fn to_code_view_transforms_body_and_parts() {
        let mut doc = DocumentTree::from_text("p 1");
        doc.blocks[0] = Block::Paragraph(Paragraph {
            text: "p 1".into(),
            fields: vec![field(2, 3, "PAGE")],
            ..Default::default()
        });
        let doc = doc.with_updated_header_part(
            "rH",
            vec![Block::Paragraph(Paragraph {
                text: "of 3".into(),
                fields: vec![field(3, 4, "NUMPAGES")],
                ..Default::default()
            })],
        );
        assert!(doc.has_any_fields());
        let cv = doc.to_code_view();
        assert_eq!(cv.paragraph_at_path(&BlockPath::top(0)).unwrap().text, "p { PAGE }");
        let Block::Paragraph(hp) = &cv.headers["rH"][0] else {
            panic!()
        };
        assert_eq!(hp.text, "of { NUMPAGES }");
        assert!(!DocumentTree::from_text("plain").has_any_fields());
    }
}
