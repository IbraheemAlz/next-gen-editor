//! Issue #560 — every paragraph the ENGINE creates (a table's cells and
//! its escape paragraph, a new row / column, a split or merged cell's
//! placeholder, a text box, a note body, the first paragraph of an empty
//! container) is formatted through the document's ONE style cascade
//! (`StyleCascade::new_paragraph`): `props` = `docDefaults → the applied
//! paragraph style's chain` — `Normal` for a paragraph naming no style —
//! exactly what the reader gives a `<w:p>` with no `<w:pPr>`, so it lays
//! out now as it does after save + reopen. Nothing is direct.
//!
//! Issue #558 — the default CHARACTER style formats every run that names
//! no character style, between the paragraph style's `<w:rPr>` and the
//! run's own formatting (`StyleCascade::text_style`).

use super::*;

/// docDefaults: 8 pt after, 11 pt runs. `Normal` (default): RTL, 6 pt
/// before / 12 pt after, 14 pt runs. `FootnoteText` basedOn `Normal`:
/// 1 pt after, 10 pt runs.
fn doc() -> DocumentTree {
    let mut d = DocumentTree::from_text("body text");
    d.style_defaults.spacing.after_twips = 160;
    d.style_defaults.line_height = Some(LineHeight::Auto { twips: 259 });
    d.style_run_defaults.font_size = Some(11.0);
    let normal = ParagraphStyle {
        id: "Normal".into(),
        name: "Normal".into(),
        para: ParaProperties {
            direction: Some(TextDirection::Rtl),
            spacing: Spacing {
                before_twips: 120,
                after_twips: 240,
            },
            ..Default::default()
        },
        run: SpanStyle {
            font_size: Some(14.0),
            ..Default::default()
        },
        is_default: true,
        ..Default::default()
    };
    let footnote = ParagraphStyle {
        id: "FootnoteText".into(),
        name: "footnote text".into(),
        based_on: Some("Normal".into()),
        para: ParaProperties {
            spacing: Spacing {
                before_twips: 0,
                after_twips: 20,
            },
            ..Default::default()
        },
        run: SpanStyle {
            font_size: Some(10.0),
            ..Default::default()
        },
        ..Default::default()
    };
    d.styles.insert("Normal".into(), normal);
    d.styles.insert("FootnoteText".into(), footnote);
    /* The seed paragraph as the reader would have built it. */
    let mut blocks = d.blocks.clone();
    if let Some(Block::Paragraph(p)) = blocks.get_mut(0) {
        d.style_cascade().recompute(p, &d.style_defaults);
    }
    d.blocks = blocks;
    d
}

/// `p` is what the reader builds for a `<w:p>` with no `<w:pPr>` (or only
/// `<w:pStyle w:val=style>`) in `d`.
fn assert_minted(d: &DocumentTree, p: &Paragraph, style: Option<&str>, what: &str) {
    assert_eq!(p.style_id.as_deref(), style, "{what}: style");
    assert_eq!(
        p.props,
        d.resolve_style_cascade(style),
        "{what}: props are the cascade"
    );
    assert_eq!(
        p.direct_overrides,
        ParaProperties::default(),
        "{what}: nothing direct"
    );
}

fn cell_paragraph(d: &DocumentTree, table: u32, row: u32, col: u32) -> &Paragraph {
    let path = BlockPath::top(table)
        .push(PathStep::Cell { row, col })
        .push(PathStep::Block(0));
    d.paragraph_at_path(&path)
        .unwrap_or_else(|| panic!("no paragraph in cell ({row}, {col})"))
}

