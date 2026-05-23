//! `engine` — document model + undo stack.
//!
//! Phase 1 weeks 15–18: plain-text paragraphs, in-place text insertion,
//! cheap snapshots via `im::Vector` for undo/redo.

use im::Vector;

pub mod html;

#[derive(Debug, Clone, Default)]
pub struct DocumentTree {
    pub paragraphs: Vector<Paragraph>,
}

/// A selectable font family (Backlog #9). `engine-wasm` resolves it to a
/// loaded font face when building layout style spans; the pure document model
/// just stores the choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontFamily {
    Amiri,
    LiberationSans,
    NotoNaskhArabic,
}

/// Inline style for a run of characters: font size, colour, the
/// bold / italic / underline / strikethrough flags, a background (highlight)
/// colour, and a font family. All are carried through layout and render.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SpanStyle {
    pub font_size: Option<f32>,
    pub color: Option<[u8; 4]>,
    pub bold: Option<bool>,
    pub italic: Option<bool>,
    pub underline: Option<bool>,
    pub strike: Option<bool>,
    pub bg_color: Option<[u8; 4]>,
    pub font_family: Option<FontFamily>,
}

impl SpanStyle {
    /// Overlay `patch`'s set fields onto `self`.
    pub fn merged_with(self, patch: SpanStyle) -> SpanStyle {
        SpanStyle {
            font_size: patch.font_size.or(self.font_size),
            color: patch.color.or(self.color),
            bold: patch.bold.or(self.bold),
            italic: patch.italic.or(self.italic),
            underline: patch.underline.or(self.underline),
            strike: patch.strike.or(self.strike),
            bg_color: patch.bg_color.or(self.bg_color),
            font_family: patch.font_family.or(self.font_family),
        }
    }
}

/// A styled byte range `[start, end)` within a paragraph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyleRun {
    pub start: u32,
    pub end: u32,
    pub style: SpanStyle,
}

/// Paragraph text alignment (Backlog #9). `Start` / `End` are
/// writing-direction-relative — they resolve against the base direction at
/// layout time; `Center` and `Justify` are absolute. Mirrors
/// `text_pipeline::Alignment`; kept here so the pure document model carries no
/// dependency on the text-shaping crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Start,
    End,
    Center,
    Justify,
}

/// Paragraph indentation. OOXML carries these as twips (1/1440 inch); the
/// engine stores them in the same unit and converts to layout pixels at
/// `engine-wasm` boundary so the pure document model has no float-DPI
/// dependency. `first_line` and `hanging` are mutually exclusive in OOXML;
/// the reader sets the matching field and zeroes the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Indent {
    pub start_twips: i32,
    pub end_twips: i32,
    pub first_line_twips: i32,
    pub hanging_twips: i32,
}

/// Per-paragraph vertical spacing. Twips, matching `<w:spacing>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Spacing {
    pub before_twips: i32,
    pub after_twips: i32,
}

/// Explicit paragraph base direction (`<w:bidi/>` for RTL). `None` lets
/// `text_pipeline::first_strong_direction` infer from the first strong
/// character — the current document-wide default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextDirection {
    Ltr,
    Rtl,
}

/// Per-paragraph line-height override (`<w:spacing w:line>` /
/// `w:lineRule>`). `None` inherits the renderer's default line height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LineHeight {
    /// `w:lineRule="auto"` — `w:line` is a 240-ths multiple of single line
    /// height; we store the integer twips for round-trip, layout converts.
    Auto { twips: i32 },
    /// `w:lineRule="exact"` — fixed twip height; overflow clips.
    Exact { twips: i32 },
    /// `w:lineRule="atLeast"` — minimum; grows for tall glyphs.
    AtLeast { twips: i32 },
}

/// Paragraph-level properties parsed from `<w:pPr>`. Holds every field the
/// engine needs to round-trip a Word paragraph; layout consumes the
/// alignment / indent / spacing / direction / line-height subset.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParaProperties {
    pub alignment: Option<Alignment>,
    pub indent: Indent,
    pub spacing: Spacing,
    pub direction: Option<TextDirection>,
    pub line_height: Option<LineHeight>,
    pub keep_next: bool,
    pub keep_lines: bool,
    pub page_break_before: bool,
}

