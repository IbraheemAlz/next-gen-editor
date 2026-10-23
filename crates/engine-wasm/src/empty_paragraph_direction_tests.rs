//! Issues #552 / #507 — an empty (or neutral-only) paragraph takes its
//! direction from what the document DECLARES (its own / its style's
//! `<w:bidi>`, then `<w:docDefaults>`, then the layout config's base, which
//! the editor boots LTR), never from a hard-coded RTL; and Left / Right
//! that leave a table cell follow the TABLE's direction (`<w:bidiVisual>`),
//! whatever the cell paragraph resolved to.
//!
//! These run on `Engine::new_headless`, i.e. on `RenderConfig::
//! shell_default` — the editor's own boot config — so they measure what
//! the user sees after Enter or a toolbar table insert.

use super::*;

fn top(idx: u32) -> BridgeBlockPath {
    BridgeBlockPath {
        steps: vec![BridgePathStep::Block { idx }],
    }
}

fn cell(table: u32, row: u32, col: u32) -> BridgeBlockPath {
    BridgeBlockPath {
        steps: vec![
            BridgePathStep::Block { idx: table },
            BridgePathStep::Cell { row, col },
            BridgePathStep::Block { idx: 0 },
        ],
    }
}

fn pos(path: BridgeBlockPath, offset: u32) -> BridgeLogicalPos {
    BridgeLogicalPos { path, offset }
}

fn place(e: &mut Engine, at: BridgeLogicalPos) {
    let evt = e.apply_sync(Command::SetSelection {
        range: BridgeLogicalRange {
            start: at.clone(),
            end: at.clone(),
        },
        caret: at,
    });
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
}

/// Press an arrow key; the caret position and its rectangle after it.
fn press(e: &mut Engine, direction: MoveDirection) -> (BridgeLogicalPos, BridgeRect) {
    let evt = e.apply_sync(Command::MoveCaret {
        direction,
        extend: false,
    });
    let Event::SelectionChanged { caret, .. } = evt else {
        panic!("MoveCaret {direction:?}: {evt:?}");
    };
    (
        e.selection.as_ref().expect("a selection").caret.clone(),
        caret,
    )
}

fn caret_rect(e: &mut Engine) -> BridgeRect {
    let at = e.selection.as_ref().expect("a selection").caret.clone();
    let evt = e.apply_sync(Command::SetSelection {
        range: BridgeLogicalRange {
            start: at.clone(),
            end: at.clone(),
        },
        caret: at,
    });
    let Event::SelectionChanged { caret, .. } = evt else {
        panic!("{evt:?}");
    };
    caret
}

fn line_of<'a>(geom: &'a [LineGeom], path: &BridgeBlockPath) -> &'a LineGeom {
    geom.iter()
        .find(|l| &l.path == path)
        .unwrap_or_else(|| panic!("no line for {path:?}"))
}

fn a11y_paragraph_dirs(nodes: &[A11yNode], out: &mut Vec<Direction>) {
    for n in nodes {
        match n {
            A11yNode::Paragraph(p) => out.push(p.resolved_direction),
            A11yNode::Table(t) => {
                for row in &t.rows {
                    for c in &row.cells {
                        a11y_paragraph_dirs(&c.nodes, out);
                    }
                }
            }
            _ => {}
        }
    }
}

fn a11y_dirs(e: &Engine) -> Vec<Direction> {
    let mut out = Vec::new();
    a11y_paragraph_dirs(&e.build_a11y_nodes(), &mut out);
    out
}

/// "Hello world", Enter at its end: the new empty paragraph.
fn hello_then_enter() -> Engine {
    let mut e = Engine::new_headless(DocumentTree::from_text("Hello world"));
    place(&mut e, pos(top(0), 11));
    let evt = e.apply_sync(Command::SplitParagraph { at: None });
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(
        e.selection.as_ref().unwrap().caret,
        pos(top(1), 0),
        "Enter parks the caret in the new paragraph"
    );
    e
}

/// The editor boots an LTR document (the seed text is Latin-first; the
/// boot config's base is what a paragraph with no strong character falls
/// back to).
#[test]
fn the_shell_boots_an_ltr_document() {
    assert_eq!(
        RenderConfig::shell_default().base_direction,
        ShapingDirection::Ltr
    );
}