#[test]
fn a_new_paragraph_is_the_cascade_of_its_style() {
    let d = doc();
    let p = d.new_paragraph();
    assert_minted(&d, &p, None, "unstyled");
    /* Normal's spacing and bidi over the docDefaults' line rule. */
    assert_eq!(p.props.direction, Some(TextDirection::Rtl));
    assert_eq!(p.props.spacing.after_twips, 240);
    assert_eq!(p.props.line_height, Some(LineHeight::Auto { twips: 259 }));
    let f = d.new_paragraph_in_style(Some("FootnoteText".into()));
    assert_minted(&d, &f, Some("FootnoteText"), "FootnoteText");
    assert_eq!(f.props.spacing.after_twips, 20);
    assert_eq!(f.props.direction, Some(TextDirection::Rtl), "via Normal");
    /* Without a style table the cascade is empty: the old default. */
    let plain = DocumentTree::from_text("");
    assert_eq!(plain.new_paragraph().props, ParaProperties::default());
}

#[test]
fn an_inserted_table_its_cells_and_escape_paragraph_take_the_cascade() {
    let d = doc().insert_table(BlockPath::top(1), 2, 3);
    for row in 0..2 {
        for col in 0..3 {
            assert_minted(
                &d,
                cell_paragraph(&d, 1, row, col),
                None,
                &format!("cell ({row}, {col})"),
            );
        }
    }
    let Some(Block::Paragraph(escape)) = d.blocks.get(2) else {
        panic!("the escape paragraph after the table");
    };
    assert_minted(&d, escape, None, "escape paragraph");

    let d = d.insert_row(BlockPath::top(1), 2);
    let d = d.insert_column(BlockPath::top(1), 3);
    for col in 0..4 {
        assert_minted(&d, cell_paragraph(&d, 1, 2, col), None, "new row");
    }
    for row in 0..3 {
        assert_minted(&d, cell_paragraph(&d, 1, row, 3), None, "new column");
    }

    /* A vertical merge leaves the continuation cell an empty placeholder;
    splitting the owner again restores fresh cells. */
    let merged = d.merge_cells(BlockPath::top(1), 0, 0, 1, 1);
    assert_minted(
        &merged,
        cell_paragraph(&merged, 1, 1, 0),
        None,
        "placeholder",
    );
    let split = merged.split_cell(BlockPath::top(1), 0, 0);
    assert_minted(&split, cell_paragraph(&split, 1, 0, 1), None, "split cell");

    /* Issue #563 — a table inserted INTO a cell: its cells and the escape
    paragraph after it inside the outer cell take the cascade too. */
    let cell = BlockPath::top(1).push(PathStep::Cell { row: 0, col: 0 });
    let nested = d.insert_table(cell.clone().push(PathStep::Block(1)), 1, 2);
    let inner = cell.clone().push(PathStep::Block(1));
    for col in 0..2 {
        let path = inner
            .clone()
            .push(PathStep::Cell { row: 0, col })
            .push(PathStep::Block(0));
        let p = nested
            .paragraph_at_path(&path)
            .unwrap_or_else(|| panic!("nested cell (0, {col})"));
        assert_minted(&nested, p, None, "nested cell");
    }
    let escape = cell.push(PathStep::Block(2));
    let p = nested
        .paragraph_at_path(&escape)
        .expect("the escape paragraph after the nested table");
    assert_minted(&nested, p, None, "nested escape paragraph");
}

