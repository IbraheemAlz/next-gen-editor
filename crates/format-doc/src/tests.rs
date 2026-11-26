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
                count: 2
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
        build(&crate::test_builder::styled_sample()),
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

/* ---- Issue #575: the style sheet and the font table ---- */

use crate::test_builder::{
    ISTD_NIL, SAMPLE_TEXTS, STI_USER, StyleSheetSpec, StyleSpec, font_table, styled_sample,
};
use engine::{FontFamily, VertAlign};

fn family(name: &str) -> Option<FontFamily> {
    FontFamily::from_display_name(name)
}

/// The style table, the document defaults and every paragraph's style id
/// land where the `.docx` reader puts them; `props` is the engine's own
/// cascade of style + direct formatting.
#[test]
fn the_style_sheet_becomes_the_engine_style_table() {
    let out = import(&styled_sample());
    assert!(out.not_imported.is_empty(), "{:?}", out.not_imported);
    let doc = &out.document;
    assert_eq!(texts(doc), SAMPLE_TEXTS.to_vec());

    /* Only the two paragraph styles are mirrored; Normal is the default. */
    let mut ids: Vec<&str> = doc.styles.keys().map(String::as_str).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec!["Heading1", "Normal"]);
    let normal = &doc.styles["Normal"];
    assert!(normal.is_default);
    assert_eq!(normal.based_on, None);
    assert_eq!(normal.para.direction, Some(TextDirection::Rtl));
    assert_eq!(normal.para.spacing.after_twips, 160);
    assert_eq!(
        normal.para.line_height,
        Some(LineHeight::Auto { twips: 276 })
    );
    assert_eq!(normal.run.font_size, Some(12.0));
    assert_eq!(normal.run.font_size_cs, Some(14.0));
    assert_eq!(normal.run.font_family, family("Arial"));
    assert_eq!(normal.run.font_family_cs, family("Simplified Arabic"));
    let h1 = &doc.styles["Heading1"];
    assert!(!h1.is_default);
    assert_eq!(
        h1.name, "heading 1",
        "Word's built-in name, aliases dropped"
    );
    assert_eq!(h1.based_on.as_deref(), Some("Normal"));
    assert_eq!(h1.next.as_deref(), Some("Normal"));
    /* Its OWN properties only: the chain is the engine's to fold. */
    assert_eq!(h1.para.direction, None);
    assert_eq!(h1.para.keep_next, Some(true));
    assert_eq!(h1.para.outline_level, Some(0));
    assert_eq!(h1.run.bold, Some(true));
    assert_eq!(h1.run.font_size, Some(16.0));
    assert_eq!(h1.run.font_family, None);

    /* The defaults: 10 pt in the style sheet's default fonts. */
    assert_eq!(doc.style_run_defaults.font_size, Some(10.0));
    assert_eq!(
        doc.style_run_defaults.font_family,
        family("Times New Roman")
    );
    assert_eq!(
        doc.style_run_defaults.font_family_cs,
        family("Simplified Arabic")
    );
    assert_eq!(doc.style_defaults, engine::ParaProperties::default());

    let ps = paragraphs(doc);
    /* A Normal paragraph names no style (Word writes no <w:pStyle>) and
    is right-to-left through Normal alone. */
    assert_eq!(ps[0].style_id, None);
    assert_eq!(ps[0].direct_overrides, engine::ParaProperties::default());
    assert_eq!(ps[0].props.direction, Some(TextDirection::Rtl));
    assert_eq!(ps[0].props.spacing.after_twips, 160);
    assert_eq!(ps[0].props, doc.resolve_style_cascade(None));
    assert!(
        ps[0].spans.is_empty(),
        "no direct formatting: {:?}",
        ps[0].spans
    );
    let run0 = doc.resolve_style_run_cascade(None);
    assert_eq!(
        (run0.font_family.clone(), run0.font_size),
        (family("Arial"), Some(12.0))
    );

    assert_eq!(ps[1].style_id.as_deref(), Some("Heading1"));
    assert_eq!(
        ps[1].props.direction,
        Some(TextDirection::Rtl),
        "from Normal"
    );
    assert_eq!(ps[1].props.keep_next, Some(true));
    assert_eq!(ps[1].props.spacing.before_twips, 240);
    let run1 = doc.resolve_style_run_cascade(Some("Heading1"));
    assert_eq!(
        (run1.bold, run1.font_size, run1.font_family),
        (Some(true), Some(16.0), family("Arial"))
    );

    /* The character style is folded into its run with its id. */
    let quoted = ps[2].text.find("quoted").expect("quoted") as u32;
    assert_eq!(ps[2].spans.len(), 1, "{:?}", ps[2].spans);
    assert_eq!(
        (ps[2].spans[0].start, ps[2].spans[0].end),
        (quoted, quoted + 6)
    );
    let s = &ps[2].spans[0].style;
    assert_eq!(s.char_style.as_deref(), Some("QuoteChar"));
    assert_eq!(s.italic, Some(true));
    assert_eq!(s.color, Some([0x80, 0, 0, 255]));

    /* Direct formatting stays direct, over the style. */
    assert_eq!(ps[3].direct_overrides.direction, Some(TextDirection::Ltr));
    assert_eq!(ps[3].props.direction, Some(TextDirection::Ltr));
    assert_eq!(ps[3].props.spacing.after_twips, 160, "Normal's spacing");
}

