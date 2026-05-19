//! `engine` — document model + undo stack.
//!
//! Phase 1 weeks 15–18: plain-text paragraphs, in-place text insertion,
//! cheap snapshots via `im::Vector` for undo/redo.

use im::Vector;

#[derive(Debug, Clone, Default)]
pub struct DocumentTree {
    pub paragraphs: Vector<Paragraph>,
}

#[derive(Debug, Clone, Default)]
pub struct Paragraph {
    pub text: String,
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
        });
        Self { paragraphs }
    }

    /// Build a document from a list of paragraph plain-text bodies.
    pub fn from_paragraphs<I: IntoIterator<Item = String>>(texts: I) -> Self {
        let mut paragraphs = Vector::new();
        for t in texts {
            paragraphs.push_back(Paragraph { text: t });
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
            });
            return Self { paragraphs };
        }
        let para_idx = (at.para as usize).min(paragraphs.len() - 1);
        let mut para = paragraphs[para_idx].clone();
        let offset = (at.offset as usize).min(para.text.len());
        para.text.insert_str(offset, text);
        paragraphs.set(para_idx, para);
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
