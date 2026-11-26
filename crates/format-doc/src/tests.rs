//! Unit tests over `.doc` images built in memory by [`crate::test_builder`]
//! (no binary fixture is committed).

use engine::text_input::{LINE_BREAK, PAGE_BREAK};
use engine::{
    Alignment, Block, BorderStyle, CellWidth, DocumentTree, LineHeight, Paragraph, RowHeight,
    SectionType, SpanStyle, Table, TableCell, TableLayout, TextDirection, UnderlineStyle,
    VMergeRole, VerticalAlign,
};

use crate::test_builder::{
    DocSpec, Encoding, Para, PieceSpec, Tc, build, prc_data, prm0, prm1, run, sprm, streams,
};
use crate::{DocError, LegacyContent, NotImported, read_doc};

fn paragraphs(doc: &DocumentTree) -> Vec<&Paragraph> {
    doc.blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => p,
            Block::Table(_) => panic!("the importer makes no tables"),
        })
        .collect()
}

fn texts(doc: &DocumentTree) -> Vec<String> {
    paragraphs(doc).iter().map(|p| p.text.clone()).collect()
}

/// The span style covering byte `at` of `p` (default when none).
fn style_at(p: &Paragraph, at: u32) -> SpanStyle {
    p.spans
        .iter()
        .find(|s| s.start <= at && at < s.end)
        .map(|s| s.style.clone())
        .unwrap_or_default()
}

