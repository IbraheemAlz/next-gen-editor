//! Table properties of one table row — the `TAP` the table sprms (`sgc`
//! 5) of its row mark build ([MS-DOC] 2.6.3 Table Properties; 2.4.3
//! Overview of Tables: "the properties of each row mark MUST define the
//! cells for that table row").
//!
//! Written from the specification. A row mark's grpprl is applied in
//! order, as every grpprl is: `sprmTDefTable` (2.9.321 TDefTableOperand)
//! lays the row's cells out — their edges (`rgdxaCenter`) and one `TC80`
//! (2.9.316) each — and the cell-range sprms that follow (borders,
//! shading, widths, merges, vertical alignment, margins) modify those
//! cell definitions; a cell-range sprm before any definition has no cell
//! to modify. The row-wide sprms (table borders, justification, height,
//! direction, preferred width, autofit, default cell margins) describe
//! the row as a whole; the table assembly ([`crate::table`]) takes the
//! table-wide ones from the first row of a table.
//!
//! Not modelled (no engine counterpart yet, issue #624): the leading /
//! trailing row offsets (`sprmTWidthBefore` / `sprmTWidthAfter`),
//! absolute table positioning (sprmTPc / sprmTDxaAbs / …), cell spacing,
//! text flow, fit-text / no-wrap, the table style (`sprmTIstd` only
//! separates tables, 2.4.3), the `sprmTInsert` / `sprmTDelete` /
//! `sprmTDxaCol` cell operations Word writes only inside `sprmPTableProps`
//! (which is not processed: Word also writes `sprmTDefTable` for every
//! row, 2.4.3), revision marks.

use engine::{
    Alignment, BorderStroke, BorderStyle, CellBorders, CellMargins, CellWidth,
    MAX_POINT_FIELD_TWIPS, RowHeight, clamp_border_eighth_pt,
};

use crate::le::{u16_at, u32_at};
use crate::props::ico_rgb;
use crate::sprm::Prl;

// Table sprms (2.6.3), by the name the specification gives them.
const T_JC90: u16 = 0x5400;
const T_DXA_LEFT: u16 = 0x9601;
const T_DXA_GAP_HALF: u16 = 0x9602;
const T_F_CANT_SPLIT90: u16 = 0x3403;
const T_TABLE_HEADER: u16 = 0x3404;
const T_TABLE_BORDERS80: u16 = 0xD605;
const T_DYA_ROW_HEIGHT: u16 = 0x9407;
const T_DEF_TABLE: u16 = 0xD608;
const T_DEF_TABLE_SHD80: u16 = 0xD609;
const T_F_BIDI: u16 = 0x560B;
const T_DEF_TABLE_SHD3RD: u16 = 0xD60C;
const T_DEF_TABLE_SHD: u16 = 0xD612;
const T_TABLE_BORDERS: u16 = 0xD613;
const T_TABLE_WIDTH: u16 = 0xF614;
const T_F_AUTOFIT: u16 = 0x3615;
const T_DEF_TABLE_SHD2ND: u16 = 0xD616;
const T_SET_BRC80: u16 = 0xD620;
const T_MERGE: u16 = 0x5624;
const T_SPLIT: u16 = 0x5625;
const T_VERT_MERGE: u16 = 0xD62B;
const T_VERT_ALIGN: u16 = 0xD62C;
const T_SET_SHD: u16 = 0xD62D;
const T_SET_SHD_ODD: u16 = 0xD62E;
const T_SET_BRC: u16 = 0xD62F;
const T_CELL_PADDING: u16 = 0xD632;
const T_CELL_PADDING_DEFAULT: u16 = 0xD634;
const T_CELL_WIDTH: u16 = 0xD635;
const T_ISTD: u16 = 0x563A;
const T_F_BIDI90: u16 = 0x5664;
const T_F_CANT_SPLIT: u16 = 0x3466;
const T_DEF_TABLE_SHD_RAW: u16 = 0xD670;
const T_DEF_TABLE_SHD_RAW2ND: u16 = 0xD671;
const T_DEF_TABLE_SHD_RAW3RD: u16 = 0xD672;
const T_JC: u16 = 0x548A;

/// A row has at most 63 cells (2.4.3; `TDefTableOperand.NumberOfColumns`
/// MUST NOT exceed 63). Cell indices of the cell-range sprms are held to
/// it.
pub(crate) const MAX_ROW_CELLS: usize = 63;