/// Issue #552 (a) — an empty paragraph after an LTR paragraph is LTR: its
/// caret sits at the LEFT margin (where the Latin line starts), with the
/// line height of the paragraph mark's (Latin) face, and every read-back
/// — layout, caret direction, the a11y mirror, the toolbar — says LTR.
#[test]
fn an_empty_paragraph_after_an_ltr_paragraph_is_ltr_at_the_left_margin() {
    let mut e = hello_then_enter();
    let geom = e.document_geometry().expect("geometry");
    let hello = line_of(&geom, &top(0));
    let empty = line_of(&geom, &top(1));
    let left_margin = hello.slots.first().expect("Hello slots").x;
    assert!(
        (empty.start_x - left_margin).abs() < 0.5,
        "empty paragraph starts at {} (left margin {left_margin}, right edge {})",
        empty.start_x,
        empty.hit_left + empty.hit_width
    );
    let rect = caret_rect(&mut e);
    assert!(
        (rect.x - left_margin).abs() < 0.5,
        "caret x {} (left margin {left_margin})",
        rect.x
    );
    /* The paragraph mark's face sets the empty line: the same Latin face
    and size as the "Hello world" line, so the same caret height. */
    place(&mut e, pos(top(0), 0));
    let hello_rect = caret_rect(&mut e);
    assert!(
        (rect.h - hello_rect.h).abs() < 0.5,
        "empty caret h {} vs Latin caret h {}",
        rect.h,
        hello_rect.h
    );

    assert_eq!(e.paragraph_direction_at(&top(1)), ShapingDirection::Ltr);
    assert_eq!(a11y_dirs(&e), vec![Direction::Ltr, Direction::Ltr]);
    place(&mut e, pos(top(1), 0));
    let evt = e.apply_sync(Command::SetSelection {
        range: BridgeLogicalRange {
            start: pos(top(1), 0),
            end: pos(top(1), 0),
        },
        caret: pos(top(1), 0),
    });
    let Event::SelectionChanged {
        direction,
        paragraph_direction,
        ..
    } = evt
    else {
        panic!("{evt:?}");
    };
    assert_eq!(direction, Direction::Ltr, "the document direction");
    assert_eq!(paragraph_direction, Some(Direction::Ltr));
    let (pages, _, _, _) = e.build_pages(e.scale(), false, None).expect("layout");
    let dirs: Vec<ShapingDirection> = pages
        .iter()
        .flat_map(|p| p.blocks.iter())
        .filter_map(|b| b.as_paragraph().map(|p| p.direction))
        .collect();
    assert_eq!(dirs, vec![ShapingDirection::Ltr, ShapingDirection::Ltr]);
}

/// A paragraph of digits / punctuation only is laid out in the document
/// direction too (first-strong finds nothing).
#[test]
fn a_neutral_only_paragraph_is_ltr_in_an_ltr_document() {
    let mut e = hello_then_enter();
    let evt = e.apply_sync(Command::InsertText {
        at: None,
        text: "12345 !?".into(),
    });
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    assert_eq!(e.paragraph_direction_at(&top(1)), ShapingDirection::Ltr);
    assert_eq!(a11y_dirs(&e), vec![Direction::Ltr, Direction::Ltr]);
}

/// "Hello", then a toolbar-style 3 x 3 table after it (caret in cell
/// (0, 0), the trailing escape paragraph at `top(2)`).
fn hello_and_table() -> Engine {
    let mut e = Engine::new_headless(DocumentTree::from_text("Hello"));
    place(&mut e, pos(top(0), 5));
    let evt = e.apply_sync(Command::InsertTable {
        at: top(1),
        rows: 3,
        cols: 3,
    });
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(e.selection.as_ref().unwrap().caret, pos(cell(1, 0, 0), 0));
    e
}

/// Reading order of the 3 x 3 table, then the escape paragraph.
fn reading_order() -> Vec<BridgeLogicalPos> {
    let mut out = Vec::new();
    for r in 0..3 {
        for c in 0..3 {
            out.push(pos(cell(1, r, c), 0));
        }
    }
    out.push(pos(top(2), 0));
    out
}

/// Issue #552 (b) / #507 — Right walks an inserted table's EMPTY cells in
/// reading order (same row, then the next row, then out of the table),
/// Left walks back; each cell is LTR and its caret sits at the cell's left
/// edge, so the caret x grows along a row.
#[test]
fn right_and_left_walk_an_inserted_tables_empty_cells_in_reading_order() {
    let mut e = hello_and_table();
    assert!(
        a11y_dirs(&e).iter().all(|d| *d == Direction::Ltr),
        "every empty cell is LTR: {:?}",
        a11y_dirs(&e)
    );
    let order = reading_order();
    let geom = e.document_geometry().expect("geometry");
    for at in &order[..9] {
        let line = line_of(&geom, &at.path);
        assert!(
            (line.start_x - line.hit_left).abs() < line.hit_width / 2.0,
            "{:?}: start_x {} is not on the left half of [{}, {}]",
            at.path,
            line.start_x,
            line.hit_left,
            line.hit_left + line.hit_width
        );
    }

    let mut xs = vec![caret_rect(&mut e).x];
    for want in &order[1..] {
        let (got, rect) = press(&mut e, MoveDirection::Right);
        assert_eq!(&got, want, "Right");
        xs.push(rect.x);
    }
    for row in xs[..9].chunks(3) {
        assert!(
            row[0] < row[1] && row[1] < row[2],
            "caret x grows left to right along a row: {row:?}"
        );
    }
    for want in order[..9].iter().rev() {
        let (got, _) = press(&mut e, MoveDirection::Left);
        assert_eq!(&got, want, "Left");
    }
    let (got, _) = press(&mut e, MoveDirection::Left);
    assert_eq!(
        got,
        pos(top(0), 5),
        "Left from cell (0, 0) leaves the table"
    );
}

