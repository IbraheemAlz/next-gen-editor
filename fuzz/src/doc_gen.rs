//! Issue #567 — inputs for the `doc_reader` target (legacy Word 97-2003
//! `.doc` import, `format-doc`).
//!
//! NOT a structure-aware generator (that is a follow-up issue): the target
//! reads raw bytes (a committed seed or a mutation of one) as the file
//! itself, and every other input as a SPLICE — a small valid image built by
//! `format_doc::test_builder` from the input's head (text, run / paragraph
//! flags, an optional section and an out-of-order piece split), then the
//! rest of the input overwriting bytes of its two streams at input-chosen
//! offsets before the compound file is assembled. That keeps the random
//! sweep inside the FIB / piece table / FKP / SEPX parsers instead of
//! failing on the container signature.

use arbitrary::Unstructured;
use format_doc::test_builder::{DocSpec, Encoding, Para, PieceSpec, build, run, sprm, streams};

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
    vec![
        ("seed_plain.doc", build(&plain)),
        ("seed_rtl_section.doc", build(&rtl)),
        ("seed_fast_saved.doc", build(&fast_saved)),
    ]
}

/// A splice input: a valid image from the head of `data`, then byte
/// overwrites of its streams from the rest. `None` when `data` is too
/// short to choose anything.
pub fn spliced(data: &[u8]) -> Option<Vec<u8>> {
    let mut u = Unstructured::new(data);
    let flags: u8 = u.arbitrary().ok()?;
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
        paras.push(Para::new(vec![run(&text, chpx)], papx));
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
