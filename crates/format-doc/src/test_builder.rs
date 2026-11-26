//! Test support: build minimal Word 97-2003 `.doc` byte images in memory,
//! so no binary fixture is ever committed. Used by this crate's unit
//! tests, the engine's native open tests and the fuzz crate's seeds; never
//! by the reader.
//!
//! Layout of what [`build`] writes (every field per [MS-DOC]):
//!
//! - **`WordDocument` stream** — the FIB at byte 0 (`FibBase`: `wIdent`
//!   0xA5EC, `nFib` 0x00C1, the `fWhichTblStm` / `fComplex` /
//!   `fEncrypted` flags; `csw` 14 + `FibRgW97` zeros; `cslw` 22 +
//!   `FibRgLw97` with `cbMac` and `ccpText`; `cbRgFcLcb` 0x5D + the
//!   `FibRgFcLcb97` pairs of the structures below; `cswNew` 0), the text
//!   from byte 1024 piece by piece in the requested physical order (a
//!   compressed piece one Windows-1252 byte per CP, an uncompressed one a
//!   UTF-16LE unit per CP), then the CHPX FKP pages and the PAPX FKP pages
//!   (each 512 bytes, 512-aligned; a `PapxInFkp` carries its paragraph's
//!   [`Para::istd`]), then each section's `Sepx`.
//! - **`1Table` stream** (`0Table` when asked) — the `Clx` (one `Prc` per
//!   [`DocSpec::prcs`] entry, then the `Pcdt` / `PlcPcd`), `PlcBteChpx`,
//!   `PlcBtePapx` and `PlcfSed`, then — when the spec has them — the
//!   style sheet (`STSH`: `LPStshi` with an 18-byte `Stshif` and `ftcBi`,
//!   one `LPStd` per [`StyleSheetSpec::styles`] slot) and the font table
//!   (`SttbfFfn`: a 1-byte `cchData` and an `FFN` per [`DocSpec::fonts`]
//!   name).
//! - Both inside a version-3 compound file from `format-docx`'s
//!   `opc::cfb::test_writer`.

use crate::cp1252;

/// One run of text and the CHPX grpprl that formats it.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub text: String,
    pub chpx: Vec<u8>,
}

/// One paragraph: its runs, its PAPX grpprl, its paragraph style and the
/// mark that ends it (`'\r'`, or `'\u{7}'` for a cell / row end, `'\u{C}'`
/// for a section mark).
#[derive(Debug, Clone)]
pub struct Para {
    pub runs: Vec<Run>,
    pub papx: Vec<u8>,
    pub mark: char,
    /// `GrpPrlAndIstd.istd`: the paragraph style (0 = Normal).
    pub istd: u16,
    /// The paragraph mark's own CHPX grpprl (`Paragraph::mark_style`).
    pub mark_chpx: Vec<u8>,
}

impl Para {
    /// A paragraph of plain runs ending in `'\r'`, in style 0 (Normal).
    pub fn new(runs: Vec<Run>, papx: Vec<u8>) -> Para {
        Para {
            runs,
            papx,
            mark: '\r',
            istd: 0,
            mark_chpx: Vec::new(),
        }
    }

    /// The same paragraph in style `istd`.
    pub fn styled(self, istd: u16) -> Para {
        Para { istd, ..self }
    }

    /// A paragraph of one unformatted run.
    pub fn plain(text: &str) -> Para {
        Para::new(vec![run(text, Vec::new())], Vec::new())
    }
}

/// A run of `text` formatted by `chpx`.
pub fn run(text: &str, chpx: Vec<u8>) -> Run {
    Run {
        text: text.to_string(),
        chpx,
    }
}

/// How one piece stores its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// 8-bit Windows-1252, one byte per CP (`fCompressed` = 1).
    Compressed,
    /// UTF-16LE, two bytes per CP.
    Utf16,
}

/// One piece: `cps` consecutive CPs of the logical text.
#[derive(Debug, Clone, Copy)]
pub struct PieceSpec {
    pub cps: u32,
    pub encoding: Encoding,
    /// The piece's `Prm` (0 = none).
    pub prm: u16,
}

/// What to build.
#[derive(Debug, Clone)]
pub struct DocSpec {
    pub paragraphs: Vec<Para>,
    /// How the CP sequence is cut into pieces; the lengths must sum to the
    /// text length. Empty = one piece, compressed when every character
    /// encodes in Windows-1252, else UTF-16.
    pub pieces: Vec<PieceSpec>,
    /// The order the pieces' bytes are stored in (indices into `pieces`);
    /// empty = logical order. A fast-saved document stores them out of
    /// order.
    pub physical_order: Vec<usize>,
    /// `Prc` grpprls a Prm1 can name.
    pub prcs: Vec<Vec<u8>>,
    /// Sections: `(end_cp, sepx grpprl)`. Empty = no `PlcfSed`.
    pub sections: Vec<(u32, Vec<u8>)>,
    /// `FibBase.fComplex`.
    pub fast_saved: bool,
    /// `FibBase.fEncrypted` (nothing is actually encrypted).
    pub encrypted: bool,
    /// Store the table as `1Table` (`fWhichTblStm` = 1), else `0Table`.
    pub one_table: bool,
    /// The style sheet (`fcStshf`); `None` writes none.
    pub styles: Option<StyleSheetSpec>,
    /// The font table (`fcSttbfFfn`) by `ftc`; empty writes none.
    pub fonts: Vec<String>,
}