/// Issue #507 — leaving a cell follows the TABLE's direction, not the
/// direction the cell's paragraph resolved to: an RTL empty paragraph in
/// an LTR table still hands Right to the next cell.
#[test]
fn leaving_a_cell_follows_the_table_direction_not_the_paragraphs() {
    let mut e = hello_and_table();
    place(&mut e, pos(cell(1, 0, 1), 0));
    let evt = e.apply_sync(Command::SetParagraphDirection {
        range: BridgeLogicalRange {
            start: pos(cell(1, 0, 1), 0),
            end: pos(cell(1, 0, 1), 0),
        },
        direction: Direction::Rtl,
    });
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    assert_eq!(
        e.paragraph_direction_at(&cell(1, 0, 1)),
        ShapingDirection::Rtl
    );
    let (got, _) = press(&mut e, MoveDirection::Right);
    assert_eq!(got, pos(cell(1, 0, 2), 0), "Right in an LTR table");
    place(&mut e, pos(cell(1, 0, 1), 0));
    let (got, _) = press(&mut e, MoveDirection::Left);
    assert_eq!(got, pos(cell(1, 0, 0), 0), "Left in an LTR table");
}

/// A right-to-left table (`<w:bidiVisual>`: logical column 0 rightmost)
/// mirrors the mapping: Left is the next cell in reading order, Right the
/// previous one.
#[test]
fn a_right_to_left_table_mirrors_the_cell_exit() {
    let mut e = hello_and_table();
    let evt = e.apply_sync(Command::SetTableProperties {
        table_path: top(1),
        patch: bridge::TablePropertiesPatch {
            bidi_visual: Some(true),
        },
    });
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    place(&mut e, pos(cell(1, 0, 1), 0));
    let (got, _) = press(&mut e, MoveDirection::Left);
    assert_eq!(got, pos(cell(1, 0, 2), 0), "Left: next cell, visually left");
    let (got, _) = press(&mut e, MoveDirection::Right);
    assert_eq!(got, pos(cell(1, 0, 1), 0), "Right: previous cell");
    let (got, _) = press(&mut e, MoveDirection::Right);
    assert_eq!(got, pos(cell(1, 0, 0), 0));
}

fn open(bytes: Vec<u8>) -> Engine {
    let mut e = Engine::new_headless(DocumentTree::new());
    let evt = e.apply_sync(Command::OpenDocument {
        bytes,
        format: DocFormat::Docx,
        name: None,
        defaults: None,
        limits: None,
        password: None,
    });
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
    e
}

/// Enter at the end of an opened document's first paragraph: the new
/// empty paragraph (`top(1)`) inherits that paragraph's properties.
fn open_and_enter(bytes: Vec<u8>) -> Engine {
    let mut e = open(bytes);
    let len = e.undo.current().nth_paragraph(0).unwrap().text.len() as u32;
    place(&mut e, pos(top(0), len));
    let evt = e.apply_sync(Command::SplitParagraph { at: None });
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    e
}

/// Issue #552 (c) — an RTL document stays RTL: Enter at the end of the
/// `pPr_bidi_rtl.docx` paragraph (direct `<w:bidi/>`) opens an empty
/// paragraph that resolves RTL everywhere. (Its `<w:jc w:val="end"/>` puts
/// an RTL paragraph's caret on the LEFT, so the edge is checked on a
/// start-aligned document below.)
#[test]
fn an_rtl_fixtures_empty_paragraph_stays_rtl() {
    let e = open_and_enter(
        include_bytes!("../../format-docx/tests/fixtures/pPr_bidi_rtl.docx").to_vec(),
    );
    assert_eq!(e.paragraph_direction_at(&top(1)), ShapingDirection::Rtl);
    assert_eq!(a11y_dirs(&e), vec![Direction::Rtl, Direction::Rtl]);
}

