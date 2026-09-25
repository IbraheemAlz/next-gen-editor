//! Issue #278 — note references outside top-level body paragraphs.
//!
//! `DocumentTree::note_references` walks every story container that
//! paints — table cells, text-box stories, header / footer parts — so a
//! reference there is numbered in document order with the body's, laid
//! out in the page's band (a cell's note on the page the cell lands on, a
//! box's on the page its anchor lands on, a header's on the first page
//! showing the header), mirrored to accessibility and exported.

use super::*;

const SENTINEL: char = '\u{FFFC}';

fn para(text: &str, refs: &[(u32, engine::InlineKind)]) -> engine::Paragraph {
    engine::Paragraph {
        text: text.to_string(),
        inline_objects: refs
            .iter()
            .map(|(at, kind)| engine::InlineObject {
                at: *at,
                kind: kind.clone(),
                anchor: None,
                source_xml: None,
            })
            .collect(),
        ..Default::default()
    }
}

fn footnote_ref(id: u32) -> engine::InlineKind {
    engine::InlineKind::FootnoteRef {
        id,
        custom_mark_follows: false,
    }
}

fn endnote_ref(id: u32) -> engine::InlineKind {
    engine::InlineKind::EndnoteRef {
        id,
        custom_mark_follows: false,
    }
}

/// A paragraph `"<head>\u{FFFC}<tail>"` whose sentinel is `kind`.
fn para_with(head: &str, kind: engine::InlineKind, tail: &str) -> engine::Paragraph {
    para(
        &format!("{head}{SENTINEL}{tail}"),
        &[(head.len() as u32, kind)],
    )
}

/// Word's note body shape: the self-mark, a space, the text.
fn note_story(kind: engine::NoteKind, id: i32, text: &str) -> engine::NoteStory {
    engine::NoteStory {
        id,
        kind,
        note_type: engine::NoteType::Normal,
        body: vec![engine::Block::Paragraph(para(
            &format!("{SENTINEL} {text}"),
            &[(0, engine::InlineKind::NoteSelfRef { kind })],
        ))],
        source_xml: None,
        dirty: false,
    }
}

fn add_footnote(d: &mut DocumentTree, id: u32, text: &str) {
    d.footnote_stories.insert(
        id as i32,
        note_story(engine::NoteKind::Footnote, id as i32, text),
    );
}

fn one_cell_table(p: engine::Paragraph) -> engine::Table {
    engine::Table {
        rows: vec![engine::TableRow {
            cells: vec![engine::TableCell {
                blocks: vec![engine::Block::Paragraph(p)],
                ..Default::default()
            }],
            ..Default::default()
        }],
        dirty: true,
        ..Default::default()
    }
}

fn inline_text_box(story_para: engine::Paragraph) -> engine::InlineKind {
    engine::InlineKind::TextBox {
        width_emu: 2 * 914_400,
        height_emu: 914_400,
        story: Box::new(engine::TextBoxStory {
            body: vec![engine::Block::Paragraph(story_para)],
            ..Default::default()
        }),
    }
}

/// Page 1: a body reference (footnote 1) then a FORM FEED. Page 2: a
/// one-cell table whose cell references footnote 2, then a paragraph
/// hosting an in-line text box whose story references footnote 3.
fn cell_and_box_doc() -> DocumentTree {
    let mut d = DocumentTree::new();
    d.blocks = vec![
        engine::Block::Paragraph(para_with("Body", footnote_ref(1), " text.\u{000C}")),
        engine::Block::Table(one_cell_table(para_with("Cell", footnote_ref(2), " text"))),
        engine::Block::Paragraph(para_with(
            "Host ",
            inline_text_box(para_with("Box", footnote_ref(3), " text")),
            " after",
        )),
    ]
    .into();
    add_footnote(&mut d, 1, "Body note.");
    add_footnote(&mut d, 2, "Cell note.");
    add_footnote(&mut d, 3, "Box note.");
    d
}

/// A default header referencing footnote 7 and endnote 9; page 1's body
/// references footnote 8 and ends in a FORM FEED; page 2 carries plain
/// text (the header repeats there, its note does not).
fn header_note_doc() -> DocumentTree {
    let mut d = DocumentTree::new();
    d.blocks = vec![
        engine::Block::Paragraph(para_with("Page one", footnote_ref(8), ".\u{000C}")),
        engine::Block::Paragraph(para("Page two.", &[])),
    ]
    .into();
    let header = para(
        &format!("Header{SENTINEL} and{SENTINEL}"),
        &[(6, footnote_ref(7)), (6 + 3 + 4, endnote_ref(9))],
    );
    d.headers
        .insert("rIdH".to_string(), vec![engine::Block::Paragraph(header)]);
    d.body_section.header_refs.default = Some("rIdH".to_string());
    add_footnote(&mut d, 7, "Header note.");
    add_footnote(&mut d, 8, "Body note.");
    d.endnote_stories.insert(
        9,
        note_story(engine::NoteKind::Endnote, 9, "Header endnote."),
    );
    d
}