impl DocSpec {
    pub fn new(paragraphs: Vec<Para>) -> DocSpec {
        DocSpec {
            paragraphs,
            pieces: Vec::new(),
            physical_order: Vec::new(),
            prcs: Vec::new(),
            sections: Vec::new(),
            fast_saved: false,
            encrypted: false,
            one_table: true,
            styles: None,
            fonts: Vec::new(),
        }
    }

    /// The number of CPs of the main text (every UTF-16 unit of every run
    /// plus one mark per paragraph).
    pub fn ccp_text(&self) -> u32 {
        self.units().len() as u32
    }

    /// The logical text as `(UTF-16 unit, its CHPX grpprl, paragraph
    /// index)` — a run's grpprl for its text, the paragraph's `mark_chpx`
    /// for its mark.
    fn units(&self) -> Vec<(u16, &[u8], usize)> {
        let mut out = Vec::new();
        for (pi, p) in self.paragraphs.iter().enumerate() {
            for r in &p.runs {
                for u in r.text.encode_utf16() {
                    out.push((u, r.chpx.as_slice(), pi));
                }
            }
            let mut buf = [0u16; 2];
            for u in p.mark.encode_utf16(&mut buf) {
                out.push((*u, p.mark_chpx.as_slice(), pi));
            }
        }
        out
    }
}

/// `istdBase` / `istdNext` "no style".
pub const ISTD_NIL: u16 = 0x0FFF;
/// `sti` of a user-defined style.
pub const STI_USER: u16 = 0x0FFE;

/// One style of a [`StyleSheetSpec`].
#[derive(Debug, Clone)]
pub struct StyleSpec {
    /// `StdfBase.sti`: 0 Normal, 1-9 heading 1-9, 65 Default Paragraph
    /// Font, [`STI_USER`] a user style.
    pub sti: u16,
    /// `StdfBase.stk`: 1 paragraph, 2 character, 3 table, 4 numbering.
    pub stk: u8,
    /// `istdBase` ([`ISTD_NIL`]: none).
    pub base: u16,
    /// `istdNext` ([`ISTD_NIL`]: none).
    pub next: u16,
    /// `xstzName` (aliases after a comma).
    pub name: String,
    /// The `UpxPapx` grpprl (paragraph styles).
    pub papx: Vec<u8>,
    /// The `UpxChpx` grpprl (paragraph and character styles).
    pub chpx: Vec<u8>,
}

impl StyleSpec {
    /// A paragraph style.
    pub fn paragraph(sti: u16, name: &str, base: u16, papx: Vec<u8>, chpx: Vec<u8>) -> StyleSpec {
        StyleSpec {
            sti,
            stk: 1,
            base,
            next: ISTD_NIL,
            name: name.to_string(),
            papx,
            chpx,
        }
    }

    /// A character style.
    pub fn character(sti: u16, name: &str, base: u16, chpx: Vec<u8>) -> StyleSpec {
        StyleSpec {
            sti,
            stk: 2,
            base,
            next: ISTD_NIL,
            name: name.to_string(),
            papx: Vec::new(),
            chpx,
        }
    }
}

/// The style sheet ([`DocSpec::styles`]).
#[derive(Debug, Clone)]
pub struct StyleSheetSpec {
    /// The styles by `istd`; `None` writes an empty `LPStd`.
    pub styles: Vec<Option<StyleSpec>>,
    /// `Stshif.ftcAsci` / `ftcOther`: the default Latin font.
    pub ftc_ascii: u16,
    /// `STSHI.ftcBi`: the default complex-script font.
    pub ftc_bi: u16,
    /// `Stshif.cbSTDBaseInFile`: 10 (`StdfBase` only) or 18 (with
    /// `StdfPost2000`).
    pub std_base: u16,
}

impl StyleSheetSpec {
    /// A sheet of `styles` in the first font of the table.
    pub fn new(styles: Vec<Option<StyleSpec>>) -> StyleSheetSpec {
        StyleSheetSpec {
            styles,
            ftc_ascii: 0,
            ftc_bi: 0,
            std_base: 10,
        }
    }