fn import(spec: &DocSpec) -> crate::DocImport {
    read_doc(&build(spec)).expect("the built document imports")
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

#[test]
fn compressed_text_paragraphs_and_a_bold_run() {
    let spec = DocSpec::new(vec![
        Para::new(
            vec![
                run("Hello ", Vec::new()),
                run("bold", sprm::bold(true)),
                run(" world", Vec::new()),
            ],
            Vec::new(),
        ),
        Para::plain("Second paragraph — with a dash"),
    ]);
    let out = import(&spec);
    let doc = &out.document;
    assert_eq!(
        texts(doc),
        vec!["Hello bold world", "Second paragraph — with a dash"]
    );
    let p = paragraphs(doc)[0];
    assert_eq!(p.spans.len(), 1, "{:?}", p.spans);
    assert_eq!((p.spans[0].start, p.spans[0].end), (6, 10));
    assert_eq!(p.spans[0].style.bold, Some(true));
    assert!(out.not_imported.is_empty(), "{:?}", out.not_imported);
    assert!(!out.has_page_setup);
    assert!(doc.source_package.is_none());
}

#[test]
fn unicode_pieces_carry_arabic_with_rtl_paragraph_and_run() {
    let arabic = "مرحبا بالعالم";
    let spec = DocSpec::new(vec![
        Para::new(
            vec![run(
                arabic,
                cat(&[
                    sprm::rtl_run(true),
                    sprm::bold_cs(true),
                    sprm::size_cs_half_points(28),
                ]),
            )],
            cat(&[sprm::rtl_paragraph(true), sprm::jc(0)]),
        ),
        Para::plain("Latin after"),
    ]);
    let out = import(&spec);
    let ps = paragraphs(&out.document);
    assert_eq!(ps[0].text, arabic);
    assert_eq!(ps[0].props.direction, Some(TextDirection::Rtl));
    assert_eq!(ps[0].direct_overrides.direction, Some(TextDirection::Rtl));
    assert_eq!(ps[0].props.alignment, Some(Alignment::Start));
    let s = style_at(ps[0], 0);
    assert!(
        s.forces_complex_script(),
        "the run is flagged <w:rtl/>: {s:?}"
    );
    assert_eq!(s.bold_cs, Some(true));
    assert_eq!(s.font_size_cs, Some(14.0));
    assert_eq!(ps[1].props.direction, None);
    assert_eq!(ps[1].text, "Latin after");
}

#[test]
fn paragraph_alignment_indents_and_spacing() {
    let spec = DocSpec::new(vec![
        Para::new(
            vec![run("centered", Vec::new())],
            cat(&[
                sprm::jc(1),
                sprm::indent_start(720),
                sprm::indent_end(360),
                sprm::indent_first_line(-360),
                sprm::space_before(240),
                sprm::space_after(120),
                sprm::line_spacing(360, true),
            ]),
        ),
        /* Physical right (sprmPJc80) in a right-to-left paragraph is its
        reading START. */
        Para::new(
            vec![run("rtl right", Vec::new())],
            cat(&[sprm::rtl_paragraph(true), sprm::jc80(2)]),
        ),
        Para::new(
            vec![run("exact", Vec::new())],
            cat(&[
                sprm::jc(3),
                sprm::line_spacing(-280, false),
                sprm::indent_first_line(200),
            ]),
        ),
        Para::new(
            vec![run("at least", Vec::new())],
            sprm::line_spacing(300, false),
        ),
    ]);
    let doc = import(&spec).document;
    let ps = paragraphs(&doc);
    let p = &ps[0].props;
    assert_eq!(p.alignment, Some(Alignment::Center));
    assert_eq!(p.indent.start_twips, 720);
    assert_eq!(p.indent.end_twips, 360);
    assert_eq!(p.indent.hanging_twips, 360);
    assert_eq!(p.indent.first_line_twips, 0);
    assert_eq!(p.spacing.before_twips, 240);
    assert_eq!(p.spacing.after_twips, 120);
    assert_eq!(p.line_height, Some(LineHeight::Auto { twips: 360 }));
    assert_eq!(ps[0].direct_overrides, ps[0].props);
    assert_eq!(ps[1].props.alignment, Some(Alignment::Start));
    assert_eq!(ps[2].props.alignment, Some(Alignment::Justify));
    assert_eq!(
        ps[2].props.line_height,
        Some(LineHeight::Exact { twips: 280 })
    );
    assert_eq!(ps[2].props.indent.first_line_twips, 200);
    assert_eq!(
        ps[3].props.line_height,
        Some(LineHeight::AtLeast { twips: 300 })
    );
    assert_eq!(ps[3].props.alignment, None);
}

#[test]
fn character_formatting_italic_underline_strike_size_colour() {
    let spec = DocSpec::new(vec![Para::new(
        vec![
            run("i", sprm::italic(true)),
            run("u", sprm::underline(3)),
            run("s", sprm::strike(true)),
            run("z", sprm::size_half_points(48)),
            run("r", sprm::ico(6)),
            run("c", sprm::color(0x12, 0x34, 0x56)),
            run("w", sprm::underline(11)),
        ],
        Vec::new(),
    )]);
    let doc = import(&spec).document;
    let p = paragraphs(&doc)[0];
    assert_eq!(p.text, "iuszrcw");
    assert_eq!(style_at(p, 0).italic, Some(true));
    assert_eq!(style_at(p, 1).underline, Some(UnderlineStyle::Double));
    assert_eq!(style_at(p, 2).strike, Some(true));
    assert_eq!(style_at(p, 3).font_size, Some(24.0));
    assert_eq!(style_at(p, 4).color, Some([255, 0, 0, 255]));
    assert_eq!(style_at(p, 5).color, Some([0x12, 0x34, 0x56, 255]));
    assert_eq!(style_at(p, 6).underline, Some(UnderlineStyle::Wavy));
    assert_eq!(p.spans.len(), 7);
}

/// A fast-saved document: the pieces are stored out of physical order,
/// mix 8-bit and 16-bit text, a paragraph spans three pieces, and two
/// pieces carry property modifiers (a Prm0 bold, a Prm1 grpprl with italic
/// and a paragraph justification) on top of the FKP formatting.
#[test]
fn a_fast_saved_document_reads_through_its_piece_table() {
    let spec = DocSpec {
        pieces: vec![
            /* "Hello " */
            PieceSpec {
                cps: 6,
                encoding: Encoding::Compressed,
                prm: 0,
            },
            /* "café " — UTF-16, bold by its Prm0 */
            PieceSpec {
                cps: 5,
                encoding: Encoding::Utf16,
                prm: prm0(0x55, 1),
            },
            /* "world\r" — italic + centered by its Prm1 */
            PieceSpec {
                cps: 6,
                encoding: Encoding::Compressed,
                prm: prm1(0),
            },
            /* "two\r" */
            PieceSpec {
                cps: 4,
                encoding: Encoding::Utf16,
                prm: 0,
            },
        ],
        physical_order: vec![3, 1, 2, 0],
        prcs: vec![cat(&[sprm::italic(true), sprm::jc(1)])],
        fast_saved: true,
        ..DocSpec::new(vec![
            Para::new(vec![run("Hello café world", Vec::new())], Vec::new()),
            Para::new(vec![run("two", Vec::new())], sprm::jc(2)),
        ])
    };
    let doc = import(&spec).document;
    let ps = paragraphs(&doc);
    assert_eq!(texts(&doc), vec!["Hello café world", "two"]);
    let p = ps[0];
    assert_eq!(style_at(p, 0), SpanStyle::default(), "piece 0 has no Prm");
    let cafe = p.text.find("café").expect("café") as u32;
    assert_eq!(style_at(p, cafe).bold, Some(true), "Prm0 bold on piece 1");
    let world = p.text.find("world").expect("world") as u32;
    let s = style_at(p, world);
    assert_eq!(
        (s.italic, s.bold),
        (Some(true), None),
        "Prm1 italic on piece 2"
    );
    /* The mark of paragraph 0 is in piece 2: its Prm1 justification
    applies to the paragraph. */
    assert_eq!(p.props.alignment, Some(Alignment::Center));
    assert_eq!(ps[1].props.alignment, Some(Alignment::End));
}

#[test]
fn every_section_brings_its_page_setup() {
    let spec = DocSpec {
        sections: vec![
            /* Section 1 ends after "one\x0C" (CP 4): landscape Letter. */
            (
                4,
                cat(&[
                    sprm::page_size(15840, 12240),
                    sprm::margins(1440, 1440, 720, 720),
                ]),
            ),
            /* Section 2 (the last) — A4, continuous; no margin sprms, so
            the SEP defaults (1.25 in sides, 1 in top / bottom) apply. */
            (
                8,
                cat(&[sprm::page_size(11906, 16838), sprm::section_start(0)]),
            ),
        ],
        ..DocSpec::new(vec![
            Para {
                mark: '\u{C}',
                ..Para::plain("one")
            },
            Para::plain("two"),
        ])
    };
    let out = import(&spec);
    assert!(out.has_page_setup);
    let doc = &out.document;
    assert_eq!(texts(doc), vec!["one", "two"]);
    let ps = paragraphs(doc);
    let first = ps[0]
        .section_end
        .as_deref()
        .expect("section 1 ends on paragraph 1");
    assert_eq!(
        (first.geometry.width, first.geometry.height),
        (792.0, 612.0)
    );
    assert_eq!(first.geometry.margin_left, 72.0);
    assert_eq!(first.geometry.margin_top, 36.0);
    assert!(
        ps[1].section_end.is_none(),
        "the last section is the body section"
    );
    let body = &doc.body_section;
    assert_eq!((body.geometry.width, body.geometry.height), (595.3, 841.9));
    assert_eq!(body.geometry.margin_left, 90.0);
    assert_eq!(body.geometry.margin_top, 72.0);
    assert_eq!(body.section_type, SectionType::Continuous);
    assert_eq!(engine::first_point_field_beyond_bound(doc), None);
}

#[test]
fn out_of_range_measures_are_held_to_word_bounds() {
    let spec = DocSpec {
        sections: vec![(
            2,
            cat(&[
                sprm::page_size(60000, 50),
                sprm::margins(40000, 40000, 30000, -30000),
            ]),
        )],
        ..DocSpec::new(vec![Para::new(
            vec![run("x", sprm::size_half_points(65000))],
            cat(&[
                sprm::indent_start(i16::MAX),
                sprm::space_before(u16::MAX),
                sprm::line_spacing(i16::MIN, false),
            ]),
        )])
    };
    let doc = import(&spec).document;
    assert_eq!(engine::first_point_field_beyond_bound(&doc), None);
    assert_eq!(engine::first_font_size_beyond_bound(&doc), None);
    let p = paragraphs(&doc)[0];
    assert_eq!(style_at(p, 0).font_size, Some(engine::MAX_FONT_SIZE_PT));
}

#[test]
fn special_characters_map_onto_the_model() {
    /* tab, vertical tab (line break), page break, non-breaking and
    optional hyphen, a picture anchor, a stray control character and a
    noncharacter. */
    let text = "a\tb\u{B}c\u{C}d\u{1E}e\u{1F}f\u{1}g\u{3}h\u{FFFF}i";
    let spec = DocSpec::new(vec![Para::plain(text)]);
    let out = import(&spec);
    let p = paragraphs(&out.document)[0].clone();
    assert_eq!(
        p.text,
        format!("a\tb{LINE_BREAK}c{PAGE_BREAK}d\u{2011}e\u{AD}fgh\u{FFFD}i")
    );
    assert!(
        p.text
            .chars()
            .all(|c| c == PAGE_BREAK || engine::text_input::is_xml_char(c)),
        "{:?}",
        p.text
    );
    assert_eq!(
        out.not_imported,
        vec![NotImported {
            content: LegacyContent::Pictures,
            count: 1
        }]
    );
}

#[test]
fn a_field_keeps_its_result_and_hides_its_instruction() {
    let text = "See \u{13} HYPERLINK \"http://x\" \u{14}the site\u{15} and \u{13}PAGE\u{15}.";
    let spec = DocSpec::new(vec![
        Para::plain(text),
        /* A stray begin with no end is closed at its paragraph end. */
        Para::plain("broken \u{13} CODE"),
        Para::plain("still visible"),
    ]);
    let out = import(&spec);
    assert_eq!(
        texts(&out.document),
        vec!["See the site and .", "broken ", "still visible"]
    );
    assert!(out.not_imported.contains(&NotImported {
        content: LegacyContent::Fields,
        count: 3
    }));
}

/* ---- Tables (issue #576, [MS-DOC] 2.4.3 / 2.6.3) ---- */

/// The table at block `i` of `doc`.
fn table_at(doc: &DocumentTree, i: usize) -> &Table {
    match doc.blocks.get(i) {
        Some(Block::Table(t)) => t,
        other => panic!("block {i} is not a table: {other:?}"),
    }
}

/// The texts of a cell's paragraphs ("<table>" for a nested table).
fn cell_texts(cell: &TableCell) -> Vec<String> {
    cell.blocks
        .iter()
        .map(|b| match b {
            Block::Paragraph(p) => p.text.clone(),
            Block::Table(_) => "<table>".to_string(),
        })
        .collect()
}

/// The texts of every cell, row by row.
fn grid_texts(t: &Table) -> Vec<Vec<String>> {
    t.rows
        .iter()
        .map(|r| r.cells.iter().map(|c| cell_texts(c).join("|")).collect())
        .collect()
}

#[test]
fn a_2x2_table_imports_with_its_grid_widths_borders_and_rows() {
    /* Word's default table at the left margin: the first edge 108 twips
    (the default cell margin) left of it, 2 in columns, single 1/2 pt
    borders, the second row at least 0.5 in tall. */
    let tap = |height: i16| {
        cat(&[
            sprm::def_table(&[-108, 2772, 5652], &[Tc::default(), Tc::default()]),
            sprm::table_borders80(sprm::brc80(4, 0x01, 0)),
            sprm::row_height(height),
        ])
    };
    let spec = DocSpec::new(vec![
        Para::plain("before"),
        Para::cell("A1", Vec::new()),
        Para::cell("B1", Vec::new()),
        Para::row_end(tap(0)),
        Para::cell("A2", Vec::new()),
        Para::cell("B2", sprm::jc(1)),
        Para::row_end(tap(720)),
        Para::plain("after"),
    ]);
    let out = import(&spec);
    let doc = &out.document;
    assert!(out.not_imported.is_empty(), "{:?}", out.not_imported);
    assert_eq!(doc.blocks.len(), 3);
    assert!(matches!(&doc.blocks[0], Block::Paragraph(p) if p.text == "before"));
    assert!(matches!(&doc.blocks[2], Block::Paragraph(p) if p.text == "after"));
    let t = table_at(doc, 1);
    assert_eq!(grid_texts(t), vec![vec!["A1", "B1"], vec!["A2", "B2"]]);
    assert_eq!(t.grid, vec![2880, 2880]);
    assert_eq!(t.props.indent_twips, -108);
    assert_eq!(t.props.layout, TableLayout::Fixed);
    assert!(!t.props.bidi_visual);
    let borders = t.props.borders.as_ref().expect("table borders");
    let top = borders.top.as_ref().expect("a top border");
    assert_eq!(
        (top.style.clone(), top.size_eighth_pt),
        (BorderStyle::Single, 4)
    );
    assert!(borders.inside_h.is_some() && borders.inside_v.is_some());
    for row in &t.rows {
        for c in &row.cells {
            assert_eq!(c.props.grid_span, 1);
            assert_eq!(c.props.width, Some(CellWidth::Dxa(2880)));
        }
    }
    assert_eq!(t.rows[0].props.height, None);
    assert_eq!(
        t.rows[1].props.height,
        Some(RowHeight::AtLeast { twips: 720 })
    );
    /* The cell paragraph keeps its own formatting. */
    let Block::Paragraph(b2) = &t.rows[1].cells[1].blocks[0] else {
        panic!("a paragraph");
    };
    assert_eq!(b2.props.alignment, Some(Alignment::Center));
    assert_eq!(doc.ragged_table_rows(), 0);
}

#[test]
fn merged_cells_span_grid_columns_both_ways() {
    let restart = Tc {
        vert_merge: 3,
        ..Tc::default()
    };
    let spec = DocSpec::new(vec![
        /* Row 1: three 1000-twip cells, the third starting a vertical
        merge. */
        Para::cell("a", Vec::new()),
        Para::cell("b", Vec::new()),
        Para::cell("c", Vec::new()),
        Para::row_end(sprm::def_table(
            &[0, 1000, 2000, 3000],
            &[Tc::default(), Tc::default(), restart],
        )),
        /* Row 2 (Word 2000+): one wide cell over the first two columns,
        the second cell continuing the merge (sprmTVertMerge). */
        Para::cell("wide", Vec::new()),
        Para::cell("", Vec::new()),
        Para::row_end(cat(&[
            sprm::def_table(&[0, 2000, 3000], &[]),
            sprm::vert_merge(1, 1),
        ])),
        /* Row 3 (Word 97): three cells, the second merged into the first
        by its TCGRF; its (empty) content is not shown. */
        Para::cell("merged", Vec::new()),
        Para::cell("", Vec::new()),
        Para::cell("z", Vec::new()),
        Para::row_end(sprm::def_table(
            &[0, 1000, 2000, 3000],
            &[
                Tc {
                    horz_merge: 2,
                    ..Tc::default()
                },
                Tc {
                    horz_merge: 1,
                    ..Tc::default()
                },
                Tc::default(),
            ],
        )),
    ]);
    let out = import(&spec);
    let t = table_at(&out.document, 0);
    assert_eq!(t.grid, vec![1000, 1000, 1000]);
    assert_eq!(
        grid_texts(t),
        vec![vec!["a", "b", "c"], vec!["wide", ""], vec!["merged", "z"]]
    );
    let spans: Vec<Vec<u8>> = t
        .rows
        .iter()
        .map(|r| r.cells.iter().map(|c| c.props.grid_span).collect())
        .collect();
    assert_eq!(spans, vec![vec![1, 1, 1], vec![2, 1], vec![2, 1]]);
    assert_eq!(t.rows[0].cells[2].props.v_merge, VMergeRole::Restart);
    assert_eq!(t.rows[1].cells[1].props.v_merge, VMergeRole::Continue);
    assert_eq!(t.rows[1].cells[0].props.width, Some(CellWidth::Dxa(2000)));
    assert_eq!(out.document.ragged_table_rows(), 0);
}

#[test]
fn a_nested_table_lives_in_its_cell() {
    let inner = |text: &str, row_end: bool| {
        let mut g = cat(&[sprm::in_table(true), sprm::itap(2), sprm::inner_cell(true)]);
        if row_end {
            g.extend(sprm::inner_row_end(true));
            g.extend(sprm::def_table(&[0, 1440], &[]));
        }
        let runs = if text.is_empty() {
            Vec::new()
        } else {
            vec![run(text, Vec::new())]
        };
        Para::new(runs, g)
    };
    let spec = DocSpec::new(vec![
        /* Outer cell A: a paragraph, a 1x1 nested table, the cell mark's
        (empty) paragraph. */
        Para::new(
            vec![run("outer", Vec::new())],
            cat(&[sprm::in_table(true), sprm::itap(1)]),
        ),
        inner("inner", false),
        inner("", true),
        Para::cell("", sprm::itap(1)),
        Para::cell("B", sprm::itap(1)),
        Para::row_end(cat(&[
            sprm::itap(1),
            sprm::def_table(&[0, 2880, 5760], &[]),
        ])),
        Para::plain("after"),
    ]);
    let out = import(&spec);
    assert!(out.not_imported.is_empty(), "{:?}", out.not_imported);
    let t = table_at(&out.document, 0);
    assert_eq!(grid_texts(t), vec![vec!["outer|<table>|", "B"]]);
    let Block::Table(nested) = &t.rows[0].cells[0].blocks[1] else {
        panic!("a nested table");
    };
    assert_eq!(grid_texts(nested), vec![vec!["inner"]]);
    assert_eq!(nested.grid, vec![1440]);
    assert!(matches!(&out.document.blocks[1], Block::Paragraph(p) if p.text == "after"));
}

#[test]
fn a_table_at_the_end_of_the_text_is_followed_by_a_paragraph() {
    let spec = DocSpec::new(vec![
        Para::cell("only", Vec::new()),
        Para::row_end(sprm::def_table(&[0, 1440], &[])),
    ]);
    let doc = import(&spec).document;
    assert_eq!(doc.blocks.len(), 2);
    assert_eq!(grid_texts(table_at(&doc, 0)), vec![vec!["only"]]);
    assert!(matches!(&doc.blocks[1], Block::Paragraph(p) if p.text.is_empty()));
}

#[test]
fn a_right_to_left_table_keeps_its_direction_and_splits_from_an_ltr_one() {
    let rtl = |header: bool| {
        cat(&[
            sprm::def_table(&[0, 2000], &[]),
            sprm::table_rtl(true),
            sprm::table_jc(1),
            sprm::header_row(header),
        ])
    };
    let spec = DocSpec::new(vec![
        Para::cell("رأس", sprm::rtl_paragraph(true)),
        Para::row_end(rtl(true)),
        Para::cell("خلية", sprm::rtl_paragraph(true)),
        Para::row_end(rtl(false)),
        /* A left-to-right row right after: another table (2.4.3). */
        Para::cell("ltr", Vec::new()),
        Para::row_end(sprm::def_table(&[0, 2000], &[])),
    ]);
    let doc = import(&spec).document;
    let t = table_at(&doc, 0);
    assert!(t.props.bidi_visual);
    assert_eq!(t.props.alignment, Some(Alignment::Center));
    assert_eq!(grid_texts(t), vec![vec!["رأس"], vec!["خلية"]]);
    assert!(t.rows[0].props.header && !t.rows[1].props.header);
    let ltr = table_at(&doc, 1);
    assert!(!ltr.props.bidi_visual);
    assert_eq!(grid_texts(ltr), vec![vec!["ltr"]]);
}

#[test]
fn cell_shading_alignment_and_borders_come_from_the_row_mark() {
    let tc = Tc {
        v_align: 2,
        borders: [sprm::brc80(12, 0x03, 6), [0; 4], [0xFF; 4], [0; 4]],
        ..Tc::default()
    };
    let spec = DocSpec::new(vec![
        Para::cell("x", Vec::new()),
        Para::cell("y", Vec::new()),
        Para::row_end(cat(&[
            sprm::def_table(&[0, 1000, 2000], &[tc, Tc::default()]),
            sprm::set_shd(1, 2, [0xFF, 0xEB, 0x78]),
        ])),
    ]);
    let doc = import(&spec).document;
    let t = table_at(&doc, 0);
    let (x, y) = (&t.rows[0].cells[0].props, &t.rows[0].cells[1].props);
    assert_eq!(x.v_align, VerticalAlign::Bottom);
    let b = x.borders.as_ref().expect("the first cell's borders");
    let top = b.top.as_ref().expect("a top border");
    assert_eq!(
        (top.style.clone(), top.size_eighth_pt, top.color),
        (BorderStyle::Double, 12, Some([255, 0, 0, 255]))
    );
    /* A none / nil border in a TC80 is not set: the table's shows. */
    assert!(b.left.is_none() && b.bottom.is_none());
    assert_eq!(x.shading, None);
    assert_eq!(y.shading, Some([0xFF, 0xEB, 0x78, 255]));
    assert!(y.borders.is_none());
}

/// A row mark whose table sprms do not fit its FKP page keeps them in the
/// Data stream behind a `sprmPHugePapx` (2.6.2).
#[test]
fn a_row_mark_read_through_a_huge_papx() {
    let real = cat(&[
        sprm::in_table(true),
        sprm::row_end(true),
        sprm::def_table(&[0, 1500, 3000], &[]),
    ]);
    let mut data = vec![0u8; 16];
    let at = data.len() as u32;
    data.extend(prc_data(&real));
    let spec = DocSpec {
        data,
        ..DocSpec::new(vec![
            Para::cell("p", Vec::new()),
            Para::cell("q", Vec::new()),
            Para {
                runs: Vec::new(),
                papx: sprm::huge_papx(at),
                mark: '\u{7}',
            },
        ])
    };
    let doc = import(&spec).document;
    let t = table_at(&doc, 0);
    assert_eq!(grid_texts(t), vec![vec!["p", "q"]]);
    assert_eq!(t.grid, vec![1500, 1500]);
    /* An offset past the Data stream leaves the in-page grpprl: no row
    mark, so the mark ends a third (empty) cell of an unterminated row. */
    let mut broken = spec.clone();
    broken.paragraphs[2].papx = sprm::huge_papx(1 << 20);
    let doc = import(&broken).document;
    let t = table_at(&doc, 0);
    assert_eq!(grid_texts(t), vec![vec!["p", "q", ""]]);
}

#[test]
fn a_cell_without_a_row_mark_is_a_one_cell_table() {
    /* A damaged file: a cell mark with no row mark. An automatic layout
    over the even fallback grid. */
    let spec = DocSpec::new(vec![
        Para::plain("before"),
        Para::cell("lonely", Vec::new()),
        Para::plain("after"),
    ]);
    let out = import(&spec);
    let t = table_at(&out.document, 1);
    assert_eq!(grid_texts(t), vec![vec!["lonely"]]);
    assert_eq!(t.props.layout, TableLayout::Autofit);
    assert_eq!(t.grid.len(), 1);
    assert!(out.not_imported.is_empty(), "{:?}", out.not_imported);
}

#[test]
fn tables_nested_past_the_model_depth_keep_their_text_and_are_reported() {
    /* Depth 70: the levels past 64 stay paragraphs of the deepest cell. */
    let deep = 70;
    let mut paras = Vec::new();
    for d in (1..=deep).rev() {
        let mut g = cat(&[sprm::in_table(true), sprm::itap(d)]);
        if d > 1 {
            g.extend(sprm::inner_cell(true));
        }
        paras.push(Para {
            runs: vec![run(&format!("d{d}"), Vec::new())],
            papx: g,
            mark: if d == 1 { '\u{7}' } else { '\r' },
        });
    }
    let out = import(&DocSpec::new(paras));
    assert_eq!(
        out.not_imported,
        vec![NotImported {
            content: LegacyContent::Tables,
            count: (deep - 64) as u32
        }]
    );
    /* 64 tables, one inside the first cell of the other; every paragraph
    kept. */
    let mut depth = 0;
    let mut texts = 0;
    let mut blocks: Vec<Block> = out.document.blocks.iter().cloned().collect();
    loop {
        texts += blocks
            .iter()
            .filter(|b| matches!(b, Block::Paragraph(p) if !p.text.is_empty()))
            .count();
        let Some(Block::Table(t)) = blocks.first() else {
            break;
        };
        depth += 1;
        let next = t.rows[0].cells[0].blocks.clone();
        blocks = next;
    }
    assert_eq!(depth, 64);
    assert_eq!(texts, deep as usize);
}

#[test]
fn an_empty_main_text_is_one_empty_paragraph() {
    let spec = DocSpec::new(Vec::new());
    let doc = import(&spec).document;
    assert_eq!(texts(&doc), vec![String::new()]);
}

#[test]
fn the_0table_stream_is_read_when_the_fib_names_it() {
    let spec = DocSpec {
        one_table: false,
        ..DocSpec::new(vec![Para::plain("zero table")])
    };
    assert_eq!(texts(&import(&spec).document), vec!["zero table"]);
}

#[test]
fn an_encrypted_document_is_refused_as_encrypted() {
    let spec = DocSpec {
        encrypted: true,
        ..DocSpec::new(vec![Para::plain("secret")])
    };
    assert_eq!(read_doc(&build(&spec)).unwrap_err(), DocError::Encrypted);
}

#[test]
fn foreign_and_damaged_containers_get_typed_errors() {
    let not_cfb = read_doc(b"PK\x03\x04 not a compound file").unwrap_err();
    assert!(
        matches!(not_cfb, DocError::Container { stream: None, .. }),
        "{not_cfb:?}"
    );

    let workbook = format_docx::opc::cfb::test_writer::build(&[("Workbook", &[0u8; 64])]);
    assert_eq!(read_doc(&workbook).unwrap_err(), DocError::NotWordDocument);
    assert!(crate::is_legacy_compound_file(&workbook));

    let (mut word, table) = streams(&DocSpec::new(vec![Para::plain("x")]));
    word[0..2].copy_from_slice(&0xA5DCu16.to_le_bytes());
    let w95 =
        format_docx::opc::cfb::test_writer::build(&[("WordDocument", &word), ("1Table", &table)]);
    assert!(matches!(
        read_doc(&w95).unwrap_err(),
        DocError::UnsupportedVersion {
            w_ident: 0xA5DC,
            ..
        }
    ));

    /* A Clx whose PlcPcd is cut short names the real table stream. */
    let (word, mut table) = streams(&DocSpec::new(vec![Para::plain("abc")]));
    table[1..5].copy_from_slice(&3u32.to_le_bytes());
    let cut =
        format_docx::opc::cfb::test_writer::build(&[("WordDocument", &word), ("1Table", &table)]);
    let e = read_doc(&cut).unwrap_err();
    match &e {
        DocError::Malformed { part, .. } => assert_eq!(part, "1Table: Clx"),
        other => panic!("expected a malformed Clx, got {other:?}"),
    }
    assert!(e.to_string().starts_with("1Table: Clx: "), "{e}");
}

/// Every table holds rows, every row cells, every cell blocks ending in a
/// paragraph, each span inside the grid; tables nest at most 64 deep.
fn assert_table_shapes<'a>(blocks: impl Iterator<Item = &'a Block>, depth: u32) {
    for b in blocks {
        if let Block::Table(t) = b {
            assert!(depth < 64, "a table nested past the model's depth");
            assert!(!t.rows.is_empty() && !t.grid.is_empty());
            for row in &t.rows {
                assert!(!row.cells.is_empty());
                for c in &row.cells {
                    assert!(c.props.grid_span >= 1);
                    assert!(c.props.grid_span as usize <= t.grid.len());
                    assert!(matches!(c.blocks.last(), Some(Block::Paragraph(_))));
                    assert_table_shapes(c.blocks.iter(), depth + 1);
                }
            }
        }
    }
}

