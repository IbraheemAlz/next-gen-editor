//! Unit tests over `.doc` images built in memory by [`crate::test_builder`]
//! (no binary fixture is committed).

use engine::text_input::{LINE_BREAK, PAGE_BREAK};
use engine::{
    Alignment, Block, DocumentTree, LineHeight, Paragraph, SectionType, SpanStyle, TextDirection,
    UnderlineStyle,
};

use crate::test_builder::{
    DocSpec, Encoding, Para, PieceSpec, build, prm0, prm1, run, sprm, streams,
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

#[test]
fn table_cells_become_paragraphs_and_row_marks_vanish() {
    let cell = |t: &str| Para {
        mark: '\u{7}',
        ..Para::new(vec![run(t, Vec::new())], sprm::in_table(true))
    };
    let spec = DocSpec::new(vec![
        Para::plain("before"),
        cell("A1"),
        cell("B1"),
        Para {
            mark: '\u{7}',
            ..Para::new(
                Vec::new(),
                cat(&[sprm::in_table(true), sprm::row_end(true)]),
            )
        },
        Para::new(vec![run("listed", Vec::new())], sprm::list(1)),
    ]);
    let out = import(&spec);
    assert_eq!(texts(&out.document), vec!["before", "A1", "B1", "listed"]);
    assert_eq!(
        out.not_imported,
        vec![
            NotImported {
                content: LegacyContent::Tables,
                count: 3
            },
            NotImported {
                content: LegacyContent::Lists,
                count: 1
            },
        ]
    );
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
                for p in paragraphs(&doc) {
                    assert!(
                        p.text
                            .chars()
                            .all(|c| c == PAGE_BREAK || engine::text_input::is_xml_char(c))
                    );
                    for s in &p.spans {
                        assert!(s.start < s.end && s.end as usize <= p.text.len());
                    }
                }
            }
        }
    }
}