    /// The `STSH` bytes.
    pub fn bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        /* LPStshi: cbStshi, Stshif (18 bytes), ftcBi. */
        v.extend_from_slice(&20u16.to_le_bytes());
        v.extend_from_slice(&(self.styles.len() as u16).to_le_bytes()); // cstd
        v.extend_from_slice(&self.std_base.to_le_bytes()); // cbSTDBaseInFile
        v.extend_from_slice(&1u16.to_le_bytes()); // fStdStylenamesWritten
        v.extend_from_slice(&0x005Bu16.to_le_bytes()); // stiMaxWhenSaved
        v.extend_from_slice(&0x000Fu16.to_le_bytes()); // istdMaxFixedWhenSaved
        v.extend_from_slice(&0u16.to_le_bytes()); // nVerBuiltInNamesWhenSaved
        v.extend_from_slice(&self.ftc_ascii.to_le_bytes()); // ftcAsci
        v.extend_from_slice(&self.ftc_ascii.to_le_bytes()); // ftcFE
        v.extend_from_slice(&self.ftc_ascii.to_le_bytes()); // ftcOther
        v.extend_from_slice(&self.ftc_bi.to_le_bytes()); // ftcBi
        for s in &self.styles {
            let Some(s) = s else {
                v.extend_from_slice(&0u16.to_le_bytes());
                continue;
            };
            let mut std = Vec::new();
            let cupx: u16 = match s.stk {
                1 => 2,
                2 => 1,
                _ => 0,
            };
            /* StdfBase: sti | flags, stk | istdBase, cupx | istdNext,
            bchUpe (patched below), grfstd; then StdfPost2000 (istdLink,
            rsid, iftcHtml + iPriority) as zeros when the sheet says so. */
            std.extend_from_slice(&(s.sti & 0x0FFF).to_le_bytes());
            std.extend_from_slice(&(u16::from(s.stk & 0x0F) | (s.base << 4)).to_le_bytes());
            std.extend_from_slice(&(cupx | (s.next << 4)).to_le_bytes());
            std.extend_from_slice(&[0, 0, 0, 0]);
            std.resize(usize::from(self.std_base.max(10)), 0);
            /* xstzName. */
            let name: Vec<u16> = s.name.encode_utf16().collect();
            std.extend_from_slice(&(name.len() as u16).to_le_bytes());
            for u in name.iter().chain(&[0]) {
                std.extend_from_slice(&u.to_le_bytes());
            }
            let upx = |std: &mut Vec<u8>, data: &[u8]| {
                std.extend_from_slice(&(data.len() as u16).to_le_bytes());
                std.extend_from_slice(data);
                if data.len() % 2 == 1 {
                    std.push(0);
                }
            };
            match s.stk {
                1 => {
                    /* UpxPapx: this style's istd (unused) + grpprl. */
                    let mut papx = 0u16.to_le_bytes().to_vec();
                    papx.extend_from_slice(&s.papx);
                    upx(&mut std, &papx);
                    upx(&mut std, &s.chpx);
                }
                2 => upx(&mut std, &s.chpx),
                _ => {}
            }
            let len = std.len() as u16;
            std[6..8].copy_from_slice(&len.to_le_bytes());
            v.extend_from_slice(&len.to_le_bytes());
            v.extend_from_slice(&std);
        }
        v
    }
}

/// The `SttbfFfn` of `fonts`: a non-extended STTB whose entries are FFNs
/// (a TrueType face of the swiss family, weight 400, then the name).
pub fn font_table(fonts: &[String]) -> Vec<u8> {
    let mut v = (fonts.len() as u16).to_le_bytes().to_vec();
    v.extend_from_slice(&0u16.to_le_bytes()); // cbExtra
    for f in fonts {
        let mut ffn = vec![0u8; 39];
        ffn[0] = 0x26; // prq 2, fTrueType, ff 2
        ffn[1..3].copy_from_slice(&400u16.to_le_bytes());
        for u in f.encode_utf16().chain([0]) {
            ffn.extend_from_slice(&u.to_le_bytes());
        }
        v.push(ffn.len() as u8);
        v.extend_from_slice(&ffn);
    }
    v
}

