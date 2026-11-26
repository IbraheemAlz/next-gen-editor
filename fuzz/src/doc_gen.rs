//! Issue #567 — inputs for the `doc_reader` target (legacy Word 97-2003
//! `.doc` import, `format-doc`).
//!
//! NOT a structure-aware generator (that is a follow-up issue): the target
//! reads raw bytes (a committed seed or a mutation of one) as the file
//! itself, and every other input as a SPLICE — a small valid image built by
//! `format_doc::test_builder` from the input's head (text, run / paragraph
//! flags, an optional section, an out-of-order piece split, — issue #576 —
//! the paragraphs as the cells of one table row, and — issue #575 — an
//! optional style sheet and font table with paragraph and character
//! styles), then the rest of the input overwriting bytes of its two streams
//! at input-chosen offsets before the compound file is assembled. That
//! keeps the random sweep inside the FIB / piece table / FKP / SEPX / STSH /
//! SttbfFfn parsers instead of failing on the container signature.

use arbitrary::Unstructured;
use format_doc::test_builder::{
    DocSpec, Encoding, Para, PieceSpec, Tc, build, prc_data, run, sprm, streams, styled_sample,
};

/// The committed `corpus/doc_reader/` seeds: `(file name, .doc bytes)`.
/// Written by `examples/regen-seeds`, pinned by
/// `tests::doc_seeds_are_committed`.
pub fn doc_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let cat = |v: &[Vec<u8>]| v.concat();
    let plain = DocSpec::new(vec![
        Para::new(
            vec![
                run("Hello ", Vec::new()),
                run("bold", sprm::bold(true)),
                run(
                    " world",
                    cat(&[sprm::italic(true), sprm::size_half_points(28)]),
                ),
            ],
            cat(&[sprm::jc(1), sprm::space_after(200)]),
        ),
        Para::plain("Tab\there, a break\u{B}and a field \u{13} PAGE \u{14}7\u{15}."),
    ]);
    let mut rtl = DocSpec::new(vec![
        Para::new(
            vec![run(
                "مرحبا بالعالم",
                cat(&[sprm::rtl_run(true), sprm::bold_cs(true)]),
            )],
            cat(&[sprm::rtl_paragraph(true), sprm::jc80(2)]),
        ),
        Para {
            mark: '\u{7}',
            ..Para::new(vec![run("cell", Vec::new())], sprm::in_table(true))
        },
    ]);
    rtl.sections = vec![(
        rtl.ccp_text(),
        cat(&[
            sprm::page_size(11906, 16838),
            sprm::margins(1440, 1440, 1440, 1440),
        ]),
    )];
    let fast_saved = DocSpec {
        pieces: vec![
            PieceSpec {
                cps: 6,
                encoding: Encoding::Compressed,
                prm: 0,
            },
            PieceSpec {
                cps: 5,
                encoding: Encoding::Utf16,
                prm: format_doc::test_builder::prm0(0x55, 1),
            },
            PieceSpec {
                cps: 6,
                encoding: Encoding::Compressed,
                prm: format_doc::test_builder::prm1(0),
            },
        ],
        physical_order: vec![2, 0, 1],
        prcs: vec![cat(&[sprm::italic(true), sprm::jc(2)])],
        fast_saved: true,
        ..DocSpec::new(vec![Para::plain("Hello café world")])
    };
    /* Issue #576 — tables: a 2x2 table (TDefTable with TC80s, table
    borders, a shading, a vertical merge) holding a nested 1x1 table, a
    right-to-left table after it, and a row mark read through a Huge PAPX
    in the Data stream. */
    let nested = |text: &str, row_end: bool| {
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
    let tc = Tc {
        vert_merge: 3,
        v_align: 1,
        borders: [sprm::brc80(8, 0x01, 6); 4],
        ..Tc::default()
    };
    let huge_row = cat(&[
        sprm::in_table(true),
        sprm::row_end(true),
        sprm::def_table(&[0, 1500, 3000], &[Tc::default(); 2]),
        sprm::table_rtl(true),
    ]);
    let mut tables = DocSpec::new(vec![
        Para::plain("before"),
        nested("inner", false),
        nested("", true),
        Para::cell("A1", Vec::new()),
        Para::cell("B1", Vec::new()),
        Para::row_end(cat(&[
            sprm::def_table(&[-108, 2772, 5652], &[tc, Tc::default()]),
            sprm::table_borders80(sprm::brc80(4, 0x01, 0)),
            sprm::set_shd(1, 2, [0xFF, 0xEB, 0x78]),
            sprm::row_height(400),
        ])),
        Para::cell("", Vec::new()),
        Para::cell("B2", Vec::new()),
        Para::row_end(cat(&[
            sprm::def_table(&[-108, 2772, 5652], &[]),
            sprm::vert_merge(0, 1),
        ])),
        Para::plain("between"),
        Para::cell("ي", sprm::rtl_paragraph(true)),
        Para::cell("ب", sprm::rtl_paragraph(true)),
        Para {
            runs: Vec::new(),
            papx: sprm::huge_papx(8),
            mark: '\u{7}',
            ..Para::new(Vec::new(), Vec::new())
        },
        Para::plain("after"),
    ]);
    tables.data = cat(&[vec![0; 8], prc_data(&huge_row)]);
    vec![
        ("seed_plain.doc", build(&plain)),
        ("seed_rtl_section.doc", build(&rtl)),
        ("seed_fast_saved.doc", build(&fast_saved)),
        ("seed_tables.doc", build(&tables)),
        /* Issue #575 — a style sheet and a font table. */
        ("seed_styles.doc", build(&styled_sample())),
    ]
}