#[test]
fn a_text_box_note_body_and_empty_container_seed_take_the_cascade() {
    let d = doc();
    let (boxed, host, at) =
        d.insert_text_box_at(LogicalPos::new(BlockPath::top(0), 4), 914_400, 457_200);
    let Some(Block::Paragraph(host_para)) = boxed.block_at(&host) else {
        panic!("host paragraph");
    };
    let story = host_para
        .inline_objects
        .iter()
        .find_map(|o| match &o.kind {
            InlineKind::TextBox { story, .. } if o.at == at => Some(story),
            _ => None,
        })
        .expect("the text box");
    let Some(Block::Paragraph(seed)) = story.body.first() else {
        panic!("the box's paragraph");
    };
    assert_minted(&d, seed, None, "text box");

    let (noted, id) = d.insert_note_at(LogicalPos::new(BlockPath::top(0), 4), NoteKind::Footnote);
    let note = &noted.note_stories(NoteKind::Footnote)[&(id as i32)];
    let Some(Block::Paragraph(body)) = note.body.first() else {
        panic!("the note body");
    };
    assert_eq!(body.style_id.as_deref(), Some("FootnoteText"));
    assert_eq!(
        body.props,
        d.resolve_style_cascade(Some("FootnoteText")),
        "the note body lays out in FootnoteText now"
    );

    /* Typing into a container with no paragraph at all. */
    let mut empty = doc();
    empty.blocks = Vector::new();
    let typed = empty.insert_text(LogicalPos::new(BlockPath::top(0), 0), "x");
    let Some(Block::Paragraph(p)) = typed.blocks.get(0) else {
        panic!("the minted paragraph");
    };
    assert_eq!(p.text, "x");
    assert_minted(&typed, p, None, "first paragraph of an empty container");
}

/* ======================================================= issue #558 === */

/// `doc()` plus a default character style: bold, Liberation Serif-sized
/// 16 pt.
fn doc_with_default_char() -> DocumentTree {
    let mut d = doc();
    d.default_char_style = Some(SpanStyle {
        bold: Some(true),
        font_size: Some(16.0),
        ..Default::default()
    });
    d
}

#[test]
fn the_default_character_style_formats_runs_naming_none() {
    let d = doc_with_default_char();
    /* Text no span covers. */
    let plain = d.resolve_text_style(None, None);
    assert_eq!((plain.bold, plain.font_size), (Some(true), Some(16.0)));
    /* A span with only direct formatting: the direct value wins where it
    is set, the default character style elsewhere. */
    let direct = SpanStyle {
        bold: Some(false),
        ..Default::default()
    };
    let s = d.resolve_text_style(None, Some(&direct));
    assert_eq!((s.bold, s.font_size), (Some(false), Some(16.0)));
    /* A run naming a character style takes that style's chain (folded
    into the span by the reader) INSTEAD of the default one: the
    paragraph style's 14 pt shows through. */
    let named = SpanStyle {
        char_style: Some("Emphasis".into()),
        italic: Some(true),
        ..Default::default()
    };
    let s = d.resolve_text_style(None, Some(&named));
    assert_eq!(
        (s.bold, s.italic, s.font_size),
        (None, Some(true), Some(14.0))
    );
    /* It sits ABOVE the paragraph style's run formatting (ECMA-376
    §17.7.2: character style over paragraph style). */
    assert_eq!(
        d.resolve_text_style(Some("FootnoteText"), None).font_size,
        Some(16.0)
    );
    /* The paragraph baseline itself is unchanged (no character style). */
    assert_eq!(d.resolve_style_run_cascade(None).bold, None);
    /* Without one, text keeps the paragraph baseline. */
    assert_eq!(
        doc().resolve_text_style(None, None),
        doc().resolve_style_run_cascade(None)
    );
}

/// A pre-#558 snapshot (no key) reads no default character style, and a
/// tree without one encodes without the key.
#[test]
fn the_default_character_style_rides_snapshots_only_when_set() {
    let key = b"default_char_style";
    let has_key = |bytes: &[u8]| bytes.windows(key.len()).any(|w| w == key);
    let without = doc();
    let bytes = rmp_serde::to_vec_named(&without).expect("encode");
    assert!(!has_key(&bytes), "skipped when None");
    let back: DocumentTree = rmp_serde::from_slice(&bytes).expect("decode");
    assert_eq!(back.default_char_style, None);
    let with = doc_with_default_char();
    let bytes = rmp_serde::to_vec_named(&with).expect("encode");
    assert!(has_key(&bytes));
    let back: DocumentTree = rmp_serde::from_slice(&bytes).expect("decode");
    assert_eq!(back.default_char_style, with.default_char_style);
}