/// Property-modifier builders ([MS-DOC] 2.6): each returns one `Prl`.
pub mod sprm {
    fn prl(op: u16, operand: &[u8]) -> Vec<u8> {
        let mut v = op.to_le_bytes().to_vec();
        v.extend_from_slice(operand);
        v
    }
    pub fn bold(on: bool) -> Vec<u8> {
        prl(0x0835, &[u8::from(on)])
    }
    pub fn italic(on: bool) -> Vec<u8> {
        prl(0x0836, &[u8::from(on)])
    }
    pub fn strike(on: bool) -> Vec<u8> {
        prl(0x0837, &[u8::from(on)])
    }
    /// `sprmCKul`: 1 single, 3 double, 4 dotted, 11 wave, …
    pub fn underline(kul: u8) -> Vec<u8> {
        prl(0x2A3E, &[kul])
    }
    /// `sprmCHps`: the size in half-points.
    pub fn size_half_points(hps: u16) -> Vec<u8> {
        prl(0x4A43, &hps.to_le_bytes())
    }
    /// `sprmCHpsBi`: the complex-script size in half-points.
    pub fn size_cs_half_points(hps: u16) -> Vec<u8> {
        prl(0x4A61, &hps.to_le_bytes())
    }
    /// `sprmCIco`: a palette colour (6 = red).
    pub fn ico(i: u8) -> Vec<u8> {
        prl(0x2A42, &[i])
    }
    /// `sprmCCv`: an RGB colour.
    pub fn color(r: u8, g: u8, b: u8) -> Vec<u8> {
        prl(0x6870, &[r, g, b, 0])
    }
    /// `sprmCFBiDi`: a right-to-left run.
    pub fn rtl_run(on: bool) -> Vec<u8> {
        prl(0x085A, &[u8::from(on)])
    }
    /// `sprmCFBoldBi`.
    pub fn bold_cs(on: bool) -> Vec<u8> {
        prl(0x085C, &[u8::from(on)])
    }
    /// `sprmPJc` (logical): 0 start, 1 center, 2 end, 3 justify.
    pub fn jc(v: u8) -> Vec<u8> {
        prl(0x2461, &[v])
    }
    /// `sprmPJc80` (physical): 0 left, 1 center, 2 right, 3 justify.
    pub fn jc80(v: u8) -> Vec<u8> {
        prl(0x2403, &[v])
    }
    /// `sprmPFBiDi`: a right-to-left paragraph.
    pub fn rtl_paragraph(on: bool) -> Vec<u8> {
        prl(0x2441, &[u8::from(on)])
    }
    /// `sprmPDxaLeft` (twips, signed).
    pub fn indent_start(tw: i16) -> Vec<u8> {
        prl(0x845E, &tw.to_le_bytes())
    }
    /// `sprmPDxaRight` (twips, signed).
    pub fn indent_end(tw: i16) -> Vec<u8> {
        prl(0x845D, &tw.to_le_bytes())
    }
    /// `sprmPDxaLeft1` (twips; negative = hanging).
    pub fn indent_first_line(tw: i16) -> Vec<u8> {
        prl(0x8460, &tw.to_le_bytes())
    }
    /// `sprmPDyaBefore` (twips).
    pub fn space_before(tw: u16) -> Vec<u8> {
        prl(0xA413, &tw.to_le_bytes())
    }
    /// `sprmPDyaAfter` (twips).
    pub fn space_after(tw: u16) -> Vec<u8> {
        prl(0xA414, &tw.to_le_bytes())
    }
    /// `sprmPDyaLine`: an LSPD (`dyaLine`, `fMultLinespace`).
    pub fn line_spacing(dya_line: i16, multiple: bool) -> Vec<u8> {
        let mut op = dya_line.to_le_bytes().to_vec();
        op.extend_from_slice(&u16::from(multiple).to_le_bytes());
        prl(0x6412, &op)
    }
    /// `sprmPFInTable`.
    pub fn in_table(on: bool) -> Vec<u8> {
        prl(0x2416, &[u8::from(on)])
    }
    /// `sprmPFTtp`: a table row's end mark.
    pub fn row_end(on: bool) -> Vec<u8> {
        prl(0x2417, &[u8::from(on)])
    }
    /// `sprmPIlfo`: list membership.
    pub fn list(ilfo: i16) -> Vec<u8> {
        prl(0x460B, &ilfo.to_le_bytes())
    }
    /// `sprmSXaPage` / `sprmSYaPage` (twips).
    pub fn page_size(w: u16, h: u16) -> Vec<u8> {
        let mut v = prl(0xB01F, &w.to_le_bytes());
        v.extend(prl(0xB020, &h.to_le_bytes()));
        v
    }
    /// `sprmSDxaLeft` / `Right`, `sprmSDyaTop` / `Bottom` (twips).
    pub fn margins(left: u16, right: u16, top: i16, bottom: i16) -> Vec<u8> {
        let mut v = prl(0xB021, &left.to_le_bytes());
        v.extend(prl(0xB022, &right.to_le_bytes()));
        v.extend(prl(0x9023, &top.to_le_bytes()));
        v.extend(prl(0x9024, &bottom.to_le_bytes()));
        v
    }
    /// `sprmSBkc`: 0 continuous, 2 new page, 3 even, 4 odd.
    pub fn section_start(bkc: u8) -> Vec<u8> {
        prl(0x3009, &[bkc])
    }
    /// `sprmCFBold` with a raw ToggleOperand (0x80 "the style's value",
    /// 0x81 "the opposite of the style's value").
    pub fn bold_operand(operand: u8) -> Vec<u8> {
        prl(0x0835, &[operand])
    }
    /// `sprmCFItalic` with a raw ToggleOperand.
    pub fn italic_operand(operand: u8) -> Vec<u8> {
        prl(0x0836, &[operand])
    }
    /// `sprmCFCaps`.
    pub fn caps(on: bool) -> Vec<u8> {
        prl(0x083B, &[u8::from(on)])
    }
    /// `sprmCFSmallCaps`.
    pub fn small_caps(on: bool) -> Vec<u8> {
        prl(0x083A, &[u8::from(on)])
    }
    /// `sprmCIss`: 0 normal, 1 superscript, 2 subscript.
    pub fn vert_align(iss: u8) -> Vec<u8> {
        prl(0x2A48, &[iss])
    }
    /// `sprmCHighlight`: an Ico (7 = yellow).
    pub fn highlight(ico: u8) -> Vec<u8> {
        prl(0x2A0C, &[ico])
    }
    /// `sprmCIstd`: the run's character style.
    pub fn char_style(istd: u16) -> Vec<u8> {
        prl(0x4A30, &istd.to_le_bytes())
    }
    /// `sprmCRgFtc0`: the ASCII font (an index into the font table).
    pub fn font(ftc: u16) -> Vec<u8> {
        prl(0x4A4F, &ftc.to_le_bytes())
    }
    /// `sprmCFtcBi`: the complex-script font.
    pub fn font_cs(ftc: u16) -> Vec<u8> {
        prl(0x4A5E, &ftc.to_le_bytes())
    }
    /// `sprmPIstd`: the paragraph style (overrides the PAPX's istd).
    pub fn paragraph_style(istd: u16) -> Vec<u8> {
        prl(0x4600, &istd.to_le_bytes())
    }
    /// `sprmPFKeep`.
    pub fn keep_lines(on: bool) -> Vec<u8> {
        prl(0x2405, &[u8::from(on)])
    }
    /// `sprmPFKeepFollow`.
    pub fn keep_next(on: bool) -> Vec<u8> {
        prl(0x2406, &[u8::from(on)])
    }
    /// `sprmPFPageBreakBefore`.
    pub fn page_break_before(on: bool) -> Vec<u8> {
        prl(0x2407, &[u8::from(on)])
    }
    /// `sprmPFWidowControl`.
    pub fn widow_control(on: bool) -> Vec<u8> {
        prl(0x2431, &[u8::from(on)])
    }
    /// `sprmPOutLvl`: 0-8, 9 body text.
    pub fn outline_level(lvl: u8) -> Vec<u8> {
        prl(0x2640, &[lvl])
    }
}