fn fn_anchor(id: u32) -> engine::NoteAnchor {
    engine::NoteAnchor {
        kind: engine::NoteKind::Footnote,
        id,
    }
}

/// Every reference mark shaped into `blocks` (cells and text-box
/// sentinels excluded — the story paints separately), in line order.
fn marks_in(blocks: &[LayoutBlock]) -> Vec<String> {
    let mut out = Vec::new();
    layout::boxes::for_each_paragraph_in_blocks(blocks, &mut |p: &ParagraphBox| {
        for g in p
            .lines
            .iter()
            .flat_map(|l| l.runs.iter())
            .flat_map(|r| r.glyphs.iter())
        {
            if g.inline_note_anchor.is_some()
                && let Some(m) = &g.inline_footnote_marker
            {
                out.push(m.clone());
            }
        }
    });
    out
}

fn band(page: &PageBox) -> Vec<(u32, String)> {
    page.footnotes
        .entries
        .iter()
        .map(|e| (e.id, e.marker.clone()))
        .collect()
}

#[test]
fn note_references_walk_cells_and_text_boxes_in_document_order() {
    let d = cell_and_box_doc();
    let refs = d.note_references();
    let got: Vec<(u32, u32, engine::NoteContainer)> = refs
        .iter()
        .map(|r| (r.anchor.id, r.top_block, r.container))
        .collect();
    assert_eq!(
        got,
        vec![
            (1, 0, engine::NoteContainer::Body),
            (2, 1, engine::NoteContainer::TableCell),
            (3, 2, engine::NoteContainer::TextBox),
        ]
    );
    let markers = d.note_markers();
    for (id, want) in [(1, "1"), (2, "2"), (3, "3")] {
        assert_eq!(markers.get(&fn_anchor(id)).map(String::as_str), Some(want));
    }
}

#[test]
fn a_header_reference_numbers_ahead_of_its_sections_body() {
    let d = header_note_doc();
    let got: Vec<(u32, engine::NoteContainer)> = d
        .note_references()
        .iter()
        .map(|r| (r.anchor.id, r.container))
        .collect();
    assert_eq!(
        got,
        vec![
            (7, engine::NoteContainer::Header),
            (9, engine::NoteContainer::Header),
            (8, engine::NoteContainer::Body),
        ]
    );
    let markers = d.note_markers();
    assert_eq!(markers[&fn_anchor(7)], "1");
    assert_eq!(markers[&fn_anchor(8)], "2");
    /* Endnotes run their own sequence from 1. */
    assert_eq!(
        markers[&engine::NoteAnchor {
            kind: engine::NoteKind::Endnote,
            id: 9
        }],
        "1"
    );
}

/// A header part no section paints still keeps its note on save.
#[test]
fn unpainted_header_references_stay_on_the_writers_keep_list() {
    let mut d = header_note_doc();
    d.body_section.header_refs.default = None;
    d.body_section.header_refs.first = Some("rIdH".to_string());
    /* `first` without `titlePg` never paints: not numbered… */
    assert!(
        d.note_references()
            .iter()
            .all(|r| r.container == engine::NoteContainer::Body)
    );
    /* …but the writer keeps the note the part points at. */
    assert!(d.all_note_reference_anchors().contains(&fn_anchor(7)));
}

/// Acceptance (#278): the cell's note is reserved on the page the cell
/// lands on (page 2) and numbered "2"; the text box's note on the page
/// its anchor lands on, numbered "3" — in the band, in the cell's
/// reference mark and in the box story's reference mark. Pinned.
#[test]
fn cell_and_text_box_notes_land_in_their_pages_band() {
    let engine = tests::test_engine_with_doc(cell_and_box_doc());
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    assert_eq!(pages.len(), 2, "the FORM FEED breaks after block 0");
    assert_eq!(band(&pages[0]), vec![(1, "1".to_string())]);
    assert_eq!(
        band(&pages[1]),
        vec![(2, "2".to_string()), (3, "3".to_string())],
        "cell + box notes open page 2's band"
    );
    assert_eq!(marks_in(&pages[1].blocks), vec!["2".to_string()]);
    let frame = pages[1]
        .floats
        .iter()
        .find_map(|f| f.text_box.as_deref())
        .expect("the text box frame");
    assert_eq!(
        marks_in(&frame.blocks),
        vec!["3".to_string()],
        "the box story shapes its document-order label"
    );
    /* The band sits below the body: no overlap. */
    let body_bottom = pages[1]
        .blocks
        .iter()
        .map(|b| b.origin().y + b.size().height)
        .fold(0.0_f32, f32::max)
        + pages[1].margins.top;
    assert!(pages[1].footnotes.y >= body_bottom);
    let fp = layout::geometry_fingerprint(&pages);
    eprintln!("NOTE CONTAINER FINGERPRINT cell_and_box = {fp:#x}");
    assert_eq!(fp, PINNED_NOTE_CELL_AND_BOX, "cell/box note fixture moved");
}

const PINNED_NOTE_CELL_AND_BOX: u64 = 0x962426db020f8674;