/// The tier-a Arabic document (`<w:bidi/>`, justified): Enter opens an
/// empty RTL paragraph whose caret sits at the RIGHT margin.
#[test]
fn an_rtl_documents_empty_paragraph_sits_at_the_right_margin() {
    let mut e =
        open_and_enter(include_bytes!("../../../tests/corpus/tier-a/arabic-kashida.docx").to_vec());
    assert_eq!(e.paragraph_direction_at(&top(1)), ShapingDirection::Rtl);
    assert_eq!(a11y_dirs(&e), vec![Direction::Rtl, Direction::Rtl]);
    let geom = e.document_geometry().expect("geometry");
    let empty = line_of(&geom, &top(1));
    let right = empty.hit_left + empty.hit_width;
    assert!(
        (empty.start_x - right).abs() < 0.5,
        "an RTL empty paragraph starts at the right margin: {} in [{}, {right}]",
        empty.start_x,
        empty.hit_left,
    );
    let rect = caret_rect(&mut e);
    assert!(
        rect.x > empty.hit_left + empty.hit_width / 2.0,
        "caret x {} on the right half",
        rect.x
    );
}

/// A document whose `<w:docDefaults>` declare `<w:bidi/>` is an RTL
/// document: its read paragraphs resolve RTL, and so do the paragraphs
/// the engine mints in it (a toolbar table's empty cells, the escape
/// paragraph) — the document default, not the shell's LTR boot base.
#[test]
fn doc_defaults_bidi_makes_engine_minted_paragraphs_rtl() {
    let mut d = DocumentTree::from_text("");
    d.style_defaults.direction = Some(engine::TextDirection::Rtl);
    d.styles_dirty = true;
    let bytes = format_docx::build_minimal_docx(&d).expect("build");
    let reread = format_docx::read_docx(&bytes).expect("reread").document;
    assert_eq!(
        reread.style_defaults.direction,
        Some(engine::TextDirection::Rtl),
        "the fixture declares the direction on docDefaults"
    );
    let mut e = open(bytes);
    assert_eq!(e.paragraph_direction_at(&top(0)), ShapingDirection::Rtl);
    place(&mut e, pos(top(0), 0));
    let evt = e.apply_sync(Command::InsertTable {
        at: top(1),
        rows: 2,
        cols: 2,
    });
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    let dirs = a11y_dirs(&e);
    assert!(
        dirs.iter().all(|d| *d == Direction::Rtl),
        "every paragraph follows the RTL docDefaults: {dirs:?}"
    );
    assert_eq!(
        e.paragraph_direction_at(&cell(1, 1, 1)),
        ShapingDirection::Rtl
    );
    let Event::SelectionChanged { direction, .. } = e.apply_sync(Command::SetSelection {
        range: BridgeLogicalRange {
            start: pos(cell(1, 0, 0), 0),
            end: pos(cell(1, 0, 0), 0),
        },
        caret: pos(cell(1, 0, 0), 0),
    }) else {
        panic!("SetSelection");
    };
    assert_eq!(direction, Direction::Rtl, "the document direction");
}

/// Issue #556 — a document whose `<w:bidi/>` lives only on its DEFAULT
/// paragraph style (`Normal`) is an RTL document too: its empty unstyled
/// paragraph resolves RTL, Enter at its end opens another RTL paragraph,
/// typing Latin into it keeps it RTL, and a toolbar table's cells (minted
/// by the engine, no style) follow the document default.
#[test]
fn a_normal_style_bidi_makes_the_document_rtl() {
    let mut e = open(format_docx::test_fixtures::default_style_rtl_docx());
    assert_eq!(e.paragraph_direction_at(&top(1)), ShapingDirection::Rtl);
    place(&mut e, pos(top(1), 0));
    let evt = e.apply_sync(Command::SplitParagraph { at: None });
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(e.selection.as_ref().unwrap().caret, pos(top(2), 0));
    assert_eq!(e.paragraph_direction_at(&top(2)), ShapingDirection::Rtl);
    let evt = e.apply_sync(Command::InsertText {
        at: None,
        text: "Latin".into(),
    });
    let Event::SelectionChanged { direction, .. } = evt else {
        panic!("InsertText: {evt:?}");
    };
    assert_eq!(
        direction,
        Direction::Rtl,
        "Latin text in a Normal-RTL paragraph"
    );
    let dirs = a11y_dirs(&e);
    assert_eq!(
        dirs,
        [
            Direction::Rtl,
            Direction::Rtl,
            Direction::Rtl,
            Direction::Rtl,
            Direction::Rtl,
            Direction::Ltr,
            Direction::Ltr,
        ],
        "the inserted paragraph mirrors RTL; Plain and the direct LTR stay LTR"
    );
    let evt = e.apply_sync(Command::InsertTable {
        at: top(3),
        rows: 1,
        cols: 2,
    });
    assert!(matches!(evt, Event::SelectionChanged { .. }), "{evt:?}");
    assert_eq!(
        e.paragraph_direction_at(&cell(3, 0, 1)),
        ShapingDirection::Rtl,
        "an engine-minted cell takes the document default (docDefaults + Normal)"
    );
}