/// A splice input: a valid image from the head of `data`, then byte
/// overwrites of its streams from the rest. `None` when `data` is too
/// short to choose anything.
pub fn spliced(data: &[u8]) -> Option<Vec<u8>> {
    let mut u = Unstructured::new(data);
    let flags: u8 = u.arbitrary().ok()?;
    /* Issue #575 — a second flag byte (the first one's eight bits are
    taken): bit 0 adds the style sheet and font table. */
    let styled = u.arbitrary::<u8>().unwrap_or(0) & 1 != 0;
    let paragraphs = usize::from(u.int_in_range(1u8..=4).ok()?);
    let mut paras = Vec::with_capacity(paragraphs);
    for _ in 0..paragraphs {
        let len = usize::from(u.int_in_range(0u8..=12).ok()?);
        let mut text = String::new();
        for _ in 0..len {
            let b: u8 = u.arbitrary().ok()?;
            /* Latin, Arabic and the special characters the walk maps. */
            text.push(match b % 8 {
                0..=3 => char::from(b'a' + b % 26),
                4 => char::from_u32(0x0627 + u32::from(b % 20)).unwrap_or('a'),
                5 => [' ', '\t', '\u{B}', '\u{C}'][usize::from(b % 4)],
                6 => ['\u{13}', '\u{14}', '\u{15}', '\u{1}'][usize::from(b % 4)],
                _ => 'é',
            });
        }
        let mut chpx = Vec::new();
        if flags & 1 != 0 {
            chpx.extend(sprm::bold(true));
        }
        if flags & 2 != 0 {
            chpx.extend(sprm::rtl_run(true));
        }
        let mut papx = Vec::new();
        if flags & 4 != 0 {
            papx.extend(sprm::rtl_paragraph(true));
        }
        if flags & 8 != 0 {
            papx.extend(sprm::jc(u.int_in_range(0u8..=4).ok()?));
        }
        /* Issue #575 — with a style sheet: a paragraph style (Normal,
        heading 1, an empty slot, a character style), a character style
        and a font on the run, a toggle relative to the style. */
        let mut istd = 0;
        if styled {
            let st: u8 = u.arbitrary().ok()?;
            istd = [0, 1, 2, 15][usize::from(st % 4)];
            if st & 4 != 0 {
                chpx.extend(sprm::char_style(15));
            }
            if st & 8 != 0 {
                chpx.extend(sprm::font(u16::from(st >> 6)));
            }
            if st & 16 != 0 {
                chpx.extend(sprm::bold_operand(0x80 | (st >> 5 & 1)));
            }
        }
        paras.push(Para::new(vec![run(&text, chpx)], papx).styled(istd));
    }
    /* Issue #576 — bit 7: the paragraphs are the cells of one table row,
    closed by a row mark carrying a cell layout and table borders. */
    if flags & 128 != 0 {
        let cells = paras.len();
        for p in &mut paras {
            p.papx.extend(sprm::in_table(true));
            p.mark = '\u{7}';
        }
        let edges: Vec<i16> = (0..=cells).map(|i| i as i16 * 1440 - 108).collect();
        paras.push(Para::row_end(
            [
                sprm::def_table(&edges, &[]),
                sprm::table_borders80(sprm::brc80(4, 0x01, 0)),
            ]
            .concat(),
        ));
    }
    let mut spec = DocSpec::new(paras);
    let total = spec.ccp_text();
    if flags & 16 != 0 {
        spec.sections = vec![(total, sprm::page_size(12240, 15840))];
    }
    /* Split the text in two UTF-16 pieces stored in reverse order (a
    fast-saved layout), when there is something to split. */
    if flags & 32 != 0 && total >= 2 {
        let first = total / 2;
        spec.pieces = vec![
            PieceSpec {
                cps: first,
                encoding: Encoding::Utf16,
                prm: 0,
            },
            PieceSpec {
                cps: total - first,
                encoding: Encoding::Utf16,
                prm: 0,
            },
        ];
        spec.physical_order = vec![1, 0];
    }
    spec.one_table = flags & 64 == 0;
    if styled {
        let sample = styled_sample();
        spec.styles = sample.styles;
        spec.fonts = sample.fonts;
    }
    let (mut word, mut table) = streams(&spec);
    /* At most 64 overwrites: (stream, offset, value) — only while input
    is left (`Unstructured` answers 0 once it runs dry, which would turn
    every short input into the same `wIdent` overwrite). */
    for _ in 0..64 {
        if u.len() < 3 {
            break;
        }
        let Ok(at) = u.arbitrary::<u16>() else { break };
        let Ok(value) = u.arbitrary::<u8>() else {
            break;
        };
        let target = if at & 1 == 0 { &mut word } else { &mut table };
        let len = target.len();
        if len > 0 {
            target[usize::from(at >> 1) % len] = value;
        }
    }
    let table_name = if spec.one_table { "1Table" } else { "0Table" };
    Some(format_docx::opc::cfb::test_writer::build(&[
        ("WordDocument", &word),
        (table_name, &table),
    ]))
}