/// `TCGRF.horzMerge` (2.9.317): this cell is merged into the cell before
/// it — "it contributes its layout region to the set and its own contents
/// are not rendered". 2 and 3 mark the first cell of the set.
pub(crate) const HORZ_MERGED: u8 = 1;
/// `TCGRF.horzMerge`: the first cell of a horizontally merged set.
const HORZ_FIRST: u8 = 2;

/// One cell definition of a row: a `TC80` plus every cell-range sprm the
/// row applied to it.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CellDef {
    /// `TCGRF.horzMerge`: 0 not merged, [`HORZ_MERGED`], 2 / 3 first.
    pub(crate) horz_merge: u8,
    /// `TCGRF.vertMerge` (VerticalMergeFlag, 2.9.351): 0 clear,
    /// 1 `fvmMerge` (continues the cell above), 3 `fvmRestart`.
    pub(crate) vert_merge: u8,
    /// `TCGRF.vertAlign` (VerticalAlign): 0 top, 1 center, 2 bottom.
    pub(crate) v_align: u8,
    /// `TCGRF.ftsWidth` (Fts, 2.9.103) and `TC80.wWidth`: the preferred
    /// width, which `sprmTCellWidth` may replace.
    pub(crate) fts_width: u8,
    pub(crate) w_width: u16,
    /// Top, logical left, bottom, logical right. `None` = not set (the
    /// table borders show); a set `BorderStyle::None` stroke removes one.
    pub(crate) borders: [Option<BorderStroke>; 4],
    /// `None` = not set; `Some(None)` = explicitly not shaded.
    pub(crate) shading: Option<Option<[u8; 4]>>,
    /// `sprmTCellPadding`: per-cell margins.
    pub(crate) margins: Option<CellMargins>,
}

impl CellDef {
    /// The preferred width this cell asks for, `None` when it asks for
    /// none (ftsNil): the layout region then decides.
    pub(crate) fn preferred_width(&self) -> Option<CellWidth> {
        match self.fts_width {
            1 => Some(CellWidth::Auto),
            2 => Some(CellWidth::Pct(self.w_width)),
            3 => Some(CellWidth::Dxa(
                i32::from(self.w_width).min(MAX_POINT_FIELD_TWIPS),
            )),
            _ => None,
        }
    }
}

/// The table properties of one row mark.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Tap {
    /// `rgdxaCenter`: the logical left edge of the row, then the logical
    /// right edge of each cell, in twips from the logical left margin.
    /// Empty when the row has no `sprmTDefTable`.
    pub(crate) edges: Vec<i32>,
    /// One definition per cell of `sprmTDefTable`.
    pub(crate) cells: Vec<CellDef>,
    /// `sprmTTableBorders80` / `sprmTTableBorders`: top, logical left,
    /// bottom, logical right, inside horizontal, inside vertical.
    pub(crate) table_borders: Option<CellBorders>,
    /// The last justification sprm: `(jc, physical)` — sprmTJc90 is
    /// physical, sprmTJc logical (resolved against the direction once
    /// every sprm is in, like a paragraph's).
    jc: Option<(u16, bool)>,
    /// `sprmTDxaLeft` / `sprmTDxaGapHalf`: the table origin is the logical
    /// left margin indented by `dxa_left - dxa_gap_half` — used only when
    /// the row has no `rgdxaCenter` to place it.
    pub(crate) dxa_left: Option<i32>,
    pub(crate) dxa_gap_half: Option<i32>,
    /// `sprmTDyaRowHeight`.
    pub(crate) height: Option<RowHeight>,
    /// `sprmTTableHeader`.
    pub(crate) header: bool,
    /// `sprmTFCantSplit`, which supersedes `sprmTFCantSplit90`.
    cant_split: Option<bool>,
    cant_split90: Option<bool>,
    /// `sprmTFBiDi` / `sprmTFBiDi90`: either one set makes the table
    /// right-to-left.
    bidi: bool,
    bidi90: bool,
    /// `sprmTTableWidth`: the table's preferred width.
    pub(crate) width: Option<CellWidth>,
    /// `sprmTFAutofit`.
    pub(crate) autofit: bool,
    /// `sprmTCellPaddingDefault`: the table's default cell margins.
    pub(crate) padding: CellMargins,
    /// `sprmTIstd`: the table style (only compared: two rows with
    /// different styles belong to different tables, 2.4.3).
    pub(crate) istd: Option<u16>,
}