impl ParaProperties {
    /// Overlay `patch` onto `self` using OOXML cascade semantics: a child
    /// style with a *set* (non-default) field overrides the parent. Used by
    /// the Phase 3 `format_docx::style_resolver` to fold a basedOn chain
    /// root → leaf and then drop direct `<w:pPr>` on top.
    ///
    /// **Known limitation.** Engine fields are flat (`Indent`, `Spacing` are
    /// non-`Option` structs), so we cannot distinguish "child specified 0"
    /// from "child inherited". A child whose `<w:ind w:start="0"/>` is
    /// intentional will lose to a parent's non-zero start. Real-world
    /// stylesheets virtually never set 0 explicitly, so the trade-off is
    /// acceptable for Phase 3; Phase 4+ may widen to `Option`.
    pub fn merged_with(self, patch: ParaProperties) -> ParaProperties {
        ParaProperties {
            alignment: patch.alignment.or(self.alignment),
            indent: if patch.indent == Indent::default() {
                self.indent
            } else {
                patch.indent
            },
            spacing: if patch.spacing == Spacing::default() {
                self.spacing
            } else {
                patch.spacing
            },
            direction: patch.direction.or(self.direction),
            line_height: patch.line_height.or(self.line_height),
            keep_next: patch.keep_next || self.keep_next,
            keep_lines: patch.keep_lines || self.keep_lines,
            page_break_before: patch.page_break_before || self.page_break_before,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Paragraph {
    pub text: String,
    /// Non-overlapping styled ranges, sorted by `start`; default-styled ranges
    /// are omitted. An empty list is plain text.
    pub spans: Vec<StyleRun>,
    /// Paragraph-level properties (`<w:pPr>`). Default = inherit everything
    /// from the render config / document defaults.
    pub props: ParaProperties,
    /// Phase 3 passthrough optimisation. `false` on load; flips to `true` the
    /// first time any engine mutation produces a derived paragraph. The writer
    /// emits `source_xml` verbatim when this is `false` and ignores it
    /// otherwise — so unmutated stylesheet-driven paragraphs round-trip
    /// byte-identical.
    pub dirty: bool,
    /// Raw `<w:p>...</w:p>` source bytes captured by the reader (Phase 3).
    /// `None` for paragraphs the engine synthesised (`from_text`, splits,
    /// pastes); `Some` for any paragraph parsed from a real `.docx`.
    pub source_xml: Option<Vec<u8>>,
}

impl Paragraph {
    /// Resolved style at byte offset `at` (default if no span covers it).
    pub fn style_at(&self, at: u32) -> SpanStyle {
        self.spans
            .iter()
            .find(|s| at >= s.start && at < s.end)
            .map_or(SpanStyle::default(), |s| s.style)
    }

    /// Return a copy with `patch` overlaid on the byte range `[start, end)`.
    /// Existing spans are split at the boundaries; every covered sub-range
    /// merges the patch's set fields. Adjacent equal spans are coalesced and
    /// default-only spans dropped, so the representation stays minimal.
    pub fn apply_style(&self, start: u32, end: u32, patch: SpanStyle) -> Paragraph {
        let text_len = self.text.len() as u32;
        let start = start.min(text_len);
        let end = end.min(text_len);
        if start >= end {
            return self.clone();
        }

        /* Every boundary: text extent, the patch range, existing span edges. */
        let mut bounds: Vec<u32> = vec![0, text_len, start, end];
        for s in &self.spans {
            bounds.push(s.start);
            bounds.push(s.end);
        }
        bounds.retain(|&b| b <= text_len);
        bounds.sort_unstable();
        bounds.dedup();

        /* Re-derive each interval's style, merging the patch where covered. */
        let mut spans: Vec<StyleRun> = Vec::new();
        for win in bounds.windows(2) {
            let (a, b) = (win[0], win[1]);
            let mut style = self.style_at(a);
            if a >= start && b <= end {
                style = style.merged_with(patch);
            }
            if style == SpanStyle::default() {
                continue;
            }
            match spans.last_mut() {
                Some(prev) if prev.end == a && prev.style == style => prev.end = b,
                _ => spans.push(StyleRun {
                    start: a,
                    end: b,
                    style,
                }),
            }
        }

        Paragraph {
            text: self.text.clone(),
            spans,
            props: self.props.clone(),
            dirty: true,
            source_xml: None,
        }
    }

    /// Byte range `[start, end)` of the word containing caret position
    /// `offset` — a whitespace-delimited span (PHASE_4_HEADLESS_UI.md §7,
    /// double-click select). When `offset` sits on whitespace, the run of
    /// whitespace is returned. `offset` is clamped to a char boundary.
    pub fn word_bounds(&self, offset: u32) -> (u32, u32) {
        let text = self.text.as_str();
        let len = text.len();
        if len == 0 {
            return (0, 0);
        }
        let mut off = (offset as usize).min(len);
        while off > 0 && !text.is_char_boundary(off) {
            off -= 1;
        }
        /* Classify by the char to the right; at end-of-text, the char left. */
        let ws = text[off..]
            .chars()
            .next()
            .or_else(|| text[..off].chars().next_back())
            .is_some_and(char::is_whitespace);

        let mut start = off;
        for (i, c) in text[..off].char_indices().rev() {
            if c.is_whitespace() == ws {
                start = i;
            } else {
                break;
            }
        }
        let mut end = off;
        for (i, c) in text[off..].char_indices() {
            if c.is_whitespace() == ws {
                end = off + i + c.len_utf8();
            } else {
                break;
            }
        }
        (start as u32, end as u32)
    }

    /// Return a copy with bytes `[s, e)` removed. Style spans are clipped and
    /// shifted across the deletion.
    pub fn delete_text(&self, s: u32, e: u32) -> Paragraph {
        let len = self.text.len() as u32;
        let s = s.min(len);
        let e = e.min(len);
        if s >= e {
            return self.clone();
        }
        let mut text = self.text.clone();
        text.replace_range(s as usize..e as usize, "");
        let gap = e - s;
        /* Map a pre-delete offset to its post-delete position. */
        let map = |p: u32| -> u32 {
            if p <= s {
                p
            } else if p >= e {
                p - gap
            } else {
                s
            }
        };
        let mut spans = Vec::new();
        for run in &self.spans {
            let (ns, ne) = (map(run.start), map(run.end));
            if ns < ne {
                spans.push(StyleRun {
                    start: ns,
                    end: ne,
                    style: run.style,
                });
            }
        }
        Paragraph {
            text,
            spans,
            props: self.props.clone(),
            dirty: true,
            source_xml: None,
        }
    }

    /// Split into `[0, at)` and `[at, len)`. Spans straddling `at` are split.
    pub fn split_at(&self, at: u32) -> (Paragraph, Paragraph) {
        let len = self.text.len() as u32;
        let at = at.min(len);
        let mut left = Vec::new();
        let mut right = Vec::new();
        for run in &self.spans {
            if run.start < at {
                left.push(StyleRun {
                    start: run.start,
                    end: run.end.min(at),
                    style: run.style,
                });
            }
            if run.end > at {
                right.push(StyleRun {
                    start: run.start.max(at) - at,
                    end: run.end - at,
                    style: run.style,
                });
            }
        }
        (
            Paragraph {
                text: self.text[..at as usize].to_owned(),
                spans: left,
                props: self.props.clone(),
                dirty: true,
                source_xml: None,
            },
            Paragraph {
                text: self.text[at as usize..].to_owned(),
                spans: right,
                props: self.props.clone(),
                dirty: true,
                source_xml: None,
            },
        )
    }

    /// Append `other` to a copy of `self`, shifting `other`'s spans right.
    /// The merged paragraph keeps `self`'s alignment — the surviving
    /// paragraph mark wins when a paragraph break is deleted.
    pub fn concat(&self, other: &Paragraph) -> Paragraph {
        let shift = self.text.len() as u32;
        let mut text = self.text.clone();
        text.push_str(&other.text);
        let mut spans = self.spans.clone();
        for run in &other.spans {
            spans.push(StyleRun {
                start: run.start + shift,
                end: run.end + shift,
                style: run.style,
            });
        }
        Paragraph {
            text,
            spans,
            props: self.props.clone(),
            dirty: true,
            source_xml: None,
        }
    }

    /// Byte offset of the char boundary immediately before `o` (clamped to 0).
    pub fn prev_offset(&self, o: u32) -> u32 {
        let o = (o as usize).min(self.text.len());
        self.text[..o]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i as u32)
    }

    /// Byte offset of the char boundary immediately after `o` (clamped to len).
    pub fn next_offset(&self, o: u32) -> u32 {
        let o = (o as usize).min(self.text.len());
        self.text[o..]
            .chars()
            .next()
            .map_or(o as u32, |c| (o + c.len_utf8()) as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LogicalPos {
    pub para: u32,
    /// Byte offset within the paragraph (UTF-8).
    pub offset: u32,
}

impl DocumentTree {
    pub fn new() -> Self {
        Self {
            paragraphs: Vector::new(),
        }
    }

    /// Build a single-paragraph document from a plain string.
    pub fn from_text(text: &str) -> Self {
        let mut paragraphs = Vector::new();
        paragraphs.push_back(Paragraph {
            text: text.to_owned(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            dirty: false,
            source_xml: None,
        });
        Self { paragraphs }
    }

    /// Build a document from a list of paragraph plain-text bodies.
    pub fn from_paragraphs<I: IntoIterator<Item = String>>(texts: I) -> Self {
        let mut paragraphs = Vector::new();
        for t in texts {
            paragraphs.push_back(Paragraph {
                text: t,
                spans: Vec::new(),
                props: ParaProperties::default(),
                dirty: false,
                source_xml: None,
            });
        }
        Self { paragraphs }
    }

    /// Build a document from pre-styled paragraphs — the `.docx` reader (run
    /// properties → spans) and the HTML paste path both produce these.
    pub fn from_rich_paragraphs<I: IntoIterator<Item = Paragraph>>(paras: I) -> Self {
        let mut paragraphs = Vector::new();
        for p in paras {
            paragraphs.push_back(p);
        }
        Self { paragraphs }
    }

    pub fn paragraph_count(&self) -> u32 {
        self.paragraphs.len() as u32
    }

    pub fn paragraph_text(&self, idx: u32) -> Option<&str> {
        self.paragraphs.get(idx as usize).map(|p| p.text.as_str())
    }

    pub fn end_of_document(&self) -> LogicalPos {
        if self.paragraphs.is_empty() {
            return LogicalPos { para: 0, offset: 0 };
        }
        let last = self.paragraphs.len() - 1;
        let offset = self.paragraphs[last].text.len() as u32;
        LogicalPos {
            para: last as u32,
            offset,
        }
    }

    /// Insert `text` at `at`. Out-of-range positions are clamped to end of
    /// document. Returns the new tree (the old one is structurally shared via
    /// `im::Vector`).
    pub fn insert_text(&self, at: LogicalPos, text: &str) -> Self {
        if text.is_empty() {
            return self.clone();
        }
        let mut paragraphs = self.paragraphs.clone();
        if paragraphs.is_empty() {
            paragraphs.push_back(Paragraph {
                text: text.to_owned(),
                spans: Vec::new(),
                props: ParaProperties::default(),
                dirty: true,
                source_xml: None,
            });
            return Self { paragraphs };
        }
        let para_idx = (at.para as usize).min(paragraphs.len() - 1);
        let mut para = paragraphs[para_idx].clone();
        let offset = (at.offset as usize).min(para.text.len());
        para.text.insert_str(offset, text);
        /* Shift styled spans across the insertion point — a span containing
        the point grows, spans wholly after it slide right. */
        let off = offset as u32;
        let len = text.len() as u32;
        for s in &mut para.spans {
            if s.start >= off {
                s.start += len;
            }
            if s.end > off {
                s.end += len;
            }
        }
        para.dirty = true;
        para.source_xml = None;
        paragraphs.set(para_idx, para);
        Self { paragraphs }
    }

    /// Apply a style `patch` over the logical range `[start, end)`. Splits and
    /// merges spans on every covered paragraph; unaffected paragraphs are
    /// structurally shared.
    pub fn apply_style(&self, start: LogicalPos, end: LogicalPos, patch: SpanStyle) -> Self {
        let mut paragraphs = self.paragraphs.clone();
        if paragraphs.is_empty() {
            return self.clone();
        }
        let last_idx = paragraphs.len() - 1;
        let first = (start.para as usize).min(last_idx);
        let last = (end.para as usize).min(last_idx);
        for p in first..=last {
            let lo = if p == first { start.offset } else { 0 };
            let hi = if p == last {
                end.offset
            } else {
                paragraphs[p].text.len() as u32
            };
            let styled = paragraphs[p].apply_style(lo, hi, patch);
            paragraphs.set(p, styled);
        }
        Self { paragraphs }
    }

    /// Set `align` on every paragraph the logical range `[start, end)` spans
    /// (Backlog #9). Paragraphs outside the range are structurally shared.
    /// `start`/`end` are expected in document order.
    pub fn set_alignment(&self, start: LogicalPos, end: LogicalPos, align: Alignment) -> Self {
        let mut paragraphs = self.paragraphs.clone();
        if paragraphs.is_empty() {
            return self.clone();
        }
        let last = paragraphs.len() - 1;
        let first = (start.para as usize).min(last);
        let final_para = (end.para as usize).min(last);
        for p in first..=final_para {
            let mut para = paragraphs[p].clone();
            para.props.alignment = Some(align);
            para.dirty = true;
            para.source_xml = None;
            paragraphs.set(p, para);
        }
        Self { paragraphs }
    }

    /// Delete the logical range `[start, end)`. A range spanning paragraphs
    /// merges the partial first and last paragraphs and drops those between.
    pub fn delete_range(&self, start: LogicalPos, end: LogicalPos) -> Self {
        let (start, end) = if (start.para, start.offset) <= (end.para, end.offset) {
            (start, end)
        } else {
            (end, start)
        };
        let mut paragraphs = self.paragraphs.clone();
        if paragraphs.is_empty() {
            return self.clone();
        }
        let last = paragraphs.len() - 1;
        let sp = (start.para as usize).min(last);
        let ep = (end.para as usize).min(last);
        if sp == ep {
            let edited = paragraphs[sp].delete_text(start.offset, end.offset);
            paragraphs.set(sp, edited);
        } else {
            let head = paragraphs[sp].split_at(start.offset).0;
            let tail = paragraphs[ep].split_at(end.offset).1;
            let merged = head.concat(&tail);
            for _ in sp..ep {
                paragraphs.remove(sp + 1);
            }
            paragraphs.set(sp, merged);
        }
        Self { paragraphs }
    }

    /// Split the paragraph at `at`, the break falling between the two halves.
    pub fn split_paragraph(&self, at: LogicalPos) -> Self {
        let mut paragraphs = self.paragraphs.clone();
        if paragraphs.is_empty() {
            paragraphs.push_back(Paragraph::default());
            paragraphs.push_back(Paragraph::default());
            return Self { paragraphs };
        }
        let idx = (at.para as usize).min(paragraphs.len() - 1);
        let (left, right) = paragraphs[idx].split_at(at.offset);
        paragraphs.set(idx, left);
        paragraphs.insert(idx + 1, right);
        Self { paragraphs }
    }

    /// Insert `text` at `at`, splitting it into separate paragraphs on every
    /// newline — `\r\n` and bare `\r` are normalized to `\n` first. A `text`
    /// with no newline behaves exactly like [`DocumentTree::insert_text`].
    /// Returns the new tree and the caret position at the end of the last
    /// inserted line (Backlog #12, multi-line paste).
    pub fn insert_multiline(&self, at: LogicalPos, text: &str) -> (Self, LogicalPos) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<&str> = normalized.split('\n').collect();
        let mut doc = self.clone();
        let mut cur = at;
        for (i, line) in lines.iter().enumerate() {
            doc = doc.insert_text(cur, line);
            let after = LogicalPos {
                para: cur.para,
                offset: cur.offset + line.len() as u32,
            };
            if i + 1 < lines.len() {
                /* A newline follows this line — break the paragraph so the
                next line lands in a fresh one; the remainder of the original
                paragraph rides along on the tail. */
                doc = doc.split_paragraph(after);
                cur = LogicalPos {
                    para: cur.para + 1,
                    offset: 0,
                };
            } else {
                cur = after;
            }
        }
        (doc, cur)
    }

    /// Extract the logical range `[start, end)` as standalone paragraphs,
    /// style spans clipped and shifted to local offsets. Drives rich
    /// clipboard copy — HTML + `.docx`-fragment generation (Backlog #12).
    pub fn slice(&self, start: LogicalPos, end: LogicalPos) -> Vec<Paragraph> {
        let (start, end) = if (start.para, start.offset) <= (end.para, end.offset) {
            (start, end)
        } else {
            (end, start)
        };
        if self.paragraphs.is_empty() {
            return Vec::new();
        }
        let last = self.paragraphs.len() - 1;
        let sp = (start.para as usize).min(last);
        let ep = (end.para as usize).min(last);
        if sp == ep {
            /* Clip to `end`, then take the tail from `start`. */
            let head = self.paragraphs[sp].split_at(end.offset).0;
            return vec![head.split_at(start.offset).1];
        }
        let mut out = Vec::with_capacity(ep - sp + 1);
        out.push(self.paragraphs[sp].split_at(start.offset).1);
        for p in (sp + 1)..ep {
            out.push(self.paragraphs[p].clone());
        }
        out.push(self.paragraphs[ep].split_at(end.offset).0);
        out
    }

    /// Insert pre-styled `paras` at `at`; returns the new tree and the caret
    /// at the end of the inserted content. The caller deletes any active
    /// selection first. Drives HTML paste (Backlog #12).
    pub fn insert_rich(&self, at: LogicalPos, paras: &[Paragraph]) -> (Self, LogicalPos) {
        if paras.is_empty() {
            return (self.clone(), at);
        }
        let mut paragraphs = self.paragraphs.clone();
        if paragraphs.is_empty() {
            paragraphs.push_back(Paragraph::default());
        }
        let idx = (at.para as usize).min(paragraphs.len() - 1);
        let (head, tail) = paragraphs[idx].split_at(at.offset);
        if paras.len() == 1 {
            let caret = LogicalPos {
                para: idx as u32,
                offset: (head.text.len() + paras[0].text.len()) as u32,
            };
            paragraphs.set(idx, head.concat(&paras[0]).concat(&tail));
            return (Self { paragraphs }, caret);
        }
        let lastp = &paras[paras.len() - 1];
        let caret = LogicalPos {
            para: (idx + paras.len() - 1) as u32,
            offset: lastp.text.len() as u32,
        };
        paragraphs.set(idx, head.concat(&paras[0]));
        for (k, p) in paras[1..paras.len() - 1].iter().enumerate() {
            paragraphs.insert(idx + 1 + k, p.clone());
        }
        paragraphs.insert(idx + paras.len() - 1, lastp.concat(&tail));
        (Self { paragraphs }, caret)
    }

    /// Extract the text of the logical range `[start, end)`. Paragraphs the
    /// range spans are joined by `\n`. Used for clipboard copy.
    pub fn text_range(&self, start: LogicalPos, end: LogicalPos) -> String {
        let (start, end) = if (start.para, start.offset) <= (end.para, end.offset) {
            (start, end)
        } else {
            (end, start)
        };
        if self.paragraphs.is_empty() {
            return String::new();
        }
        let last = self.paragraphs.len() - 1;
        let sp = (start.para as usize).min(last);
        let ep = (end.para as usize).min(last);
        let mut out = String::new();
        for p in sp..=ep {
            let para = &self.paragraphs[p];
            let len = para.text.len();
            let lo = if p == sp {
                (start.offset as usize).min(len)
            } else {
                0
            };
            let hi = if p == ep {
                (end.offset as usize).min(len)
            } else {
                len
            };
            if p > sp {
                out.push('\n');
            }
            if lo < hi {
                out.push_str(&para.text[lo..hi]);
            }
        }
        out
    }
}

/// Bounded undo/redo snapshot stack. Pushing a new snapshot truncates the
/// redo branch (standard editor semantics).
#[derive(Debug, Clone)]
pub struct UndoStack {
    /// Each element is a complete document snapshot. `im::Vector` clones in O(1)
    /// so pushing a snapshot is cheap structurally; only the modified
    /// `Paragraph.text` allocates.
    snapshots: Vec<DocumentTree>,
    /// Index of the current document in `snapshots`. Always `< snapshots.len()`.
    cursor: usize,
    /// Maximum snapshots retained (oldest are dropped on overflow).
    cap: usize,
}

impl UndoStack {
    pub fn new(initial: DocumentTree, cap: usize) -> Self {
        Self {
            snapshots: vec![initial],
            cursor: 0,
            cap,
        }
    }

    pub fn current(&self) -> &DocumentTree {
        &self.snapshots[self.cursor]
    }

    pub fn replace_current(&mut self, doc: DocumentTree) {
        self.snapshots[self.cursor] = doc;
    }

    pub fn push(&mut self, doc: DocumentTree) {
        /* Truncate any redo branch. */
        if self.cursor + 1 < self.snapshots.len() {
            self.snapshots.truncate(self.cursor + 1);
        }
        self.snapshots.push(doc);
        self.cursor = self.snapshots.len() - 1;
        /* Cap from the bottom. */
        while self.snapshots.len() > self.cap {
            self.snapshots.remove(0);
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    pub fn undo(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        if self.cursor + 1 >= self.snapshots.len() {
            return false;
        }
        self.cursor += 1;
        true
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }

    pub fn can_redo(&self) -> bool {
        self.cursor + 1 < self.snapshots.len()
    }

    pub fn depth(&self) -> u32 {
        self.snapshots.len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_into_empty() {
        let d = DocumentTree::new();
        let d = d.insert_text(LogicalPos { para: 0, offset: 0 }, "hello");
        assert_eq!(d.paragraph_text(0), Some("hello"));
    }

    #[test]
    fn insert_mid_paragraph() {
        let d = DocumentTree::from_text("hello world");
        let d = d.insert_text(LogicalPos { para: 0, offset: 5 }, ",");
        assert_eq!(d.paragraph_text(0), Some("hello, world"));
    }

    #[test]
    fn apply_style_creates_span() {
        let doc = DocumentTree::from_text("hello world");
        let doc = doc.apply_style(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 5 },
            SpanStyle {
                font_size: Some(20.0),
                color: None,
                ..Default::default()
            },
        );
        let spans = &doc.paragraphs[0].spans;
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0],
            StyleRun {
                start: 0,
                end: 5,
                style: SpanStyle {
                    font_size: Some(20.0),
                    color: None,
                    ..Default::default()
                },
            }
        );
    }

    #[test]
    fn overlapping_styles_split_and_merge() {
        let doc = DocumentTree::from_text("hello world");
        let red = SpanStyle {
            font_size: None,
            color: Some([255, 0, 0, 255]),
            ..Default::default()
        };
        let big = SpanStyle {
            font_size: Some(30.0),
            color: None,
            ..Default::default()
        };
        let doc = doc.apply_style(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 8 },
            red,
        );
        let doc = doc.apply_style(
            LogicalPos { para: 0, offset: 4 },
            LogicalPos {
                para: 0,
                offset: 11,
            },
            big,
        );
        let spans = &doc.paragraphs[0].spans;
        /* [0,4) red ; [4,8) red+big ; [8,11) big */
        assert_eq!(spans.len(), 3);
        assert_eq!((spans[0].start, spans[0].end), (0, 4));
        assert_eq!(spans[0].style, red);
        assert_eq!((spans[1].start, spans[1].end), (4, 8));
        assert_eq!(
            spans[1].style,
            SpanStyle {
                font_size: Some(30.0),
                color: Some([255, 0, 0, 255]),
                ..Default::default()
            }
        );
        assert_eq!((spans[2].start, spans[2].end), (8, 11));
        assert_eq!(spans[2].style, big);
    }

    #[test]
    fn insert_shifts_spans() {
        let doc = DocumentTree::from_text("abcdef");
        let doc = doc.apply_style(
            LogicalPos { para: 0, offset: 2 },
            LogicalPos { para: 0, offset: 4 },
            SpanStyle {
                font_size: None,
                color: Some([1, 2, 3, 255]),
                ..Default::default()
            },
        );
        let doc = doc.insert_text(LogicalPos { para: 0, offset: 0 }, "XX");
        let span = doc.paragraphs[0].spans[0];
        assert_eq!((span.start, span.end), (4, 6));
    }

    #[test]
    fn word_bounds_latin() {
        let p = Paragraph {
            text: "hello world".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            dirty: false,
            source_xml: None,
        };
        assert_eq!(p.word_bounds(2), (0, 5));
        assert_eq!(p.word_bounds(0), (0, 5));
        assert_eq!(p.word_bounds(5), (5, 6)); // on the space
        assert_eq!(p.word_bounds(8), (6, 11));
        assert_eq!(p.word_bounds(11), (6, 11)); // end of text → last word
    }

    #[test]
    fn word_bounds_arabic() {
        /* "مرحبا بالعالم" — 5-char word, space, 7-char word; 2 bytes/char. */
        let p = Paragraph {
            text: "مرحبا بالعالم".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            dirty: false,
            source_xml: None,
        };
        assert_eq!(p.word_bounds(4), (0, 10));
        assert_eq!(p.word_bounds(0), (0, 10));
        assert_eq!(p.word_bounds(12), (11, 25)); // mid-char offset clamps
    }

    #[test]
    fn word_bounds_empty() {
        let p = Paragraph {
            text: String::new(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            dirty: false,
            source_xml: None,
        };
        assert_eq!(p.word_bounds(0), (0, 0));
    }

    #[test]
    fn delete_within_paragraph() {
        let d = DocumentTree::from_text("hello world");
        let d = d.delete_range(
            LogicalPos { para: 0, offset: 5 },
            LogicalPos {
                para: 0,
                offset: 11,
            },
        );
        assert_eq!(d.paragraph_text(0), Some("hello"));
    }

    #[test]
    fn delete_merges_paragraphs() {
        let d = DocumentTree::from_paragraphs(["abc".to_string(), "def".to_string()]);
        let d = d.delete_range(
            LogicalPos { para: 0, offset: 3 },
            LogicalPos { para: 1, offset: 0 },
        );
        assert_eq!(d.paragraph_count(), 1);
        assert_eq!(d.paragraph_text(0), Some("abcdef"));
    }

    #[test]
    fn delete_clips_spans() {
        let doc = DocumentTree::from_text("hello world");
        let doc = doc.apply_style(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 5 },
            SpanStyle {
                font_size: Some(20.0),
                color: None,
                ..Default::default()
            },
        );
        let doc = doc.delete_range(
            LogicalPos { para: 0, offset: 3 },
            LogicalPos { para: 0, offset: 5 },
        );
        assert_eq!(doc.paragraph_text(0), Some("hel world"));
        let spans = &doc.paragraphs[0].spans;
        assert_eq!(spans.len(), 1);
        assert_eq!((spans[0].start, spans[0].end), (0, 3));
    }

    #[test]
    fn split_paragraph_in_two() {
        let d = DocumentTree::from_text("hello world");
        let d = d.split_paragraph(LogicalPos { para: 0, offset: 5 });
        assert_eq!(d.paragraph_count(), 2);
        assert_eq!(d.paragraph_text(0), Some("hello"));
        assert_eq!(d.paragraph_text(1), Some(" world"));
    }

    #[test]
    fn prev_next_offset_utf8() {
        /* "a"=1 byte, "م"=2 bytes, "b"=1 byte → char boundaries 0,1,3,4. */
        let p = Paragraph {
            text: "aمb".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            dirty: false,
            source_xml: None,
        };
        assert_eq!(p.next_offset(0), 1);
        assert_eq!(p.next_offset(1), 3);
        assert_eq!(p.prev_offset(4), 3);
        assert_eq!(p.prev_offset(3), 1);
    }

    #[test]
    fn text_range_within_and_across() {
        let d = DocumentTree::from_paragraphs(["hello world".to_string(), "second".to_string()]);
        assert_eq!(
            d.text_range(
                LogicalPos { para: 0, offset: 0 },
                LogicalPos { para: 0, offset: 5 },
            ),
            "hello"
        );
        assert_eq!(
            d.text_range(
                LogicalPos { para: 0, offset: 6 },
                LogicalPos { para: 1, offset: 6 },
            ),
            "world\nsecond"
        );
        /* reversed args normalize to document order */
        assert_eq!(
            d.text_range(
                LogicalPos { para: 0, offset: 5 },
                LogicalPos { para: 0, offset: 0 },
            ),
            "hello"
        );
    }

    #[test]
    fn apply_style_bold_italic_underline() {
        let doc = DocumentTree::from_text("hello world");
        /* Apply bold over [0,5). */
        let doc = doc.apply_style(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 5 },
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(doc.paragraphs[0].spans.len(), 1);
        assert_eq!(doc.paragraphs[0].spans[0].style.bold, Some(true));
        /* Overlay italic + underline on the same range — they merge in. */
        let doc = doc.apply_style(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 5 },
            SpanStyle {
                italic: Some(true),
                underline: Some(true),
                ..Default::default()
            },
        );
        let style = doc.paragraphs[0].style_at(2);
        assert_eq!(style.bold, Some(true));
        assert_eq!(style.italic, Some(true));
        assert_eq!(style.underline, Some(true));
        /* Outside the styled range — unstyled. */
        assert_eq!(doc.paragraphs[0].style_at(8), SpanStyle::default());
    }

    #[test]
    fn undo_redo_cycle() {
        let initial = DocumentTree::from_text("abc");
        let mut undo = UndoStack::new(initial.clone(), 16);

        let d2 = initial.insert_text(LogicalPos { para: 0, offset: 3 }, "def");
        undo.push(d2.clone());
        assert_eq!(undo.current().paragraph_text(0), Some("abcdef"));

        let d3 = d2.insert_text(LogicalPos { para: 0, offset: 6 }, "ghi");
        undo.push(d3.clone());
        assert_eq!(undo.current().paragraph_text(0), Some("abcdefghi"));

        undo.undo();
        assert_eq!(undo.current().paragraph_text(0), Some("abcdef"));
        undo.undo();
        assert_eq!(undo.current().paragraph_text(0), Some("abc"));
        assert!(!undo.can_undo());

        undo.redo();
        assert_eq!(undo.current().paragraph_text(0), Some("abcdef"));
    }

    #[test]
    fn set_alignment_marks_spanned_paragraphs() {
        let d = DocumentTree::from_paragraphs(["a".into(), "b".into(), "c".into()]);
        let d = d.set_alignment(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 1, offset: 0 },
            Alignment::Center,
        );
        assert_eq!(d.paragraphs[0].props.alignment, Some(Alignment::Center));
        assert_eq!(d.paragraphs[1].props.alignment, Some(Alignment::Center));
        /* outside the range — untouched */
        assert_eq!(d.paragraphs[2].props.alignment, None);
    }

    #[test]
    fn alignment_survives_text_edits() {
        let d = DocumentTree::from_text("hello world");
        let d = d.set_alignment(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 0 },
            Alignment::End,
        );
        /* insertion clones the paragraph in place — alignment rides along */
        let d = d.insert_text(LogicalPos { para: 0, offset: 0 }, "X");
        assert_eq!(d.paragraph_text(0), Some("Xhello world"));
        assert_eq!(d.paragraphs[0].props.alignment, Some(Alignment::End));
        /* a style change preserves alignment */
        let d = d.apply_style(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 3 },
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(d.paragraphs[0].props.alignment, Some(Alignment::End));
        /* and so does a deletion */
        let d = d.delete_range(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 1 },
        );
        assert_eq!(d.paragraphs[0].props.alignment, Some(Alignment::End));
    }