/// The fonts of [`styled_sample`], by `ftc`.
pub const SAMPLE_FONTS: [&str; 3] = ["Times New Roman", "Arial", "Simplified Arabic"];
/// The paragraph texts of [`styled_sample`].
pub const SAMPLE_TEXTS: [&str; 4] = [
    "Plain body text",
    "Chapter One",
    "Before quoted after",
    "Left to right",
];

/// A styled document (issue #575): Normal (`istd` 0) is right-to-left
/// with 8 pt after and 1.15-line spacing, in 12 pt Arial (14 pt
/// Simplified Arabic for complex script); heading 1 (`istd` 1, based on
/// Normal, followed by Normal) keeps with the next paragraph, has 12 pt
/// before and an outline level, in bold 16 pt; "Quote Char" (`istd` 15, a
/// user character style based on Default Paragraph Font, `istd` 10) is
/// italic dark red. The document defaults are Times New Roman / Simplified
/// Arabic. Paragraphs ([`SAMPLE_TEXTS`]): an unformatted Normal one, a
/// heading, a Normal one whose middle word is in "Quote Char", and a
/// Normal one made left-to-right by direct formatting.
pub fn styled_sample() -> DocSpec {
    let cat = |v: &[Vec<u8>]| v.concat();
    let mut styles: Vec<Option<StyleSpec>> = vec![None; 16];
    styles[0] = Some(StyleSpec::paragraph(
        0,
        "Normal",
        ISTD_NIL,
        cat(&[
            sprm::rtl_paragraph(true),
            sprm::space_after(160),
            sprm::line_spacing(276, true),
        ]),
        cat(&[
            sprm::font(1),
            sprm::size_half_points(24),
            sprm::font_cs(2),
            sprm::size_cs_half_points(28),
        ]),
    ));
    styles[1] = Some(StyleSpec {
        next: 0,
        ..StyleSpec::paragraph(
            1,
            "heading 1,h1",
            0,
            cat(&[
                sprm::keep_next(true),
                sprm::space_before(240),
                sprm::outline_level(0),
            ]),
            cat(&[
                sprm::bold(true),
                sprm::bold_cs(true),
                sprm::size_half_points(32),
            ]),
        )
    });
    styles[10] = Some(StyleSpec::character(
        65,
        "Default Paragraph Font",
        ISTD_NIL,
        Vec::new(),
    ));
    styles[15] = Some(StyleSpec::character(
        STI_USER,
        "Quote Char",
        10,
        cat(&[sprm::italic(true), sprm::color(0x80, 0, 0)]),
    ));
    DocSpec {
        styles: Some(StyleSheetSpec {
            ftc_ascii: 0,
            ftc_bi: 2,
            ..StyleSheetSpec::new(styles)
        }),
        fonts: SAMPLE_FONTS.iter().map(|f| f.to_string()).collect(),
        ..DocSpec::new(vec![
            Para::plain(SAMPLE_TEXTS[0]),
            Para::plain(SAMPLE_TEXTS[1]).styled(1),
            Para::new(
                vec![
                    run("Before ", Vec::new()),
                    run("quoted", sprm::char_style(15)),
                    run(" after", Vec::new()),
                ],
                Vec::new(),
            ),
            Para::new(
                vec![run(SAMPLE_TEXTS[3], Vec::new())],
                sprm::rtl_paragraph(false),
            ),
        ])
    }
}