/// An XAS / YAS value (signed twips) held to Word's 22 in.
fn twips(v: i16) -> i32 {
    i32::from(v).clamp(-MAX_POINT_FIELD_TWIPS, MAX_POINT_FIELD_TWIPS)
}

fn i16_at(b: &[u8], at: usize) -> Option<i16> {
    u16_at(b, at).map(|v| v as i16)
}

/// A Bool8 / Bool16 operand: any nonzero first byte is true.
fn flag(p: &Prl) -> bool {
    p.operand.first().is_some_and(|&v| v != 0)
}

/// The `ST_Border` token of a BrcType (2.9.18) that the model keeps as
/// [`BorderStyle::Other`] — the names the `.docx` reader keeps for the
/// same line styles.
fn other_style(t: u8) -> Option<&'static str> {
    Some(match t {
        0x08 => "dotDash",
        0x09 => "dotDotDash",
        0x0A => "triple",
        0x0B => "thinThickSmallGap",
        0x0C => "thickThinSmallGap",
        0x0D => "thinThickThinSmallGap",
        0x0E => "thinThickMediumGap",
        0x0F => "thickThinMediumGap",
        0x10 => "thinThickThinMediumGap",
        0x11 => "thinThickLargeGap",
        0x12 => "thickThinLargeGap",
        0x13 => "thinThickThinLargeGap",
        0x14 => "wave",
        0x15 => "doubleWave",
        0x16 => "dashSmallGap",
        0x17 => "dashDotStroked",
        0x18 => "threeDEmboss",
        0x19 => "threeDEngrave",
        0x1A => "outset",
        0x1B => "inset",
        _ => return None,
    })
}

/// A border stroke from the fields both `Brc80` (2.9.17) and `Brc`
/// (2.9.16) carry. `brc_type` 0 is "no border": a set `None` stroke when
/// `explicit`, else not set. 0xFF "MUST be ignored": not set.
fn stroke(
    brc_type: u8,
    width_eighths: u8,
    color: Option<[u8; 4]>,
    flags: u8,
    explicit: bool,
) -> Option<BorderStroke> {
    let style = match brc_type {
        0x00 if explicit => BorderStyle::None,
        0x00 | 0xFF => return None,
        0x03 => BorderStyle::Double,
        0x06 => BorderStyle::Dotted,
        0x07 => BorderStyle::Dashed,
        t => match other_style(t) {
            Some(token) => BorderStyle::Other(token.to_string()),
            /* 0x01 single, 0x05 thin single, and the values the table
            does not list: a single line. */
            None => BorderStyle::Single,
        },
    };
    /* dptLineWidth: eighths of a point, "values of less than 2 are
    considered to be equivalent to 2"; held to Word's 1/4 .. 6 pt. */
    let size_eighth_pt = clamp_border_eighth_pt(&style, u16::from(width_eighths.max(2)));
    let space = u16::from(flags & 0x1F);
    Some(BorderStroke {
        size_eighth_pt: if matches!(style, BorderStyle::None) {
            0
        } else {
            size_eighth_pt
        },
        style,
        color,
        space_pt: (space > 0).then_some(space),
        shadow: flags & 0x20 != 0,
        frame: flags & 0x40 != 0,
    })
}

/// A `Brc80MayBeNil` (2.9.18): `0xFFFFFFFF` is nil — "no border", a set
/// `None` stroke when `explicit`, else not set.
fn brc80(b: &[u8], explicit: bool) -> Option<BorderStroke> {
    let [width, brc_type, ico, flags]: [u8; 4] = b.get(..4)?.try_into().ok()?;
    if u32_at(b, 0) == Some(u32::MAX) {
        return stroke(0, 0, None, 0, explicit);
    }
    stroke(brc_type, width, ico_rgb(ico), flags, explicit)
}

/// A `COLORREF` (2.9.43): red, green, blue, `fAuto` (0xFF = automatic).
fn colorref(b: &[u8]) -> Option<Option<[u8; 3]>> {
    let [r, g, bl, auto]: [u8; 4] = b.get(..4)?.try_into().ok()?;
    Some((auto != 0xFF).then_some([r, g, bl]))
}