/// ModifyStyle on an imported style re-cascades every paragraph in it.
#[test]
fn an_imported_style_re_cascades_when_modified() {
    let doc = import(&styled_sample()).document;
    let mut styles = doc.styles.clone();
    styles.get_mut("Heading1").expect("Heading1").para.alignment = Some(Alignment::Center);
    let mut p = paragraphs(&doc)[1].clone();
    assert_eq!(p.props.alignment, None);
    engine::recompute_paragraph_props(&mut p, &styles, &doc.style_defaults);
    assert_eq!(p.props.alignment, Some(Alignment::Center));
    assert_eq!(p.props.direction, Some(TextDirection::Rtl));
}

/// ToggleOperand 0x80 / 0x81 (2.9.327) resolve against the style chain:
/// in a bold heading, 0x81 is not bold and 0x80 inherits the heading's
/// bold; over an italic character style 0x81 is upright.
#[test]
fn toggle_operands_resolve_against_the_style_chain() {
    let mut spec = styled_sample();
    spec.paragraphs = vec![
        Para::new(
            vec![
                run("opposite", sprm::bold_operand(0x81)),
                run("same", sprm::bold_operand(0x80)),
                run(
                    "upright",
                    cat(&[sprm::char_style(15), sprm::italic_operand(0x81)]),
                ),
            ],
            Vec::new(),
        )
        .styled(1),
        /* In Normal (not bold) 0x81 is bold. */
        Para::new(vec![run("bold", sprm::bold_operand(0x81))], Vec::new()),
    ];
    let doc = import(&spec).document;
    let ps = paragraphs(&doc);
    let at = |p: &Paragraph, w: &str| style_at(p, p.text.find(w).expect(w) as u32);
    assert_eq!(at(ps[0], "opposite").bold, Some(false));
    assert_eq!(at(ps[0], "same").bold, None, "inherits the heading's bold");
    let upright = at(ps[0], "upright");
    assert_eq!(
        (upright.italic, upright.char_style.as_deref()),
        (Some(false), Some("QuoteChar"))
    );
    assert_eq!(at(ps[1], "bold").bold, Some(true));
}

