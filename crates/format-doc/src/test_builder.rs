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
//!   (each 512 bytes, 512-aligned), then each section's `Sepx`.
//! - **`1Table` stream** (`0Table` when asked) — the `Clx` (one `Prc` per
//!   [`DocSpec::prcs`] entry, then the `Pcdt` / `PlcPcd`), `PlcBteChpx`,
//!   `PlcBtePapx` and `PlcfSed`.
//! - **`Data` stream**, only when [`DocSpec::data`] is not empty (what a
//!   `sprmPHugePapx` points into).
//! - All inside a version-3 compound file from `format-docx`'s
//!   `opc::cfb::test_writer`.
//!
//! Tables (issue #576): a cell's last paragraph ends with
//! [`Para::cell`] (0x07 + `sprmPFInTable`), a row with [`Para::row_end`]
//! (0x07 + `sprmPFTtp` + the row's table sprms, [`sprm::def_table`] …); a
//! nested table's paragraphs carry [`sprm::itap`] and end their cells /
//! rows with [`sprm::inner_cell`] / [`sprm::inner_row_end`] on a `'\r'`
//! mark.

use crate::cp1252;

/// One run of text and the CHPX grpprl that formats it.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub text: String,
    pub chpx: Vec<u8>,
}

/// One paragraph: its runs, its PAPX grpprl and the mark that ends it
/// (`'\r'`, or `'\u{7}'` for a cell / row end, `'\u{C}'` for a section
/// mark).
#[derive(Debug, Clone)]
pub struct Para {
    pub runs: Vec<Run>,
    pub papx: Vec<u8>,
    pub mark: char,
}

impl Para {
    /// A paragraph of plain runs ending in `'\r'`.
    pub fn new(runs: Vec<Run>, papx: Vec<u8>) -> Para {
        Para {
            runs,
            papx,
            mark: '\r',
        }
    }

    /// A paragraph of one unformatted run.
    pub fn plain(text: &str) -> Para {
        Para::new(vec![run(text, Vec::new())], Vec::new())
    }

    /// The last paragraph of a depth-1 table cell: `text`, a 0x07 cell
    /// mark, `sprmPFInTable` plus `papx`.
    pub fn cell(text: &str, papx: Vec<u8>) -> Para {
        let mut g = sprm::in_table(true);
        g.extend(papx);
        Para {
            runs: vec![run(text, Vec::new())],
            papx: g,
            mark: '\u{7}',
        }
    }

    /// A depth-1 row mark (TTP): an empty 0x07 paragraph with
    /// `sprmPFInTable`, `sprmPFTtp` and the row's table sprms `tap`.
    pub fn row_end(tap: Vec<u8>) -> Para {
        let mut g = sprm::in_table(true);
        g.extend(sprm::row_end(true));
        g.extend(tap);
        Para {
            runs: Vec::new(),
            papx: g,
            mark: '\u{7}',
        }
    }
}

/// One `TC80` ([MS-DOC] 2.9.316) of [`sprm::def_table`]; the default is
/// a plain, unmerged cell with no borders and no preferred width.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tc {
    /// `TCGRF.horzMerge`: 0 not merged, 1 merged into the cell before,
    /// 2 first cell of a merged set.
    pub horz_merge: u8,
    /// `TCGRF.vertMerge`: 0 not merged, 1 continues the cell above,
    /// 3 starts a vertical merge.
    pub vert_merge: u8,
    /// `TCGRF.vertAlign`: 0 top, 1 center, 2 bottom.
    pub v_align: u8,
    /// `TCGRF.ftsWidth` + `wWidth`: the preferred width (3 = twips).
    pub fts_width: u8,
    pub w_width: u16,
    /// Top, left, bottom, right `Brc80MayBeNil`s, raw.
    pub borders: [[u8; 4]; 4],
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
    /// The `Data` stream's bytes (written only when not empty).
    pub data: Vec<u8>,
    /// `FibBase.fComplex`.
    pub fast_saved: bool,
    /// `FibBase.fEncrypted` (nothing is actually encrypted).
    pub encrypted: bool,
    /// Store the table as `1Table` (`fWhichTblStm` = 1), else `0Table`.
    pub one_table: bool,
}