    #[test]
    fn split_paragraph_inherits_alignment() {
        let d = DocumentTree::from_text("hello world");
        let d = d.set_alignment(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 0 },
            Alignment::Center,
        );
        let d = d.split_paragraph(LogicalPos { para: 0, offset: 5 });
        assert_eq!(d.paragraph_count(), 2);
        /* both halves carry the original paragraph's alignment */
        assert_eq!(d.paragraphs[0].props.alignment, Some(Alignment::Center));
        assert_eq!(d.paragraphs[1].props.alignment, Some(Alignment::Center));
    }

    #[test]
    fn merge_keeps_first_paragraph_alignment() {
        let d = DocumentTree::from_paragraphs(["abc".into(), "def".into()]);
        let d = d.set_alignment(
            LogicalPos { para: 0, offset: 0 },
            LogicalPos { para: 0, offset: 0 },
            Alignment::Center,
        );
        let d = d.set_alignment(
            LogicalPos { para: 1, offset: 0 },
            LogicalPos { para: 1, offset: 0 },
            Alignment::End,
        );
        /* deleting the paragraph break merges the two */
        let d = d.delete_range(
            LogicalPos { para: 0, offset: 3 },
            LogicalPos { para: 1, offset: 0 },
        );
        assert_eq!(d.paragraph_count(), 1);
        assert_eq!(d.paragraph_text(0), Some("abcdef"));
        /* the surviving paragraph keeps the first paragraph's alignment */
        assert_eq!(d.paragraphs[0].props.alignment, Some(Alignment::Center));
    }

    #[test]
    fn insert_multiline_single_line_is_plain_insert() {
        let d = DocumentTree::from_text("abcd");
        let (d, caret) = d.insert_multiline(LogicalPos { para: 0, offset: 2 }, "XY");
        assert_eq!(d.paragraph_count(), 1);
        assert_eq!(d.paragraph_text(0), Some("abXYcd"));
        assert_eq!(caret, LogicalPos { para: 0, offset: 4 });
    }

    #[test]
    fn insert_multiline_splits_into_paragraphs() {
        let d = DocumentTree::from_text("abcd");
        let (d, caret) = d.insert_multiline(LogicalPos { para: 0, offset: 2 }, "L0\nL1\nL2");
        assert_eq!(d.paragraph_count(), 3);
        /* the original paragraph splits around the caret; the tail rides the
        last pasted line's paragraph */
        assert_eq!(d.paragraph_text(0), Some("abL0"));
        assert_eq!(d.paragraph_text(1), Some("L1"));
        assert_eq!(d.paragraph_text(2), Some("L2cd"));
        assert_eq!(caret, LogicalPos { para: 2, offset: 2 });
    }

    #[test]
    fn insert_multiline_normalizes_crlf_and_cr() {
        let d = DocumentTree::from_text("");
        let (d, _) = d.insert_multiline(LogicalPos { para: 0, offset: 0 }, "a\r\nb\rc");
        assert_eq!(d.paragraph_count(), 3);
        assert_eq!(d.paragraph_text(0), Some("a"));
        assert_eq!(d.paragraph_text(1), Some("b"));
        assert_eq!(d.paragraph_text(2), Some("c"));
    }

    #[test]
    fn insert_multiline_trailing_newline_makes_empty_paragraph() {
        let d = DocumentTree::from_text("xy");
        let (d, caret) = d.insert_multiline(LogicalPos { para: 0, offset: 2 }, "Z\n");
        assert_eq!(d.paragraph_count(), 2);
        assert_eq!(d.paragraph_text(0), Some("xyZ"));
        assert_eq!(d.paragraph_text(1), Some(""));
        assert_eq!(caret, LogicalPos { para: 1, offset: 0 });
    }

    #[test]
    fn insert_multiline_into_second_paragraph() {
        let d = DocumentTree::from_paragraphs(["first".to_string(), "second".to_string()]);
        let (d, caret) = d.insert_multiline(LogicalPos { para: 1, offset: 3 }, "A\nB");
        assert_eq!(d.paragraph_count(), 3);
        assert_eq!(d.paragraph_text(0), Some("first"));
        assert_eq!(d.paragraph_text(1), Some("secA"));
        assert_eq!(d.paragraph_text(2), Some("Bond"));
        assert_eq!(caret, LogicalPos { para: 2, offset: 1 });
    }

    #[test]
    fn slice_single_paragraph_clips_and_shifts_spans() {
        /* "hello world" with bold over "world" (bytes 6-11). */
        let bold = SpanStyle {
            bold: Some(true),
            ..Default::default()
        };
        let para = Paragraph {
            text: "hello world".into(),
            spans: vec![StyleRun {
                start: 6,
                end: 11,
                style: bold,
            }],
            props: ParaProperties::default(),
            dirty: false,
            source_xml: None,
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        /* Slice "lo wor" (bytes 3-9) — the bold span clips to 3-6, local. */
        let cut = doc.slice(
            LogicalPos { para: 0, offset: 3 },
            LogicalPos { para: 0, offset: 9 },
        );
        assert_eq!(cut.len(), 1);
        assert_eq!(cut[0].text, "lo wor");
        assert_eq!(
            cut[0].spans,
            vec![StyleRun {
                start: 3,
                end: 6,
                style: bold,
            }]
        );
    }

    #[test]
    fn insert_rich_single_paragraph_merges_inline() {
        let doc = DocumentTree::from_text("hello world");
        let frag = vec![Paragraph {
            text: "BRAVE ".into(),
            spans: vec![],
            props: ParaProperties::default(),
            dirty: false,
            source_xml: None,
        }];
        let (out, caret) = doc.insert_rich(LogicalPos { para: 0, offset: 6 }, &frag);
        assert_eq!(out.paragraph_count(), 1);
        assert_eq!(out.paragraph_text(0), Some("hello BRAVE world"));
        assert_eq!(
            caret,
            LogicalPos {
                para: 0,
                offset: 12
            }
        );
    }

    #[test]
    fn insert_rich_multi_paragraph_splices_and_keeps_spans() {
        let doc = DocumentTree::from_text("ABCD");
        let bold = SpanStyle {
            bold: Some(true),
            ..Default::default()
        };
        let frag = vec![
            Paragraph {
                text: "one".into(),
                spans: vec![],
                props: ParaProperties::default(),
                dirty: false,
                source_xml: None,
            },
            Paragraph {
                text: "two".into(),
                spans: vec![StyleRun {
                    start: 0,
                    end: 3,
                    style: bold,
                }],
                props: ParaProperties::default(),
                dirty: false,
                source_xml: None,
            },
        ];
        let (out, caret) = doc.insert_rich(LogicalPos { para: 0, offset: 2 }, &frag);
        assert_eq!(out.paragraph_count(), 2);
        assert_eq!(out.paragraph_text(0), Some("ABone"));
        assert_eq!(out.paragraph_text(1), Some("twoCD"));
        assert_eq!(caret, LogicalPos { para: 1, offset: 3 });
        assert_eq!(out.paragraphs[1].style_at(0).bold, Some(true));
        assert_eq!(out.paragraphs[1].style_at(3).bold, None);
    }
}