/// Direct fonts name the font table's faces; an index past the table is
/// ignored; the mark's own formatting becomes `mark_style`.
#[test]
fn fonts_marks_and_the_remaining_character_sprms() {
    let mut spec = styled_sample();
    spec.paragraphs = vec![
        Para::new(
            vec![
                run("roman", sprm::font(0)),
                run("عربي", cat(&[sprm::rtl_run(true), sprm::font_cs(1)])),
                run("nowhere", sprm::font(40)),
                run("CAPS", cat(&[sprm::caps(true), sprm::small_caps(true)])),
                run("sup", sprm::vert_align(1)),
                run("sub", sprm::vert_align(2)),
                run("lit", sprm::highlight(7)),
            ],
            Vec::new(),
        ),
        Para {
            mark_chpx: cat(&[sprm::bold(true), sprm::size_half_points(40)]),
            ..Para::new(Vec::new(), Vec::new())
        },
    ];
    let doc = import(&spec).document;
    let ps = paragraphs(&doc);
    let at = |w: &str| style_at(ps[0], ps[0].text.find(w).expect(w) as u32);
    assert_eq!(at("roman").font_family, family("Times New Roman"));
    let arabic = at("عربي");
    assert_eq!(arabic.font_family_cs, family("Arial"));
    assert!(arabic.forces_complex_script());
    assert_eq!(at("nowhere"), SpanStyle::default());
    let caps = at("CAPS");
    assert_eq!((caps.caps, caps.small_caps), (Some(true), Some(true)));
    assert_eq!(at("sup").vert_align, Some(VertAlign::Superscript));
    assert_eq!(at("sub").vert_align, Some(VertAlign::Subscript));
    assert_eq!(at("lit").bg_color, Some([255, 255, 0, 255]));
    let mark = ps[1].mark_style.as_deref().expect("the mark's formatting");
    assert_eq!((mark.bold, mark.font_size), (Some(true), Some(20.0)));
    assert!(ps[0].mark_style.is_none());
}

/// The paragraph sprms a style typically carries also apply directly:
/// keep together, keep with next, page break before, widow control and
/// the outline level; `sprmPIstd` re-styles a paragraph.
#[test]
fn keep_page_break_widow_and_outline_sprms() {
    let mut spec = styled_sample();
    spec.paragraphs = vec![
        Para::new(
            vec![run("kept", Vec::new())],
            cat(&[
                sprm::keep_lines(true),
                sprm::keep_next(false),
                sprm::page_break_before(true),
                sprm::widow_control(false),
                sprm::outline_level(12),
            ]),
        )
        .styled(1),
        Para::new(vec![run("restyled", Vec::new())], sprm::paragraph_style(1)),
    ];
    let doc = import(&spec).document;
    let ps = paragraphs(&doc);
    let p = &ps[0].props;
    assert_eq!(p.keep_lines, Some(true));
    assert_eq!(p.keep_next, Some(false), "direct OFF beats the style's ON");
    assert!(p.page_break_before);
    assert_eq!(p.widow_control, Some(false));
    assert_eq!(p.outline_level, Some(9), "held to body text");
    assert_eq!(ps[1].style_id.as_deref(), Some("Heading1"));
    assert_eq!(ps[1].props.keep_next, Some(true));
}

/// A physical justification (sprmPJc80) is read against the direction
/// the paragraph's STYLE gives it: right in a right-to-left Normal
/// paragraph is its start.
#[test]
fn a_physical_justification_reads_against_the_style_direction() {
    let mut spec = styled_sample();
    spec.paragraphs = vec![
        Para::new(vec![run("right", Vec::new())], sprm::jc80(2)),
        Para::new(
            vec![run("ltr right", Vec::new())],
            cat(&[sprm::rtl_paragraph(false), sprm::jc80(2)]),
        ),
    ];
    let doc = import(&spec).document;
    let ps = paragraphs(&doc);
    assert_eq!(ps[0].props.alignment, Some(Alignment::Start));
    assert_eq!(ps[1].props.alignment, Some(Alignment::End));
}