impl DocSpec {
    pub fn new(paragraphs: Vec<Para>) -> DocSpec {
        DocSpec {
            paragraphs,
            pieces: Vec::new(),
            physical_order: Vec::new(),
            prcs: Vec::new(),
            sections: Vec::new(),
            data: Vec::new(),
            fast_saved: false,
            encrypted: false,
            one_table: true,
        }
    }

    /// The number of CPs of the main text (every UTF-16 unit of every run
    /// plus one mark per paragraph).
    pub fn ccp_text(&self) -> u32 {
        self.units().len() as u32
    }

    /// The logical text as `(UTF-16 unit, chpx index, paragraph index)`.
    fn units(&self) -> Vec<(u16, Option<usize>, usize)> {
        let mut out = Vec::new();
        let mut chpx_index = 0usize;
        for (pi, p) in self.paragraphs.iter().enumerate() {
            for r in &p.runs {
                for u in r.text.encode_utf16() {
                    out.push((u, Some(chpx_index), pi));
                }
                chpx_index += 1;
            }
            let mut buf = [0u16; 2];
            for u in p.mark.encode_utf16(&mut buf) {
                out.push((*u, None, pi));
            }
        }
        out
    }

    fn chpxs(&self) -> Vec<&[u8]> {
        self.paragraphs
            .iter()
            .flat_map(|p| p.runs.iter().map(|r| r.chpx.as_slice()))
            .collect()
    }
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
    /// `sprmPItap`: the table depth.
    pub fn itap(depth: i32) -> Vec<u8> {
        prl(0x6649, &depth.to_le_bytes())
    }
    /// `sprmPFInnerTableCell`: this paragraph mark ends a nested cell.
    pub fn inner_cell(on: bool) -> Vec<u8> {
        prl(0x244B, &[u8::from(on)])
    }
    /// `sprmPFInnerTtp`: this paragraph mark ends a nested row.
    pub fn inner_row_end(on: bool) -> Vec<u8> {
        prl(0x244C, &[u8::from(on)])
    }
    /// `sprmPHugePapx`: the paragraph's properties are the `PrcData` at
    /// `offset` of the Data stream (see [`super::prc_data`]).
    pub fn huge_papx(offset: u32) -> Vec<u8> {
        prl(0x6646, &offset.to_le_bytes())
    }
    /// `sprmTDefTable`: the cell edges (twips from the left margin, one
    /// more than the cells) and one `TC80` per cell (missing ones are the
    /// default).
    pub fn def_table(edges: &[i16], tcs: &[super::Tc]) -> Vec<u8> {
        let cells = edges.len().saturating_sub(1);
        let mut body = vec![cells as u8];
        for e in edges {
            body.extend_from_slice(&e.to_le_bytes());
        }
        for tc in tcs {
            let grf = u16::from(tc.horz_merge & 3)
                | (u16::from(tc.vert_merge & 3) << 5)
                | (u16::from(tc.v_align & 3) << 7)
                | (u16::from(tc.fts_width & 7) << 9);
            body.extend_from_slice(&grf.to_le_bytes());
            body.extend_from_slice(&tc.w_width.to_le_bytes());
            for b in tc.borders {
                body.extend_from_slice(&b);
            }
        }
        let mut operand = ((body.len() + 1) as u16).to_le_bytes().to_vec();
        operand.extend(body);
        prl(0xD608, &operand)
    }
    /// `sprmTTableBorders80`: the same `Brc80` on all six table edges.
    pub fn table_borders80(brc: [u8; 4]) -> Vec<u8> {
        let mut operand = vec![0x18];
        for _ in 0..6 {
            operand.extend_from_slice(&brc);
        }
        prl(0xD605, &operand)
    }
    /// A `Brc80`: width in eighths of a point, `BrcType`, `Ico`.
    pub fn brc80(width: u8, brc_type: u8, ico: u8) -> [u8; 4] {
        [width, brc_type, ico, 0]
    }
    /// `sprmTSetShd`: cells `first .. lim` shaded with a solid RGB fill.
    pub fn set_shd(first: u8, lim: u8, rgb: [u8; 3]) -> Vec<u8> {
        let mut operand = vec![12, first, lim];
        operand.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 0]);
        operand.extend_from_slice(&[0, 0, 0, 0xFF]);
        operand.extend_from_slice(&1u16.to_le_bytes());
        prl(0xD62D, &operand)
    }
    /// `sprmTVertMerge`: cell `itc` of the row — 1 continues the cell
    /// above, 3 starts a merge.
    pub fn vert_merge(itc: u8, flag: u8) -> Vec<u8> {
        prl(0xD62B, &[2, itc, flag])
    }
    /// `sprmTJc` (logical): 0 leading edge, 1 centered, 2 trailing edge.
    pub fn table_jc(jc: u16) -> Vec<u8> {
        prl(0x548A, &jc.to_le_bytes())
    }
    /// `sprmTFBiDi`: a right-to-left table.
    pub fn table_rtl(on: bool) -> Vec<u8> {
        prl(0x560B, &u16::from(on).to_le_bytes())
    }
    /// `sprmTDyaRowHeight`: positive "at least", negative "exact" (twips).
    pub fn row_height(dya: i16) -> Vec<u8> {
        prl(0x9407, &dya.to_le_bytes())
    }
    /// `sprmTTableHeader`: a header row.
    pub fn header_row(on: bool) -> Vec<u8> {
        prl(0x3404, &[u8::from(on)])
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
}

