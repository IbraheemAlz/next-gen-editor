//! Issue #293 — the paragraph mark's own run properties
//! (`Paragraph::mark_style`, `<w:pPr><w:rPr>`): typing into an empty
//! paragraph inherits them, a split gives an empty half the insertion
//! formatting at the split point, a merge keeps the surviving paragraph's.

use crate::{
    Block, BlockPath, DocumentTree, GrabBag, LogicalPos, Paragraph, SpanStyle, StyleRun, snapshot,
};

fn pos(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

fn bold() -> SpanStyle {
    SpanStyle {
        bold: Some(true),
        ..Default::default()
    }
}

fn italic() -> SpanStyle {
    SpanStyle {
        italic: Some(true),
        ..Default::default()
    }
}

fn mark(p: &Paragraph) -> Option<SpanStyle> {
    p.mark_style.as_deref().cloned()
}

/// "Hello world" with "world" (6..11) bold.
fn hello_bold_world() -> DocumentTree {
    DocumentTree::from_text("Hello world").apply_style(pos(0, 6), pos(0, 11), bold())
}

/// Enter at the end of a bold run: the new, empty paragraph's mark is
/// bold, typing there is bold — and the paragraph the text stays in
/// keeps its own (unformatted) mark.
#[test]
fn enter_at_the_end_of_a_bold_run_types_bold() {
    let d = hello_bold_world().split_paragraph(pos(0, 11));
    let (left, right) = (d.nth_paragraph(0).unwrap(), d.nth_paragraph(1).unwrap());
    assert_eq!(right.text, "");
    assert_eq!(mark(right), Some(bold()));
    assert_eq!(mark(left), None, "the text half keeps the original mark");
    assert_eq!(right.typing_style_at(0), bold(), "toolbar preview");
    let typed = d.insert_text(pos(1, 0), "Next");
    let p = typed.nth_paragraph(1).unwrap();
    assert_eq!(p.style_at(0), bold());
    assert_eq!(p.style_at(3), bold());
    assert_eq!(
        p.spans,
        vec![StyleRun {
            start: 0,
            end: 4,
            style: bold(),
        }]
    );
}

/// Enter at the end of PLAIN text adds no mark (`None` stays `None` — no
/// `<w:rPr>` appears on save), and typing stays plain.
#[test]
fn an_unformatted_split_adds_no_mark() {
    let d = DocumentTree::from_text("plain").split_paragraph(pos(0, 5));
    assert_eq!(mark(d.nth_paragraph(1).unwrap()), None);
    assert_eq!(mark(d.nth_paragraph(0).unwrap()), None);
    let typed = d.insert_text(pos(1, 0), "x");
    assert!(typed.nth_paragraph(1).unwrap().spans.is_empty());
}

/// Enter at a paragraph START: the empty paragraph above takes the first
/// character's format (Word's insertion formatting there); the text half
/// keeps the original mark.
#[test]
fn enter_at_a_paragraph_start_marks_the_empty_paragraph_above() {
    let d = DocumentTree::from_text("Title rest").apply_style(pos(0, 0), pos(0, 5), italic());
    let d = d.split_paragraph(pos(0, 0));
    assert_eq!(d.nth_paragraph(0).unwrap().text, "");
    assert_eq!(mark(d.nth_paragraph(0).unwrap()), Some(italic()));
    assert_eq!(mark(d.nth_paragraph(1).unwrap()), None);
    let typed = d.insert_text(pos(0, 0), "x");
    assert_eq!(typed.nth_paragraph(0).unwrap().style_at(0), italic());
}

/// A mid-paragraph split keeps the original mark on both halves.
#[test]
fn a_mid_paragraph_split_keeps_the_mark_on_both_halves() {
    let mut d = hello_bold_world();
    let mut blocks = d.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[0] {
        p.mark_style = Some(Box::new(italic()));
    }
    d.blocks = blocks;
    let d = d.split_paragraph(pos(0, 3));
    assert_eq!(mark(d.nth_paragraph(0).unwrap()), Some(italic()));
    assert_eq!(mark(d.nth_paragraph(1).unwrap()), Some(italic()));
}

/// The mark never carries a run's grab bag (a `<w:lang>` on the run stays
/// on the run); `for_typing` keeps a mark's `<w:rPrChange>` off typed text.
#[test]
fn a_new_mark_takes_the_modeled_insertion_formatting_only() {
    let mut d = DocumentTree::from_text("word");
    let mut blocks = d.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[0] {
        p.spans = vec![StyleRun {
            start: 0,
            end: 4,
            style: SpanStyle {
                bold: Some(true),
                grab_bag: Some(Box::new(GrabBag {
                    fragments: vec![br#"<w:lang w:val="en-GB"/>"#.to_vec()],
                })),
                ..Default::default()
            },
        }];
    }
    d.blocks = blocks;
    let d = d.split_paragraph(pos(0, 4));
    assert_eq!(mark(d.nth_paragraph(1).unwrap()), Some(bold()));
}

/// Typing into an empty paragraph read with a mark (no split involved)
/// inherits it; text typed into a NON-empty paragraph ignores the mark.
#[test]
fn typing_into_an_empty_paragraph_inherits_its_mark() {
    let mut d = DocumentTree::from_paragraphs(["".to_string(), "text".to_string()]);
    let mut blocks = d.blocks.clone();
    for b in blocks.iter_mut() {
        if let Block::Paragraph(p) = b {
            p.mark_style = Some(Box::new(bold()));
        }
    }
    d.blocks = blocks;
    let typed = d.insert_text(pos(0, 0), "new");
    assert_eq!(typed.nth_paragraph(0).unwrap().style_at(1), bold());
    let typed = d.insert_text(pos(1, 4), "!");
    assert_eq!(
        typed.nth_paragraph(1).unwrap().style_at(4),
        SpanStyle::default()
    );
}

/// A merge keeps the surviving paragraph's mark: the head's, or the
/// tail's when the (empty) head merges away.
#[test]
fn a_merge_keeps_the_surviving_paragraphs_mark() {
    let mut d = DocumentTree::from_paragraphs(["head".to_string(), "tail".to_string()]);
    let mut blocks = d.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[0] {
        p.mark_style = Some(Box::new(bold()));
    }
    if let Block::Paragraph(p) = &mut blocks[1] {
        p.mark_style = Some(Box::new(italic()));
    }
    d.blocks = blocks;
    let merged = d.delete_range(pos(0, 4), pos(1, 0));
    assert_eq!(mark(merged.nth_paragraph(0).unwrap()), Some(bold()));
    let emptied = d.delete_range(pos(0, 0), pos(0, 4));
    let merged = emptied.delete_range(pos(0, 0), pos(1, 0));
    assert_eq!(mark(merged.nth_paragraph(0).unwrap()), Some(italic()));
}

/// `mark_follows_text`: formatting typed into an empty paragraph (pending
/// formatting) formats its mark too; a no-op when they already agree.
#[test]
fn the_mark_follows_text_typed_into_an_empty_paragraph() {
    let d = hello_bold_world().split_paragraph(pos(0, 11));
    /* Pending bold-off typed into the bold-marked empty paragraph. */
    let typed = d
        .insert_text(pos(1, 0), "plain")
        .apply_style(
            pos(1, 0),
            pos(1, 5),
            SpanStyle {
                bold: Some(false),
                ..Default::default()
            },
        )
        .mark_follows_text(&BlockPath::top(1));
    let p = typed.nth_paragraph(1).unwrap();
    assert_eq!(p.style_at(0).bold, Some(false));
    assert_eq!(p.mark_style.as_deref().and_then(|m| m.bold), Some(false));
    /* Agreement: unchanged (same tree). */
    let plain = d.insert_text(pos(1, 0), "bold");
    let again = plain.mark_follows_text(&BlockPath::top(1));
    assert_eq!(mark(again.nth_paragraph(1).unwrap()), Some(bold()));
}

/// The mark survives a crash-recovery snapshot; a paragraph without one
/// encodes no `mark_style` key (pre-#293 snapshots stay byte-stable).
#[test]
fn the_mark_rides_snapshots_and_an_absent_one_encodes_nothing() {
    let plain = DocumentTree::from_text("plain");
    let bytes = snapshot::encode(&plain).unwrap();
    assert!(
        !bytes.windows(10).any(|w| w == b"mark_style"),
        "no key for an absent mark"
    );
    let d = hello_bold_world().split_paragraph(pos(0, 11));
    let bytes = snapshot::encode(&d).unwrap();
    let back: DocumentTree = snapshot::decode(&bytes).unwrap().payload;
    assert_eq!(mark(back.nth_paragraph(1).unwrap()), Some(bold()));
}
