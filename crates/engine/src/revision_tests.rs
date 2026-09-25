//! Issues #247 / #262 — tracked moves, paragraph-mark revisions and the
//! document-wide accept / reject.

use crate::{Block, BlockPath, DocumentTree, LogicalPos, Revision, RevisionKind};

fn pos(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

fn rev(kind: RevisionKind, start: u32, end: u32) -> Revision {
    Revision {
        start,
        end,
        kind,
        author: "A".into(),
        date: "d".into(),
        id: None,
        prev_attrs: None,
        move_name: matches!(kind, RevisionKind::MoveFrom | RevisionKind::MoveTo)
            .then(|| "m".to_string()),
    }
}

/// `texts[i]` in top-level paragraph `i`, carrying `revs[i]`.
fn doc_with(texts: &[&str], revs: &[Vec<Revision>]) -> DocumentTree {
    let mut d = DocumentTree::from_paragraphs(texts.iter().map(|t| t.to_string()));
    let mut blocks = d.blocks.clone();
    for (i, rs) in revs.iter().enumerate() {
        if let Block::Paragraph(p) = &mut blocks[i] {
            p.revisions = rs.clone();
        }
    }
    d.blocks = blocks;
    d
}

fn ranges(d: &DocumentTree, block: u32) -> Vec<(RevisionKind, u32, u32)> {
    d.nth_paragraph(block)
        .map(|p| {
            p.revisions
                .iter()
                .map(|r| (r.kind, r.start, r.end))
                .collect()
        })
        .unwrap_or_default()
}

/// Issue #247 — an untracked edit beside a tracked change moves the
/// change with its text (it used to stay at its old offsets and cover
/// the typed bytes instead).
#[test]
fn insert_text_carries_revisions_with_their_text() {
    let d = doc_with(&["ab moved cd"], &[vec![rev(RevisionKind::MoveTo, 3, 8)]]);
    /* Before: shift. */
    let before = d.insert_text(pos(0, 1), "XY");
    assert_eq!(ranges(&before, 0), vec![(RevisionKind::MoveTo, 5, 10)]);
    /* At the start: stays outside (shift). */
    let at_start = d.insert_text(pos(0, 3), "XY");
    assert_eq!(ranges(&at_start, 0), vec![(RevisionKind::MoveTo, 5, 10)]);
    /* Strictly inside: grows. */
    let inside = d.insert_text(pos(0, 5), "XY");
    assert_eq!(ranges(&inside, 0), vec![(RevisionKind::MoveTo, 3, 10)]);
    /* At the end: stays outside. */
    let at_end = d.insert_text(pos(0, 8), "XY");
    assert_eq!(ranges(&at_end, 0), vec![(RevisionKind::MoveTo, 3, 8)]);
    assert_eq!(at_end.paragraph_text(0), Some("ab movedXY cd"));
}

/// Issue #247 — accept keeps a move's destination and drops its source;
/// reject the reverse.
#[test]
fn move_accept_keeps_destination_reject_keeps_source() {
    let d = doc_with(
        &["moved stay moved"],
        &[vec![
            rev(RevisionKind::MoveFrom, 0, 5),
            rev(RevisionKind::MoveTo, 11, 16),
        ]],
    );
    let accepted = d.accept_revision_at(0, 11, 16).accept_revision_at(0, 0, 5);
    assert_eq!(accepted.paragraph_text(0), Some(" stay moved"));
    assert!(ranges(&accepted, 0).is_empty());
    let rejected = d.reject_revision_at(0, 11, 16).reject_revision_at(0, 0, 5);
    assert_eq!(rejected.paragraph_text(0), Some("moved stay "));
    assert!(ranges(&rejected, 0).is_empty());
}

#[test]
fn revision_kind_outcomes() {
    use RevisionKind::*;
    for (kind, on_accept, on_reject) in [
        (Insert, false, true),
        (Delete, true, false),
        (MoveFrom, true, false),
        (MoveTo, false, true),
        (FormatChange, false, false),
    ] {
        assert_eq!(kind.removes_text(true), on_accept, "{kind:?} accept");
        assert_eq!(kind.removes_text(false), on_reject, "{kind:?} reject");
        assert_eq!(kind.wraps_text(), kind != FormatChange);
    }
}