/// A paragraph whose style is a list style is a list paragraph for the
/// import report; an `istd` naming no style formats it with Normal.
#[test]
fn style_lists_are_reported_and_an_unknown_istd_is_normal() {
    let mut spec = styled_sample();
    let sheet = spec.styles.as_mut().expect("sheet");
    sheet.styles[2] = Some(StyleSpec::paragraph(
        48,
        "List Bullet",
        0,
        sprm::list(1),
        Vec::new(),
    ));
    spec.paragraphs = vec![
        Para::plain("bullet").styled(2),
        Para::new(vec![run("not a bullet", Vec::new())], sprm::list(0)).styled(2),
        Para::plain("unknown").styled(9),
        Para::plain("far").styled(700),
    ];
    let out = import(&spec);
    assert_eq!(
        out.not_imported,
        vec![NotImported {
            content: LegacyContent::Lists,
            count: 1
        }]
    );
    let ps = paragraphs(&out.document);
    assert_eq!(ps[0].style_id.as_deref(), Some("ListBullet"));
    for p in &ps[2..] {
        assert_eq!(p.style_id, None);
        assert_eq!(p.props.direction, Some(TextDirection::Rtl), "Normal");
    }
}

/// A hostile sheet: `istdBase` cycles (a self loop, a two-cycle), a
/// character style based on a paragraph style, a next style that is a
/// character style, a long name, a `StdfPost2000` header. Everything
/// terminates; the readable styles import.
#[test]
fn a_hostile_style_sheet_terminates_and_keeps_what_it_can() {
    let mut styles: Vec<Option<StyleSpec>> = vec![None; 6];
    styles[0] = Some(StyleSpec::paragraph(
        0,
        "Normal",
        0,
        sprm::rtl_paragraph(true),
        Vec::new(),
    ));
    styles[1] = Some(StyleSpec::paragraph(
        STI_USER,
        "Loop A",
        2,
        Vec::new(),
        sprm::bold(true),
    ));
    styles[2] = Some(StyleSpec::paragraph(
        STI_USER,
        "Loop B",
        1,
        Vec::new(),
        Vec::new(),
    ));
    styles[3] = Some(StyleSpec::character(STI_USER, "Odd", 0, sprm::italic(true)));
    styles[4] = Some(StyleSpec {
        next: 3,
        ..StyleSpec::paragraph(STI_USER, &"x".repeat(400), ISTD_NIL, Vec::new(), Vec::new())
    });
    let spec = DocSpec {
        styles: Some(StyleSheetSpec {
            std_base: 18,
            ..StyleSheetSpec::new(styles)
        }),
        ..DocSpec::new(vec![
            Para::plain("loop").styled(1),
            Para::new(vec![run("odd", sprm::char_style(3))], Vec::new()),
            Para::plain("long").styled(4),
        ])
    };
    let out = import(&spec);
    assert!(out.not_imported.is_empty(), "{:?}", out.not_imported);
    let doc = &out.document;
    assert_eq!(doc.styles["LoopA"].based_on.as_deref(), Some("LoopB"));
    assert_eq!(doc.styles["LoopB"].based_on.as_deref(), Some("LoopA"));
    assert_eq!(
        doc.styles["Normal"].based_on, None,
        "a self loop is no parent"
    );
    let long = doc
        .styles
        .values()
        .find(|s| s.name.starts_with('x'))
        .expect("long");
    assert_eq!(long.name.len(), 255);
    assert_eq!(long.next, None, "a character style is no next style");
    let ps = paragraphs(doc);
    assert_eq!(ps[0].style_id.as_deref(), Some("LoopA"));
    assert_eq!(
        ps[0].props.direction, None,
        "the cycle never reaches Normal"
    );
    let run_a = doc.resolve_style_run_cascade(Some("LoopA"));
    assert_eq!(run_a.bold, Some(true));
    /* A character style based on a paragraph style keeps its own sprms. */
    assert_eq!(style_at(ps[1], 0).italic, Some(true));
}