/// Acceptance (#278): a header footnote is placed once, in the band of
/// the first page showing the header, ahead of that page's body note;
/// the header's endnote trails the document with the others. Pinned.
#[test]
fn header_footnote_lands_once_on_the_first_page_showing_the_header() {
    let engine = tests::test_engine_with_doc(header_note_doc());
    let (pages, _, _, info) = engine.build_pages(1.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    assert_eq!(pages.len(), 2);
    assert_eq!(
        band(&pages[0]),
        vec![(7, "1".to_string()), (8, "2".to_string())]
    );
    assert!(
        band(&pages[1]).is_empty(),
        "the header repeats, its note not"
    );
    for page in &pages {
        let header = page.header.as_ref().expect("header band");
        assert_eq!(
            marks_in(&header.blocks),
            vec!["1".to_string(), "1".to_string()]
        );
    }
    let last = pages.last().expect("pages");
    assert_eq!(
        last.endnotes
            .entries
            .iter()
            .map(|e| e.id)
            .collect::<Vec<_>>(),
        vec![9],
        "the header's endnote trails the document"
    );
    let fp = layout::geometry_fingerprint(&pages);
    eprintln!("NOTE CONTAINER FINGERPRINT header = {fp:#x}");
    assert_eq!(fp, PINNED_NOTE_HEADER, "header note fixture moved");
}

const PINNED_NOTE_HEADER: u64 = 0x9009be57586d03e5;

/// A header note spans sections: a NextPage section break builds a fresh
/// paginator, which must not place the note a second time.
#[test]
fn a_header_note_is_not_repeated_by_the_next_sections_paginator() {
    let mut d = header_note_doc();
    let sect = d.body_section.clone();
    if let Some(engine::Block::Paragraph(p)) = d.blocks.get_mut(0) {
        p.section_end = Some(Box::new(sect));
    }
    let engine = tests::test_engine_with_doc(d);
    let (pages, ..) = engine.build_pages(1.0, false, None).expect("layout");
    let placed: Vec<u32> = pages
        .iter()
        .flat_map(|p| p.footnotes.entries.iter().map(|e| e.id))
        .collect();
    assert_eq!(placed, vec![7, 8], "{placed:?}");
}

#[test]
fn cell_box_and_header_references_are_mirrored_to_accessibility() {
    let mut d = cell_and_box_doc();
    let h = header_note_doc();
    d.headers = h.headers.clone();
    d.body_section.header_refs = h.body_section.header_refs.clone();
    for id in [7, 8] {
        add_footnote(&mut d, id, "x");
    }
    d.endnote_stories = h.endnote_stories.clone();
    let engine = tests::test_engine_with_doc(d);
    let nodes = engine.build_a11y_nodes();
    let mut refs: Vec<String> = Vec::new();
    let mut regions: Vec<String> = Vec::new();
    fn walk(ns: &[A11yNode], refs: &mut Vec<String>, regions: &mut Vec<String>) {
        for n in ns {
            match n {
                A11yNode::Paragraph(p) => {
                    for r in &p.runs {
                        if let Some(nr) = &r.note_ref {
                            refs.push(format!("{}={}", nr.id, r.text));
                        }
                    }
                }
                A11yNode::Table(t) => {
                    for row in &t.rows {
                        for c in &row.cells {
                            walk(&c.nodes, refs, regions);
                        }
                    }
                }
                A11yNode::TextBox(b) => walk(&b.nodes, refs, regions),
                A11yNode::Story(s) => walk(&s.nodes, refs, regions),
                A11yNode::Note(note) => regions.push(note.id.clone()),
            }
        }
    }
    walk(&nodes, &mut refs, &mut regions);
    /* Numbering: header (7 → 1) first, then body 1, cell 2, box 3. */
    assert_eq!(
        refs,
        vec![
            "footnote-1=2",
            "footnote-2=3",
            "footnote-3=4",
            "footnote-7=1",
            "endnote-9=1",
        ],
        "{nodes:#?}"
    );
    for id in [
        "footnote-1",
        "footnote-2",
        "footnote-3",
        "footnote-7",
        "endnote-9",
    ] {
        assert!(regions.iter().any(|r| r == id), "{id} region: {regions:?}");
    }
}

#[test]
fn html_export_links_cell_box_and_header_references() {
    let mut d = cell_and_box_doc();
    let h = header_note_doc();
    d.headers = h.headers.clone();
    d.body_section.header_refs = h.body_section.header_refs.clone();
    add_footnote(&mut d, 7, "Header note.");
    let html = format_html::to_html(&d);
    for id in ["footnote-1", "footnote-2", "footnote-3", "footnote-7"] {
        assert!(
            html.contains(&format!("href=\"#{id}\"")),
            "{id} reference link missing: {html}"
        );
        assert!(
            html.contains(&format!("<aside role=\"doc-footnote\" id=\"{id}\">")),
            "{id} note region missing: {html}"
        );
    }
    /* Document order: the header's note is number 1. */
    assert!(html.contains("<aside role=\"doc-footnote\" id=\"footnote-7\"><sup>1</sup>"));
    assert!(html.contains("<aside role=\"doc-footnote\" id=\"footnote-3\"><sup>4</sup>"));
}