/// A `Brc` (2.9.16): `cv`, `dptLineWidth`, `brcType`, then `dptSpace` /
/// `fShadow` / `fFrame` in the low bits of the last two bytes.
fn brc(b: &[u8], explicit: bool) -> Option<BorderStroke> {
    let color = colorref(b)?.map(|[r, g, bl]| [r, g, bl, 255]);
    let width = *b.get(4)?;
    let brc_type = *b.get(5)?;
    let flags = *b.get(6)?;
    stroke(brc_type, width, color, flags, explicit)
}

/// The coverage of a shading pattern (`Ipat`, 2.9.124) in thousandths:
/// the share of the foreground colour in the cell's visible fill. Clear
/// is 0, solid 1000, the percentage patterns their percentage; the stripe
/// and cross patterns are approximated by their ink coverage (dark ones
/// about half, thin ones about a quarter). `None` for nil / unknown.
fn ipat_coverage(ipat: u16) -> Option<u32> {
    const PCT: [u32; 12] = [50, 100, 200, 250, 300, 400, 500, 600, 700, 750, 800, 900];
    const PCT_NEW: [u32; 27] = [
        25, 75, 125, 150, 175, 225, 275, 325, 350, 375, 425, 450, 475, 525, 550, 575, 625, 650,
        675, 725, 775, 825, 850, 875, 925, 950, 975,
    ];
    Some(match ipat {
        0x0000 => 0,
        0x0001 => 1000,
        0x0002..=0x000D => PCT[usize::from(ipat - 2)],
        0x000E..=0x0013 => 500,
        0x0014..=0x0019 => 250,
        0x0023..=0x003D => PCT_NEW[usize::from(ipat - 0x23)],
        _ => return None,
    })
}

/// The visible fill of a shading: the pattern's foreground (automatic =
/// black) laid over its background (automatic = white) at the pattern's
/// coverage. Clear with an automatic background is no fill; nil or an
/// unknown pattern is no fill.
fn shade(fore: Option<[u8; 3]>, back: Option<[u8; 3]>, ipat: u16) -> Option<[u8; 4]> {
    let coverage = ipat_coverage(ipat)?;
    if coverage == 0 {
        return back.map(|[r, g, b]| [r, g, b, 255]);
    }
    let f = fore.unwrap_or([0, 0, 0]);
    let b = back.unwrap_or([255, 255, 255]);
    let mix = |fc: u8, bc: u8| {
        ((u32::from(fc) * coverage + u32::from(bc) * (1000 - coverage) + 500) / 1000) as u8
    };
    Some([mix(f[0], b[0]), mix(f[1], b[1]), mix(f[2], b[2]), 255])
}

/// What a 10-byte `Shd` (2.9.248) says: `None` for `ShdNil` (all-ones
/// colours with ipatAuto, or the nil pattern) — the caller decides what
/// nil means — else the fill (`Some(None)` = no fill, `ShdAuto` among
/// them).
fn shd(b: &[u8]) -> Option<Option<Option<[u8; 4]>>> {
    let fore = colorref(b)?;
    let back = colorref(b.get(4..)?)?;
    let ipat = u16_at(b, 8)?;
    let nil = ipat == 0xFFFF || (b.get(..8) == Some(&[0xFF; 8][..]) && ipat == 0);
    Some((!nil).then(|| shade(fore, back, ipat)))
}

/// A 2-byte `Shd80` (2.9.249): icoFore, icoBack (5 bits each), ipat (6
/// bits). `Shd80Nil` (all bits of the three set) is no shading.
fn shd80(b: &[u8]) -> Option<Option<[u8; 4]>> {
    let v = u16_at(b, 0)?;
    let (fore, back, ipat) = (v & 0x1F, (v >> 5) & 0x1F, v >> 10);
    if fore == 0x1F && back == 0x1F && ipat == 0x3F {
        return Some(None);
    }
    let rgb = |i: u16| ico_rgb(i as u8).map(|[r, g, b, _]| [r, g, b]);
    Some(shade(rgb(fore), rgb(back), ipat))
}

impl Tap {
    /// The definitions of cells `first .. lim` (an `ItcFirstLim`,
    /// 2.9.130), held to the cells the row defines.
    fn range(&mut self, first: u8, lim: u8) -> &mut [CellDef] {
        let n = self.cells.len();
        let lim = usize::from(lim).min(n);
        let first = usize::from(first).min(lim);
        &mut self.cells[first..lim]
    }