/// A `PrcData` (2.9.209): `cbGrpprl` then the grpprl — what a
/// `sprmPHugePapx` points at in the Data stream.
pub fn prc_data(grpprl: &[u8]) -> Vec<u8> {
    let mut v = (grpprl.len() as u16).to_le_bytes().to_vec();
    v.extend_from_slice(grpprl);
    v
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
                    let g = g.unwrap_or(&[]);
                    if papx {
                        /* PapxInFkp: cb, [cb'], istd 0, grpprl. */
                        let len = 2 + g.len();
                        let mut d = Vec::new();
                        if len % 2 == 1 {
                            d.push(len.div_ceil(2) as u8);
                        } else {
                            d.push(0);
                            d.push((len / 2) as u8);
                        }
                        d.extend_from_slice(&[0, 0]);
                        d.extend_from_slice(g);
                        d
                    } else {
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
    let chpxs = spec.chpxs();
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
        .map(|(cp, (_, ci, _))| {
            let (fc, len) = cp_fc[cp];
            let g = ci.map(|i| chpxs[i]).filter(|g| !g.is_empty());
            (fc, fc + len, g)
        })
        .collect();
    let papx_ranges = units
        .iter()
        .enumerate()
        .map(|(cp, (_, _, pi))| {
            let (fc, len) = cp_fc[cp];
            (fc, fc + len, Some(spec.paragraphs[*pi].papx.as_slice()))
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
    pair(33, clx);
    /* cswNew = 0 right after the blob. */
    put_u16(&mut word, 0x9A + 8 * CB_RG_FC_LCB, 0);
    (word, table)
}

/// The `.doc` file of `spec`.
pub fn build(spec: &DocSpec) -> Vec<u8> {
    let (word, table) = streams(spec);
    let table_name = if spec.one_table { "1Table" } else { "0Table" };
    let mut entries: Vec<(&str, &[u8])> = vec![("WordDocument", &word), (table_name, &table)];
    if !spec.data.is_empty() {
        entries.push(("Data", &spec.data));
    }
    format_docx::opc::cfb::test_writer::build(&entries)
}