/// Hostile input: thousands of single-byte mutations of valid images
/// (deterministic PRNG) must each import or fail with a typed error, and an
/// import must keep the model's text and bounds rules.
#[test]
fn mutated_images_never_panic_and_stay_in_bounds() {
    let seeds = [
        build(&DocSpec::new(vec![
            Para::new(
                vec![run("Hello ", Vec::new()), run("bold", sprm::bold(true))],
                sprm::jc(1),
            ),
            Para::plain("two"),
        ])),
        build(&DocSpec {
            sections: vec![(4, cat(&[sprm::page_size(12240, 15840)]))],
            ..DocSpec::new(vec![Para::new(
                vec![run("عربي", sprm::rtl_run(true))],
                sprm::rtl_paragraph(true),
            )])
        }),
        /* Issue #576 — a table: cell / row marks, a TDefTable with TC80s,
        table borders, a shading and a vertical merge. */
        build(&DocSpec::new(vec![
            Para::cell("A1", Vec::new()),
            Para::cell("B1", Vec::new()),
            Para::row_end(cat(&[
                sprm::def_table(
                    &[-108, 1332, 2772],
                    &[
                        Tc {
                            vert_merge: 3,
                            borders: [sprm::brc80(8, 1, 6); 4],
                            ..Tc::default()
                        },
                        Tc::default(),
                    ],
                ),
                sprm::table_borders80(sprm::brc80(4, 1, 0)),
                sprm::set_shd(0, 2, [1, 2, 3]),
            ])),
            Para::cell("", Vec::new()),
            Para::cell("B2", Vec::new()),
            Para::row_end(cat(&[
                sprm::def_table(&[-108, 1332, 2772], &[]),
                sprm::vert_merge(0, 1),
            ])),
            Para::plain("after"),
        ])),
    ];
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for seed in &seeds {
        for _ in 0..1500 {
            let mut bytes = seed.clone();
            for _ in 0..(next() % 4 + 1) {
                let at = (next() as usize) % bytes.len();
                bytes[at] = next() as u8;
            }
            if let Ok(out) = read_doc(&bytes) {
                let doc = out.document;
                assert_eq!(engine::first_point_field_beyond_bound(&doc), None);
                assert_eq!(engine::first_font_size_beyond_bound(&doc), None);
                engine::for_each_paragraph_nested(doc.blocks.iter(), &mut |_, p| {
                    assert!(
                        p.text
                            .chars()
                            .all(|c| c == PAGE_BREAK || engine::text_input::is_xml_char(c))
                    );
                    for s in &p.spans {
                        assert!(s.start < s.end && s.end as usize <= p.text.len());
                    }
                });
                assert_table_shapes(doc.blocks.iter(), 0);
            }
        }
    }
}