/// A `Prm0` (2.9.215): `isprm` with a one-byte operand.
pub fn prm0(isprm: u8, val: u8) -> u16 {
    (u16::from(isprm & 0x7F) << 1) | (u16::from(val) << 8)
}

/// A `Prm1` (2.9.216): the index of a `Prc`.
pub fn prm1(prc: u16) -> u16 {
    (prc << 1) | 1
}

fn put_u16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put_u32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn align(v: &mut Vec<u8>, to: usize) {
    while v.len() % to != 0 {
        v.push(0);
    }
}

/// One FKP entry: bytes `[fc_start, fc_end)` formatted by `grpprl`
/// (`None` = no properties).
struct Entry<'a> {
    fc_start: u32,
    fc_end: u32,
    grpprl: Option<&'a [u8]>,
}

/// Merge per-CP byte ranges (sorted by FC) into FKP entries, filling the
/// gaps between pieces with unformatted entries so the FCs stay
/// contiguous, as an FKP's `rgfc` requires.
fn entries<'a>(mut ranges: Vec<(u32, u32, Option<&'a [u8]>)>) -> Vec<Entry<'a>> {
    ranges.sort_by_key(|r| r.0);
    let mut out: Vec<Entry<'a>> = Vec::new();
    for (s, e, g) in ranges {
        if let Some(last) = out.last_mut() {
            if last.fc_end == s && last.grpprl == g {
                last.fc_end = e;
                continue;
            }
            let gap_from = last.fc_end;
            if gap_from < s {
                out.push(Entry {
                    fc_start: gap_from,
                    fc_end: s,
                    grpprl: None,
                });
            }
        }
        out.push(Entry {
            fc_start: s,
            fc_end: e,
            grpprl: g,
        });
    }
    out
}

/// Pack `entries` into FKP pages appended to `word`; returns the bin
/// table (aFc[n + 1] + aPn[n]).
fn write_fkps(word: &mut Vec<u8>, entries: &[Entry<'_>], papx: bool) -> Vec<u8> {
    let max_runs = if papx { 0x1D } else { 0x65 };
    let per_run = if papx { 13 } else { 1 };
    let mut fcs: Vec<u32> = Vec::new();
    let mut pns: Vec<u32> = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        align(word, 512);
        let page_at = word.len();
        let mut page = vec![0u8; 512];
        /* Property data grows down from byte 511 (the crun byte). */
        let mut top = 511usize;
        let mut offsets: Vec<u8> = Vec::new();
        let first = i;
        while i < entries.len() && offsets.len() < max_runs {
            let crun = offsets.len() + 1;
            let head = 4 * (crun + 1) + per_run * crun;
            let data: Vec<u8> = match entries[i].grpprl {
                None if !papx => Vec::new(),
                g => {
                    if papx {
                        /* PapxInFkp: cb, [cb'], GrpPrlAndIstd (a gap
                        between pieces: istd 0, no sprms). */
                        let g = g.unwrap_or(&[0, 0]);
                        let len = g.len();
                        let mut d = Vec::new();
                        if len % 2 == 1 {
                            d.push(len.div_ceil(2) as u8);
                        } else {
                            d.push(0);
                            d.push((len / 2) as u8);
                        }
                        d.extend_from_slice(g);
                        d
                    } else {
                        let g = g.unwrap_or(&[]);
                        let mut d = vec![g.len() as u8];
                        d.extend_from_slice(g);
                        d
                    }
                }
            };
            let need_top = if data.is_empty() {
                top
            } else {
                top.checked_sub(data.len()).map_or(0, |t| t & !1)
            };
            if need_top < head {
                break;
            }
            if data.is_empty() {
                offsets.push(0);
            } else {
                page[need_top..need_top + data.len()].copy_from_slice(&data);
                offsets.push((need_top / 2) as u8);
                top = need_top;
            }
            i += 1;
        }
        assert!(i > first, "an FKP entry does not fit a page");
        let crun = offsets.len();
        for (k, e) in entries[first..i].iter().enumerate() {
            put_u32(&mut page, 4 * k, e.fc_start);
        }
        put_u32(&mut page, 4 * crun, entries[i - 1].fc_end);
        for (k, off) in offsets.iter().enumerate() {
            page[4 * (crun + 1) + per_run * k] = *off;
        }
        page[511] = crun as u8;
        word.extend_from_slice(&page);
        fcs.push(entries[first].fc_start);
        pns.push((page_at / 512) as u32);
    }
    if let Some(last) = entries.last() {
        fcs.push(last.fc_end);
    }
    let mut plc = Vec::new();
    for fc in &fcs {
        plc.extend_from_slice(&fc.to_le_bytes());
    }
    for pn in &pns {
        plc.extend_from_slice(&pn.to_le_bytes());
    }
    plc
}

