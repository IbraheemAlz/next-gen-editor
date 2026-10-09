//! Issue #292 — paragraph merges (`Paragraph::concat`: Backspace at a
//! paragraph start, Delete at a paragraph end, cross-paragraph deletes,
//! the edges of a rich paste) keep the head's paragraph formatting, and
//! hyperlinks / tracked changes ride every merge, split and deletion
//! through one `TextEdit` record.

use crate::{
    Alignment, Block, BlockPath, DocumentTree, Hyperlink, ListItem, LogicalPos, ParaProperties,
    Paragraph, ParagraphStyle, Revision, RevisionKind, SourceAttr, SourceMarkup, SpanStyle,
    TextDirection, TextEdit, UndoStack, recompute_paragraph_props,
};

fn pos(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

fn heading_style() -> ParagraphStyle {
    ParagraphStyle {
        id: "Heading1".into(),
        name: "heading 1".into(),
        para: ParaProperties {
            keep_next: Some(true),
            outline_level: Some(0),
            ..Default::default()
        },
        run: SpanStyle {
            bold: Some(true),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn insertion(start: u32, end: u32) -> Revision {
    Revision {
        start,
        end,
        kind: RevisionKind::Insert,
        author: "Reviewer".into(),
        date: "2026-01-01T00:00:00Z".into(),
        id: Some(7),
        prev_attrs: None,
        move_name: None,
    }
}

fn link(start: u32, end: u32) -> Hyperlink {
    Hyperlink {
        start,
        end,
        target: "https://example.com".into(),
        ..Default::default()
    }
}

/// `paras` as top-level blocks of a tree whose style table holds
/// Heading1 (with resolved props recomputed per paragraph).
fn doc(paras: Vec<Paragraph>) -> DocumentTree {
    let mut d = DocumentTree::new();
    d.styles.insert("Heading1".into(), heading_style());
    d.blocks = paras
        .into_iter()
        .map(|mut p| {
            recompute_paragraph_props(&mut p, &d.styles, &d.style_defaults);
            Block::Paragraph(p)
        })
        .collect();
    d
}

/// A centred, RTL, list-bound Heading1 "Title".
fn heading(text: &str) -> Paragraph {
    Paragraph {
        text: text.into(),
        style_id: Some("Heading1".into()),
        direct_overrides: ParaProperties {
            alignment: Some(Alignment::Center),
            direction: Some(TextDirection::Rtl),
            ..Default::default()
        },
        list_item: Some(ListItem { num_id: 3, ilvl: 1 }),
        ..Default::default()
    }
}

/// "Body text": a tracked insertion on "Body", a hyperlink on "text".
fn body() -> Paragraph {
    Paragraph {
        text: "Body text".into(),
        hyperlinks: vec![link(5, 9)],
        revisions: vec![insertion(0, 4)],
        ..Default::default()
    }
}

fn assert_heading(p: &Paragraph) {
    assert_eq!(p.style_id.as_deref(), Some("Heading1"), "{:?}", p.text);
    assert_eq!(p.direct_overrides.alignment, Some(Alignment::Center));
    assert_eq!(p.props.alignment, Some(Alignment::Center));
    assert_eq!(p.props.direction, Some(TextDirection::Rtl));
    assert_eq!(p.props.keep_next, Some(true), "the style cascade survives");
    assert_eq!(p.list_item, Some(ListItem { num_id: 3, ilvl: 1 }));
}

/// Backspace at the start of the paragraph after a heading (and Delete
/// at the heading's end — the same paragraph-break deletion) joins the
/// body onto the heading, which keeps its style, direct formatting,
/// direction and list binding.
#[test]
fn deleting_the_break_after_a_heading_keeps_the_heading() {
    let d = doc(vec![heading("Title"), body()]);
    let merged = d.delete_range(pos(0, 5), pos(1, 0));
    assert_eq!(merged.blocks.len(), 1);
    let p = merged.nth_paragraph(0).unwrap();
    assert_eq!(p.text, "TitleBody text");
    assert_heading(p);
    /* The undo snapshot of the merge passes the #250 markup check. */
    let mut undo = UndoStack::new(d, 10);
    undo.push(merged);
}

/// A deletion from inside the heading to inside a later paragraph keeps
/// the heading's formatting (the head survives), drops the middle block.
#[test]
fn a_cross_paragraph_delete_keeps_the_head_formatting() {
    let middle = Paragraph {
        text: "Middle".into(),
        ..Default::default()
    };
    let d = doc(vec![heading("Title"), middle, body()]);
    let merged = d.delete_range(pos(0, 2), pos(2, 5));
    assert_eq!(merged.blocks.len(), 1);
    let p = merged.nth_paragraph(0).unwrap();
    assert_eq!(p.text, "Titext");
    assert_heading(p);
}

/// Word: merging away an EMPTY paragraph imposes nothing — Backspace at
/// the start of a heading that follows an empty paragraph (or Delete in
/// the empty one) leaves the heading a heading; deleting a whole heading
/// and the start of the next paragraph leaves that paragraph's format.
#[test]
fn an_empty_head_merges_away_without_imposing_its_format() {
    let d = doc(vec![Paragraph::default(), heading("Title")]);
    let merged = d.delete_range(pos(0, 0), pos(1, 0));
    let p = merged.nth_paragraph(0).unwrap();
    assert_eq!(p.text, "Title");
    assert_heading(p);

    let d = doc(vec![heading("Title"), body()]);
    let merged = d.delete_range(pos(0, 0), pos(1, 5));
    let p = merged.nth_paragraph(0).unwrap();
    assert_eq!(p.text, "text");
    assert_eq!(p.style_id, None, "the heading was removed whole");
    assert_eq!(p.list_item, None);
    assert_eq!(p.props.alignment, None);
}

/// The second paragraph's hyperlink and tracked insertion survive the
/// merge, shifted onto their text — and are clipped, not dropped, by a
/// deletion that cuts into them.
#[test]
fn hyperlinks_and_revisions_survive_paragraph_merges() {
    let d = doc(vec![heading("Title"), body()]);
    let merged = d.delete_range(pos(0, 5), pos(1, 0));
    let p = merged.nth_paragraph(0).unwrap();
    assert_eq!(p.hyperlinks.len(), 1);
    let h = &p.hyperlinks[0];
    assert_eq!(&p.text[h.start as usize..h.end as usize], "text");
    assert_eq!(h.target, "https://example.com");
    assert_eq!(p.revisions.len(), 1);
    let r = &p.revisions[0];
    assert_eq!(&p.text[r.start as usize..r.end as usize], "Body");
    assert_eq!(
        (r.kind, r.author.as_str(), r.id),
        (RevisionKind::Insert, "Reviewer", Some(7))
    );

    /* "Tit|le" + "Bo|dy text": the insertion keeps "dy". */
    let merged = d.delete_range(pos(0, 3), pos(1, 2));
    let p = merged.nth_paragraph(0).unwrap();
    assert_eq!(p.text, "Titdy text");
    let r = &p.revisions[0];
    assert_eq!(&p.text[r.start as usize..r.end as usize], "dy");
    let h = &p.hyperlinks[0];
    assert_eq!(&p.text[h.start as usize..h.end as usize], "text");
    /* A head-side link survives too. */
    let mut head = heading("Title");
    head.hyperlinks.push(link(0, 3));
    let merged = doc(vec![head, body()]).delete_range(pos(0, 5), pos(1, 0));
    let p = merged.nth_paragraph(0).unwrap();
    assert_eq!(p.hyperlinks.len(), 2);
    assert_eq!((p.hyperlinks[0].start, p.hyperlinks[0].end), (0, 3));
}

/// A single-paragraph deletion (Backspace inside a paragraph) keeps a
/// hyperlink / revision it does not swallow, clipping one it cuts into.
#[test]
fn delete_text_remaps_hyperlinks_and_revisions() {
    let p = body().delete_text(2, 6);
    assert_eq!(p.text, "Boext");
    let r = &p.revisions[0];
    assert_eq!(&p.text[r.start as usize..r.end as usize], "Bo");
    let h = &p.hyperlinks[0];
    assert_eq!(&p.text[h.start as usize..h.end as usize], "ext");
    /* Swallowed whole: gone. */
    let p = body().delete_text(4, 9);
    assert!(p.hyperlinks.is_empty());
    assert_eq!(p.revisions.len(), 1);
}

/// Enter inside a hyperlink / tracked insertion continues it on both
/// halves; the right piece of a cut revision gives up the source `w:id`
/// (two wrappers must not share one).
#[test]
fn split_at_carries_straddling_overlays_to_both_halves() {
    let (l, r) = body().split_at(2);
    assert_eq!((l.text.as_str(), r.text.as_str()), ("Bo", "dy text"));
    assert_eq!((l.revisions[0].start, l.revisions[0].end), (0, 2));
    assert_eq!(l.revisions[0].id, Some(7));
    assert_eq!((r.revisions[0].start, r.revisions[0].end), (0, 2));
    assert_eq!(r.revisions[0].id, None);
    assert!(l.hyperlinks.is_empty());
    assert_eq!((r.hyperlinks[0].start, r.hyperlinks[0].end), (3, 7));
    /* A whole-side revision keeps its id. */
    let (_, r) = body().split_at(4);
    assert!(r.revisions.is_empty());
    let (l, _) = body().split_at(4);
    assert_eq!(l.revisions[0].id, Some(7));
    /* Split + concat is the identity on the overlays. */
    let (l, r) = body().split_at(7);
    let j = l.concat(&r);
    assert_eq!(j.hyperlinks, body().hyperlinks);
    assert_eq!(j.revisions, body().revisions);
}

/// A paste lands between the halves of the target paragraph: the
/// single-paragraph paste keeps the target's style (it used to clear
/// it); in a multi-paragraph paste the first piece keeps the target's
/// style and the tail piece the last pasted paragraph's (the left side
/// of each merge), never nothing.
#[test]
fn rich_paste_edges_keep_a_paragraph_style() {
    let mut d = doc(vec![heading("Title")]);
    d.styles.insert(
        "Quote".into(),
        ParagraphStyle {
            id: "Quote".into(),
            name: "Quote".into(),
            ..Default::default()
        },
    );
    let plain = Paragraph {
        text: "X".into(),
        ..Default::default()
    };
    let (one, _) = d.insert_rich(pos(0, 2), std::slice::from_ref(&plain));
    let p = one.nth_paragraph(0).unwrap();
    assert_eq!(p.text, "TiXtle");
    assert_heading(p);

    let quote = |t: &str| Paragraph {
        text: t.into(),
        style_id: Some("Quote".into()),
        ..Default::default()
    };
    let (multi, caret) = d.insert_rich(pos(0, 2), &[quote("A"), quote("B")]);
    assert_eq!(multi.paragraph_text(0), Some("TiA"));
    assert_heading(multi.nth_paragraph(0).unwrap());
    let tail = multi.nth_paragraph(1).unwrap();
    assert_eq!(tail.text, "Btle");
    assert_eq!(tail.style_id.as_deref(), Some("Quote"));
    assert_eq!(caret, pos(1, 1));

    /* Pasting into an EMPTY paragraph: the pasted heading stays one. */
    let empty = doc(vec![Paragraph::default()]);
    /* A clipboard fragment carries resolved props. */
    let mut head = heading("Head");
    recompute_paragraph_props(&mut head, &empty.styles, &empty.style_defaults);
    let (pasted, _) = empty.insert_rich(pos(0, 0), &[head, quote("Q")]);
    assert_heading(pasted.nth_paragraph(0).unwrap());
    assert_eq!(
        pasted.nth_paragraph(1).unwrap().style_id.as_deref(),
        Some("Quote")
    );
}

fn identity(para_id: &str, len: u32) -> Option<Box<SourceMarkup>> {
    Some(Box::new(SourceMarkup {
        text_len: len,
        attrs: vec![SourceAttr {
            name: "w14:paraId".into(),
            value: para_id.into(),
            ws: None,
        }],
        ..SourceMarkup::default()
    }))
}

/// Issue #199 — the merged paragraph keeps the head's source identity
/// (left ids win); an empty head hands over the tail's.
#[test]
fn the_merge_keeps_the_left_source_identity() {
    let mut a = heading("Title");
    a.source_markup = identity("AAAA0001", 5);
    let mut b = body();
    b.source_markup = identity("BBBB0002", 9);
    let paraid = |p: &Paragraph| p.source_markup.as_deref().unwrap().attrs[0].value.clone();
    assert_eq!(paraid(&a.concat(&b)), "AAAA0001");
    let e = Paragraph {
        source_markup: identity("EEEE0003", 0),
        ..Default::default()
    };
    assert_eq!(paraid(&e.concat(&b)), "BBBB0002");
}

#[test]
fn text_edit_maps_ranges_like_the_text() {
    let ins = TextEdit {
        at: 4,
        removed: 0,
        inserted: 2,
    };
    assert_eq!(ins.map_range(4, 8), Some((6, 10)), "at the start: outside");
    assert_eq!(ins.map_range(0, 4), Some((0, 4)), "at the end: outside");
    assert_eq!(ins.map_range(2, 6), Some((2, 8)), "strictly inside: grows");
    let del = TextEdit {
        at: 4,
        removed: 3,
        inserted: 0,
    };
    assert_eq!(del.map_range(0, 5), Some((0, 4)));
    assert_eq!(del.map_range(5, 10), Some((4, 7)));
    assert_eq!(del.map_range(4, 7), None, "swallowed whole");
    assert_eq!(del.map_range(8, 9), Some((5, 6)));
    let rep = TextEdit {
        at: 4,
        removed: 3,
        inserted: 5,
    };
    assert_eq!(
        rep.map_range(0, 5),
        Some((0, 9)),
        "end inside: covers the replacement"
    );
    assert_eq!(rep.map_range(5, 10), Some((4, 12)));
}
