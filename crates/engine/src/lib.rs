//! `engine` — document model + undo stack.
//!
//! Phase 1 weeks 15–18: plain-text paragraphs, in-place text insertion,
//! cheap snapshots via `im::Vector` for undo/redo.

use im::Vector;

#[derive(Debug, Clone, Default)]
pub struct DocumentTree {
    pub paragraphs: Vector<Paragraph>,
}

/// Inline style for a run of characters. Phase 3 rich-text scope is font size
/// and colour; bold / italic / underline land in a later typography PR.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SpanStyle {
    pub font_size: Option<f32>,
    pub color: Option<[u8; 4]>,
}

impl SpanStyle {
    /// Overlay `patch`'s set fields onto `self`.
    fn merged_with(self, patch: SpanStyle) -> SpanStyle {
        SpanStyle {
            font_size: patch.font_size.or(self.font_size),
            color: patch.color.or(self.color),
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

#[derive(Debug, Clone, Default)]
pub struct Paragraph {
    pub text: String,
    /// Non-overlapping styled ranges, sorted by `start`; default-styled ranges
    /// are omitted. An empty list is plain text.
    pub spans: Vec<StyleRun>,
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
        Paragraph { text, spans }
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
            },
            Paragraph {
                text: self.text[at as usize..].to_owned(),
                spans: right,
            },
        )
    }

    /// Append `other` to a copy of `self`, shifting `other`'s spans right.
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
        Paragraph { text, spans }
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
            });
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
                    color: None
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
        };
        let big = SpanStyle {
            font_size: Some(30.0),
            color: None,
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
        };
        assert_eq!(p.next_offset(0), 1);
        assert_eq!(p.next_offset(1), 3);
        assert_eq!(p.prev_offset(4), 3);
        assert_eq!(p.prev_offset(3), 1);
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
}