    /// `sprmTDefTable` (2.9.321 TDefTableOperand): `cb` (2 bytes),
    /// `NumberOfColumns`, `rgdxaCenter[NumberOfColumns + 1]` (XAS), then
    /// up to one `TC80` (2.9.316, 20 bytes) per column — missing ones are
    /// the default `TC80`. An operand cut short defines the cells its
    /// edges reach.
    fn def_table(&mut self, op: &[u8]) {
        let n = usize::from(op.get(2).copied().unwrap_or(0)).min(MAX_ROW_CELLS);
        self.edges = (0..=n)
            .map_while(|i| i16_at(op, 3 + 2 * i).map(twips))
            .collect();
        let cells = self.edges.len().saturating_sub(1);
        let tc_at = 3 + 2 * (n + 1);
        self.cells = (0..cells)
            .map(|i| {
                let tc = op.get(tc_at + 20 * i..tc_at + 20 * (i + 1));
                tc.map_or_else(CellDef::default, tc80)
            })
            .collect();
    }

    /// Apply one table sprm (any other sprm is ignored).
    pub(crate) fn apply(&mut self, p: &Prl) {
        let op = p.operand;
        match p.op {
            T_DEF_TABLE => self.def_table(op),
            T_JC90 => self.jc = u16_at(op, 0).map(|v| (v, true)),
            T_JC => self.jc = u16_at(op, 0).map(|v| (v, false)),
            T_DXA_LEFT => self.dxa_left = i16_at(op, 0).map(twips),
            T_DXA_GAP_HALF => self.dxa_gap_half = i16_at(op, 0).map(twips),
            T_F_CANT_SPLIT => self.cant_split = Some(flag(p)),
            T_F_CANT_SPLIT90 => self.cant_split90 = Some(flag(p)),
            T_TABLE_HEADER => self.header = flag(p),
            T_F_BIDI => self.bidi = flag(p),
            T_F_BIDI90 => self.bidi90 = flag(p),
            T_F_AUTOFIT => self.autofit = flag(p),
            T_ISTD => self.istd = u16_at(op, 0),
            /* A YAS: 0 = sized by the content, positive "at least",
            negative "exact" (the magnitude). */
            T_DYA_ROW_HEIGHT => {
                if let Some(v) = i16_at(op, 0).map(twips) {
                    self.height = match v {
                        0 => None,
                        v if v > 0 => Some(RowHeight::AtLeast { twips: v }),
                        v => Some(RowHeight::Exact { twips: -v }),
                    };
                }
            }
            /* FtsWWidth_Table: ftsWidth (1 byte) + wWidth (2 bytes). */
            T_TABLE_WIDTH => {
                if let (Some(&fts), Some(w)) = (op.first(), u16_at(op, 1)) {
                    self.width = CellDef {
                        fts_width: fts,
                        w_width: w,
                        ..CellDef::default()
                    }
                    .preferred_width();
                }
            }
            /* TableBordersOperand80: cb 0x18, six Brc80MayBeNil. */
            T_TABLE_BORDERS80 => {
                let e = |i: usize| op.get(1 + 4 * i..).and_then(|b| brc80(b, false));
                self.table_borders = Some(six_edges(e));
            }
            /* TableBordersOperand: cb 0x30, six Brc. */
            T_TABLE_BORDERS => {
                let e = |i: usize| op.get(1 + 8 * i..).and_then(|b| brc(b, false));
                self.table_borders = Some(six_edges(e));
            }
            /* TableBrc80Operand / TableBrcOperand: cb, ItcFirstLim,
            bordersToApply (0x01 top, 0x02 logical left, 0x04 bottom, 0x08
            logical right), the border. Nil here is "no border": a set
            none stroke over the table's. */
            T_SET_BRC80 | T_SET_BRC => {
                let (Some(&first), Some(&lim), Some(&sides)) = (op.get(1), op.get(2), op.get(3))
                else {
                    return;
                };
                let border = op.get(4..).and_then(|b| {
                    if p.op == T_SET_BRC80 {
                        brc80(b, true)
                    } else {
                        brc(b, true)
                    }
                });
                for cell in self.range(first, lim) {
                    for (side, slot) in cell.borders.iter_mut().enumerate() {
                        if sides & (1 << side) != 0 {
                            *slot = border.clone();
                        }
                    }
                }
            }
            /* DefTableShd80Operand: cb, then one Shd80 per cell from the
            first. */
            T_DEF_TABLE_SHD80 => {
                let shds = op.get(1..).unwrap_or(&[]);
                for (cell, b) in self.cells.iter_mut().zip(shds.chunks_exact(2)) {
                    if let Some(s) = shd80(b) {
                        cell.shading = Some(s);
                    }
                }
            }
            /* DefTableShdOperand: cb, then one Shd per cell of the sprm's
            range (cells 1-22, 23-44, 45-63). In the Raw arrays a ShdNil
            means "as the table style shades it": left alone; elsewhere it
            is no shading. */
            T_DEF_TABLE_SHD
            | T_DEF_TABLE_SHD2ND
            | T_DEF_TABLE_SHD3RD
            | T_DEF_TABLE_SHD_RAW
            | T_DEF_TABLE_SHD_RAW2ND
            | T_DEF_TABLE_SHD_RAW3RD => {
                let (first, raw) = match p.op {
                    T_DEF_TABLE_SHD => (0, false),
                    T_DEF_TABLE_SHD2ND => (22, false),
                    T_DEF_TABLE_SHD3RD => (44, false),
                    T_DEF_TABLE_SHD_RAW => (0, true),
                    T_DEF_TABLE_SHD_RAW2ND => (22, true),
                    _ => (44, true),
                };
                let shds = op.get(1..).unwrap_or(&[]);
                let cells = self.cells.iter_mut().skip(first).take(22);
                for (cell, b) in cells.zip(shds.chunks_exact(10)) {
                    match shd(b) {
                        Some(Some(fill)) => cell.shading = Some(fill),
                        Some(None) if !raw => cell.shading = Some(None),
                        _ => {}
                    }
                }
            }
            /* TableShadeOperand: cb 12, ItcFirstLim, Shd. sprmTSetShdOdd
            shades every other cell of the range, from its first. */
            T_SET_SHD | T_SET_SHD_ODD => {
                let (Some(&first), Some(&lim)) = (op.get(1), op.get(2)) else {
                    return;
                };
                let Some(fill) = op.get(3..).and_then(shd) else {
                    return;
                };
                let step = if p.op == T_SET_SHD_ODD { 2 } else { 1 };
                for cell in self.range(first, lim).iter_mut().step_by(step) {
                    cell.shading = Some(fill.flatten());
                }
            }
            /* VertMergeOperand: cb 2, itc, VerticalMergeFlag. */
            T_VERT_MERGE => {
                if let (Some(&itc), Some(&flags)) = (op.get(1), op.get(2))
                    && let Some(cell) = self.cells.get_mut(usize::from(itc))
                {
                    cell.vert_merge = flags & 3;
                }
            }
            /* CellRangeVertAlign: cb 3, ItcFirstLim, VerticalAlign. */
            T_VERT_ALIGN => {
                if let (Some(&first), Some(&lim), Some(&va)) = (op.get(1), op.get(2), op.get(3)) {
                    for cell in self.range(first, lim) {
                        cell.v_align = va;
                    }
                }
            }
            /* An ItcFirstLim: sprmTMerge makes the range one merged set
            (its first cell owns the region), sprmTSplit undoes it. */
            T_MERGE | T_SPLIT => {
                if let (Some(&first), Some(&lim)) = (op.first(), op.get(1)) {
                    let merge = p.op == T_MERGE;
                    for (k, cell) in self.range(first, lim).iter_mut().enumerate() {
                        cell.horz_merge = match (merge, k) {
                            (false, _) => 0,
                            (true, 0) => HORZ_FIRST,
                            (true, _) => HORZ_MERGED,
                        };
                    }
                }
            }
            /* TableCellWidthOperand: cb 5, ItcFirstLim,
            FtsWWidth_TablePart. */
            T_CELL_WIDTH => {
                if let (Some(&first), Some(&lim), Some(&fts), Some(w)) =
                    (op.get(1), op.get(2), op.get(3), u16_at(op, 4))
                {
                    for cell in self.range(first, lim) {
                        cell.fts_width = fts;
                        cell.w_width = w;
                    }
                }
            }
            /* CSSAOperand: cb 6, then a CSSA — ItcFirstLim, grfbrc (0x01
            top, 0x02 left, 0x04 bottom, 0x08 right), ftsWidth (ftsNil or
            ftsDxa), wWidth. */
            T_CELL_PADDING_DEFAULT | T_CELL_PADDING => {
                let (Some(&first), Some(&lim), Some(&sides), Some(&fts), Some(w)) =
                    (op.get(1), op.get(2), op.get(3), op.get(4), u16_at(op, 5))
                else {
                    return;
                };
                if fts != 3 {
                    return;
                }
                let w = i32::from(w).min(MAX_POINT_FIELD_TWIPS);
                if p.op == T_CELL_PADDING_DEFAULT {
                    set_margins(&mut self.padding, sides, w);
                } else {
                    for cell in self.range(first, lim) {
                        set_margins(
                            cell.margins.get_or_insert_with(CellMargins::default),
                            sides,
                            w,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    /// `true` when the row is right-to-left (either direction sprm).
    pub(crate) fn bidi(&self) -> bool {
        self.bidi || self.bidi90
    }

    /// `true` when the row may not break across pages.
    pub(crate) fn cant_split(&self) -> bool {
        self.cant_split.or(self.cant_split90).unwrap_or(false)
    }

    /// The table justification: sprmTJc is logical (0 leading edge, 1
    /// centered, 2 trailing edge — the model's start / end); sprmTJc90 is
    /// physical, turned around for a right-to-left table.
    pub(crate) fn alignment(&self) -> Option<Alignment> {
        let (jc, physical) = self.jc?;
        let flip = physical && self.bidi();
        Some(match jc {
            1 => Alignment::Center,
            0 if flip => Alignment::End,
            2 if flip => Alignment::Start,
            2 => Alignment::End,
            _ => Alignment::Start,
        })
    }
}

/// Set the margins `sides` (a CSSA `grfbrc`) of `m` to `w` twips.
fn set_margins(m: &mut CellMargins, sides: u8, w: i32) {
    if sides & 0x01 != 0 {
        m.top_twips = Some(w);
    }
    if sides & 0x02 != 0 {
        m.left_twips = Some(w);
    }
    if sides & 0x04 != 0 {
        m.bottom_twips = Some(w);
    }
    if sides & 0x08 != 0 {
        m.right_twips = Some(w);
    }
}

/// The six table border edges, in the operand order.
fn six_edges(e: impl Fn(usize) -> Option<BorderStroke>) -> CellBorders {
    CellBorders {
        top: e(0),
        left: e(1),
        bottom: e(2),
        right: e(3),
        inside_h: e(4),
        inside_v: e(5),
    }
}

/// A `TC80` (2.9.316): `tcgrf` (a TCGRF, 2.9.317 — horzMerge bits 0-1,
/// textFlow 2-4, vertMerge 5-6, vertAlign 7-8, ftsWidth 9-11), `wWidth`,
/// then four `Brc80MayBeNil` (top, logical left, bottom, logical right).
/// A nil or none border is not set: the table borders show.
fn tc80(tc: &[u8]) -> CellDef {
    let grf = u16_at(tc, 0).unwrap_or(0);
    let border = |i: usize| tc.get(4 + 4 * i..).and_then(|b| brc80(b, false));
    CellDef {
        horz_merge: (grf & 3) as u8,
        vert_merge: ((grf >> 5) & 3) as u8,
        v_align: ((grf >> 7) & 3) as u8,
        fts_width: ((grf >> 9) & 7) as u8,
        w_width: u16_at(tc, 2).unwrap_or(0),
        borders: [border(0), border(1), border(2), border(3)],
        shading: None,
        margins: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sprm::prls;

    fn tap_of(grpprl: &[u8]) -> Tap {
        let mut tap = Tap::default();
        for p in prls(grpprl) {
            tap.apply(&p);
        }
        tap
    }

    #[test]
    fn shading_blends_the_pattern_over_its_background() {
        assert_eq!(shade(None, None, 0), None, "clear, automatic: no fill");
        assert_eq!(shade(None, Some([1, 2, 3]), 0), Some([1, 2, 3, 255]));
        assert_eq!(
            shade(None, None, 1),
            Some([0, 0, 0, 255]),
            "solid auto = black"
        );
        assert_eq!(
            shade(None, None, 5),
            Some([191, 191, 191, 255]),
            "Gray-25%: black at 25 % over white"
        );
        assert_eq!(shade(Some([255, 0, 0]), None, 0xFFFF), None, "nil");
        /* Shd80 Nil, and an ico 7 (yellow) background under clear. */
        assert_eq!(shd80(&[0xFF, 0xFF]), Some(None));
        assert_eq!(
            shd80(&(7u16 << 5).to_le_bytes()),
            Some(Some([255, 255, 0, 255]))
        );
    }

    #[test]
    fn borders_map_brc_types_and_clamp_widths() {
        /* Brc80: 1 pt single in red (ico 6), 2 pt of space. */
        let b = brc80(&[8, 0x01, 6, 2], false).unwrap();
        assert_eq!(b.style, BorderStyle::Single);
        assert_eq!(b.size_eighth_pt, 8);
        assert_eq!(b.color, Some([255, 0, 0, 255]));
        assert_eq!(b.space_pt, Some(2));
        /* A width past 6 pt is held to it; a thin one to 1/4 pt. */
        assert_eq!(brc80(&[200, 0x03, 0, 0], false).unwrap().size_eighth_pt, 48);
        assert_eq!(brc80(&[0, 0x01, 0, 0], false).unwrap().size_eighth_pt, 2);
        assert_eq!(
            brc80(&[4, 0x14, 0, 0], false).unwrap().style,
            BorderStyle::Other("wave".into())
        );
        /* None and nil: not set in a TC80, a set none stroke in TSetBrc. */
        assert_eq!(brc80(&[0, 0, 0, 0], false), None);
        assert_eq!(brc80(&[0xFF; 4], false), None);
        assert_eq!(brc80(&[0xFF; 4], true).unwrap().style, BorderStyle::None);
        /* Brc: a COLORREF, then width and type. */
        let b = brc(&[0x12, 0x34, 0x56, 0, 12, 0x07, 0x20, 0], false).unwrap();
        assert_eq!(
            (b.style, b.size_eighth_pt, b.color, b.shadow),
            (BorderStyle::Dashed, 12, Some([0x12, 0x34, 0x56, 255]), true)
        );
    }

    #[test]
    fn a_def_table_operand_defines_cells_and_later_sprms_modify_them() {
        /* Two cells: edges -108, 1332, 2772; the second TC80 vertically
        merged (restart), centered, 1440 dxa preferred. */
        let mut op = Vec::new();
        op.push(2u8);
        for e in [-108i16, 1332, 2772] {
            op.extend_from_slice(&e.to_le_bytes());
        }
        op.extend_from_slice(&[0u8; 20]);
        let grf: u16 = (3 << 5) | (1 << 7) | (3 << 9);
        let mut tc = grf.to_le_bytes().to_vec();
        tc.extend_from_slice(&1440u16.to_le_bytes());
        tc.extend_from_slice(&[0u8; 16]);
        op.extend_from_slice(&tc);
        let mut g = 0xD608u16.to_le_bytes().to_vec();
        g.extend_from_slice(&((op.len() + 1) as u16).to_le_bytes());
        g.extend_from_slice(&op);
        /* sprmTSetShd over both cells: solid blue. */
        g.extend_from_slice(&0xD62Du16.to_le_bytes());
        g.extend_from_slice(&[12, 0, 2, 0, 0, 255, 0, 0, 0, 0, 0xFF, 1, 0]);
        /* sprmTJc90 right in a right-to-left table: its leading edge. */
        g.extend_from_slice(&0x5400u16.to_le_bytes());
        g.extend_from_slice(&2u16.to_le_bytes());
        g.extend_from_slice(&0x560Bu16.to_le_bytes());
        g.extend_from_slice(&1u16.to_le_bytes());
        let tap = tap_of(&g);
        assert_eq!(tap.edges, vec![-108, 1332, 2772]);
        assert_eq!(tap.cells.len(), 2);
        assert_eq!(tap.cells[1].vert_merge, 3);
        assert_eq!(tap.cells[1].v_align, 1);
        assert_eq!(tap.cells[1].preferred_width(), Some(CellWidth::Dxa(1440)));
        assert_eq!(tap.cells[0].preferred_width(), None);
        assert!(
            tap.cells
                .iter()
                .all(|c| c.shading == Some(Some([0, 0, 255, 255])))
        );
        assert!(tap.bidi());
        assert_eq!(tap.alignment(), Some(Alignment::Start));
    }

    #[test]
    fn a_cell_range_past_the_definitions_modifies_nothing() {
        let mut tap = Tap::default();
        let p = Prl {
            op: T_VERT_ALIGN,
            operand: &[3, 0, 200, 2],
        };
        tap.apply(&p);
        assert!(tap.cells.is_empty());
        let p = Prl {
            op: T_DEF_TABLE,
            operand: &[0, 0, 255, 0, 0],
        };
        tap.apply(&p);
        assert_eq!(tap.edges, vec![0], "a cut-short operand: one edge, no cell");
        assert!(tap.cells.is_empty());
    }
}