/// Byte offset of the text in the `WordDocument` stream.
const TEXT_AT: usize = 1024;
/// `cbRgFcLcb` for `nFib` 0x00C1.
const CB_RG_FC_LCB: usize = 0x5D;

/// The two streams of `spec`: `(WordDocument, table stream)`.
pub fn streams(spec: &DocSpec) -> (Vec<u8>, Vec<u8>) {
    let units = spec.units();
    let total = units.len() as u32;
    let encodable = |u: u16| {
        char::from_u32(u32::from(u))
            .and_then(cp1252::encode)
            .is_some()
    };
    let pieces: Vec<PieceSpec> = if spec.pieces.is_empty() {
        vec![PieceSpec {
            cps: total,
            encoding: if units.iter().all(|(u, _, _)| encodable(*u)) {
                Encoding::Compressed
            } else {
                Encoding::Utf16
            },
            prm: 0,
        }]
    } else {
        spec.pieces.clone()
    };
    assert_eq!(
        pieces.iter().map(|p| p.cps).sum::<u32>(),
        total,
        "the pieces must cover the text exactly"
    );
    let order: Vec<usize> = if spec.physical_order.is_empty() {
        (0..pieces.len()).collect()
    } else {
        spec.physical_order.clone()
    };

    /* The text, piece by piece in physical order. */
    let mut word = vec![0u8; TEXT_AT];
    let mut cp_starts = Vec::with_capacity(pieces.len());
    let mut acc = 0u32;
    for p in &pieces {
        cp_starts.push(acc);
        acc += p.cps;
    }
    let mut piece_byte = vec![0u32; pieces.len()];
    /* Per CP: (byte fc, byte length). */
    let mut cp_fc = vec![(0u32, 0u32); units.len()];
    for &pi in &order {
        let p = pieces[pi];
        align(&mut word, 2);
        piece_byte[pi] = word.len() as u32;
        for k in 0..p.cps {
            let cp = (cp_starts[pi] + k) as usize;
            let u = units[cp].0;
            let at = word.len() as u32;
            match p.encoding {
                Encoding::Compressed => {
                    let b = char::from_u32(u32::from(u))
                        .and_then(cp1252::encode)
                        .expect("a compressed piece holds only Windows-1252 characters");
                    word.push(b);
                    cp_fc[cp] = (at, 1);
                }
                Encoding::Utf16 => {
                    word.extend_from_slice(&u.to_le_bytes());
                    cp_fc[cp] = (at, 2);
                }
            }
        }
    }

    /* FKPs: one range per CP, merged. */
    let chpx_ranges = units
        .iter()
        .enumerate()
        .map(|(cp, (_, g, _))| {
            let (fc, len) = cp_fc[cp];
            (fc, fc + len, Some(*g).filter(|g| !g.is_empty()))
        })
        .collect();
    /* GrpPrlAndIstd per paragraph: its istd, then its grpprl. */
    let grpprl_and_istd: Vec<Vec<u8>> = spec
        .paragraphs
        .iter()
        .map(|p| {
            let mut d = p.istd.to_le_bytes().to_vec();
            d.extend_from_slice(&p.papx);
            d
        })
        .collect();
    let papx_ranges = units
        .iter()
        .enumerate()
        .map(|(cp, (_, _, pi))| {
            let (fc, len) = cp_fc[cp];
            (fc, fc + len, Some(grpprl_and_istd[*pi].as_slice()))
        })
        .collect();
    let plc_chpx = write_fkps(&mut word, &entries(chpx_ranges), false);
    let plc_papx = write_fkps(&mut word, &entries(papx_ranges), true);

    /* Sepx: cb + grpprl, at even offsets. */
    let mut sepx_at = Vec::new();
    for (_, g) in &spec.sections {
        align(&mut word, 2);
        sepx_at.push(word.len() as u32);
        word.extend_from_slice(&(g.len() as u16).to_le_bytes());
        word.extend_from_slice(g);
    }

    /* The table stream. */
    let mut table = Vec::new();
    for g in &spec.prcs {
        table.push(0x01);
        table.extend_from_slice(&(g.len() as u16).to_le_bytes());
        table.extend_from_slice(g);
    }
    let mut plc_pcd = Vec::new();
    for s in cp_starts.iter().chain(std::iter::once(&total)) {
        plc_pcd.extend_from_slice(&s.to_le_bytes());
    }
    for (pi, p) in pieces.iter().enumerate() {
        let fc = match p.encoding {
            Encoding::Compressed => (piece_byte[pi] * 2) | 0x4000_0000,
            Encoding::Utf16 => piece_byte[pi],
        };
        plc_pcd.extend_from_slice(&0u16.to_le_bytes());
        plc_pcd.extend_from_slice(&fc.to_le_bytes());
        plc_pcd.extend_from_slice(&p.prm.to_le_bytes());
    }
    table.push(0x02);
    table.extend_from_slice(&(plc_pcd.len() as u32).to_le_bytes());
    table.extend_from_slice(&plc_pcd);
    let clx = (0u32, table.len() as u32);
    let place = |table: &mut Vec<u8>, bytes: &[u8]| -> (u32, u32) {
        align(table, 2);
        let at = table.len() as u32;
        table.extend_from_slice(bytes);
        (at, bytes.len() as u32)
    };
    let bte_chpx = place(&mut table, &plc_chpx);
    let bte_papx = place(&mut table, &plc_papx);
    let sed = if spec.sections.is_empty() {
        (0, 0)
    } else {
        let mut plc = 0u32.to_le_bytes().to_vec();
        for (end, _) in &spec.sections {
            plc.extend_from_slice(&end.to_le_bytes());
        }
        for at in &sepx_at {
            plc.extend_from_slice(&0u16.to_le_bytes()); // fn
            plc.extend_from_slice(&at.to_le_bytes()); // fcSepx
            plc.extend_from_slice(&0u16.to_le_bytes()); // fnMpr
            plc.extend_from_slice(&u32::MAX.to_le_bytes()); // fcMpr
        }
        place(&mut table, &plc)
    };
    let stsh = spec
        .styles
        .as_ref()
        .map_or((0, 0), |s| place(&mut table, &s.bytes()));
    let ffn = if spec.fonts.is_empty() {
        (0, 0)
    } else {
        place(&mut table, &font_table(&spec.fonts))
    };

    /* The FIB. */
    put_u16(&mut word, 0x00, 0xA5EC); // wIdent
    put_u16(&mut word, 0x02, 0x00C1); // nFib
    put_u16(&mut word, 0x06, 0x0409); // lid
    let mut flags = 0u16;
    if spec.one_table {
        flags |= 0x0200;
    }
    if spec.fast_saved {
        flags |= 0x0004;
    }
    if spec.encrypted {
        flags |= 0x0100;
    }
    put_u16(&mut word, 0x0A, flags);
    put_u16(&mut word, 0x0C, 0x00BF); // nFibBack
    put_u16(&mut word, 0x20, 0x000E); // csw
    put_u16(&mut word, 0x3E, 0x0016); // cslw
    let len = word.len() as u32;
    put_u32(&mut word, 0x40, len); // cbMac
    put_u32(&mut word, 0x40 + 3 * 4, total); // ccpText
    put_u16(&mut word, 0x98, CB_RG_FC_LCB as u16);
    let mut pair = |i: usize, (fc, lcb): (u32, u32)| {
        put_u32(&mut word, 0x9A + 8 * i, fc);
        put_u32(&mut word, 0x9A + 8 * i + 4, lcb);
    };
    pair(6, sed);
    pair(12, bte_chpx);
    pair(13, bte_papx);
    pair(1, stsh);
    pair(15, ffn);
    pair(33, clx);
    /* cswNew = 0 right after the blob. */
    put_u16(&mut word, 0x9A + 8 * CB_RG_FC_LCB, 0);
    (word, table)
}

/// The `.doc` file of `spec`.
pub fn build(spec: &DocSpec) -> Vec<u8> {
    let (word, table) = streams(spec);
    let table_name = if spec.one_table { "1Table" } else { "0Table" };
    format_docx::opc::cfb::test_writer::build(&[("WordDocument", &word), (table_name, &table)])
}