/// A style sheet or font table that cannot be read whole does not refuse
/// the document: what it holds is used and the damage is reported.
#[test]
fn a_damaged_style_sheet_or_font_table_is_reported_not_fatal() {
    let (mut word, mut table) = streams(&styled_sample());
    let fib = crate::fib::Fib::parse(&word).expect("fib");
    let (stsh_fc, stsh_lcb) = fib.pair(crate::fib::pair::STSHF).expect("stsh");
    /* Shrink the STSH's declared size: the sheet ends inside a style. */
    let lcb_at = 0x9A + 8 * crate::fib::pair::STSHF + 4;
    word[lcb_at..lcb_at + 4].copy_from_slice(&(stsh_lcb - 7).to_le_bytes());
    let bytes =
        format_docx::opc::cfb::test_writer::build(&[("WordDocument", &word), ("1Table", &table)]);
    let out = read_doc(&bytes).expect("imports");
    assert_eq!(
        out.not_imported,
        vec![NotImported {
            content: LegacyContent::Styles,
            count: 1
        }]
    );
    assert!(out.document.styles.contains_key("Normal"));
    assert_eq!(texts(&out.document), SAMPLE_TEXTS.to_vec());

    /* A sheet that lies outside the table stream: no styles at all. */
    table.truncate(stsh_fc as usize + 3);
    let bytes =
        format_docx::opc::cfb::test_writer::build(&[("WordDocument", &word), ("1Table", &table)]);
    let out = read_doc(&bytes).expect("imports");
    assert!(
        out.not_imported
            .iter()
            .any(|n| n.content == LegacyContent::Styles)
    );
    assert!(out.document.styles.is_empty());
    assert_eq!(out.document.style_run_defaults, SpanStyle::default());
}

#[test]
fn the_builder_font_table_reads_back() {
    let names = ["Arial".to_string(), "Traditional Arabic".to_string()];
    let t = font_table(&names);
    let (fonts, damaged) = crate::ffn::FontTable::parse(&t, 0, t.len() as u32);
    assert!(!damaged);
    assert_eq!(fonts.name(1), Some("Traditional Arabic"));
}

/// Hostile style sheets and font tables: thousands of mutations inside
/// the `STSH` and the `SttbfFfn` only (deterministic PRNG). Each import
/// must succeed (neither structure refuses a document), keep the model's
/// bounds, and save as a `.docx` holding no character XML cannot carry
/// (style ids, style names and font names come from the file).
#[test]
fn mutated_style_sheets_and_font_tables_import_and_save_clean() {
    let (word, table) = streams(&styled_sample());
    let fib = crate::fib::Fib::parse(&word).expect("fib");
    let (stsh_fc, _) = fib.pair(crate::fib::pair::STSHF).expect("stsh");
    let (ffn_fc, ffn_lcb) = fib.pair(crate::fib::pair::STTBF_FFN).expect("ffn");
    let region = stsh_fc as usize..(ffn_fc + ffn_lcb) as usize;
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..2000 {
        let mut table = table.clone();
        for _ in 0..(next() % 6 + 1) {
            let at = region.start + (next() as usize) % region.len();
            table[at] = next() as u8;
        }
        let bytes = format_docx::opc::cfb::test_writer::build(&[
            ("WordDocument", &word),
            ("1Table", &table),
        ]);
        let out = read_doc(&bytes).expect("a damaged style sheet never refuses the document");
        let doc = out.document;
        assert_eq!(engine::first_point_field_beyond_bound(&doc), None);
        assert_eq!(engine::first_font_size_beyond_bound(&doc), None);
        assert_eq!(texts(&doc), SAMPLE_TEXTS.to_vec());
        for p in paragraphs(&doc) {
            if let Some(id) = &p.style_id {
                assert!(doc.styles.contains_key(id), "{id} is not in the table");
            }
        }
        assert!(doc.styles.values().filter(|s| s.is_default).count() <= 1);
        let saved = format_docx::save_docx(&doc).expect("saves");
        assert_eq!(format_docx::first_forbidden_character(&saved), None);
    }
}
