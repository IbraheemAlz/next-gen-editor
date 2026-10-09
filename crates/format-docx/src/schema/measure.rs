//! Issue #349 — one lenient reader for OOXML measure attributes.
//!
//! Page geometry (`<w:pgSz>`, `<w:pgMar>`), paragraph indents and spacing
//! (`<w:ind>`, `<w:spacing>`) and table widths (`<w:gridCol>`, `<w:tblW>`,
//! `<w:tcW>`, `<w:tblInd>`) used to parse their values ad hoc: page
//! geometry through `f32::from_str`, which happily accepts `NaN`, `inf`
//! and `1e30` (a NaN page width defeated the writer's verified-`sectPr`
//! equality forever and regenerated the section on every save), the rest
//! through integer parses that silently dropped `2.5in`-style universal
//! measures ECMA-376 allows and accepted absurd magnitudes.
//!
//! [`measure`] is the single reader. It accepts what the simple types
//! allow (ECMA-376 Part 1 §22.9.2: `ST_TwipsMeasure`,
//! `ST_SignedTwipsMeasure`, the universal-measure units `mm` / `cm` / `in`
//! / `pt` / `pc` / `pi`), a little leniency on top (surrounding whitespace,
//! a decimal fraction on a bare twips value, a leading `+`), and returns a
//! FINITE number of twips clamped into the attribute's range:
//!
//! - unusable (not a number, `NaN`, infinite, a unit the type does not
//!   allow, negative for an unsigned type) → `None`: the caller keeps its
//!   default, and the reader report gets [`DocxWarning::InvalidMeasure`];
//! - finite but outside the range → clamped, with
//!   [`DocxWarning::MeasureClamped`].
//!
//! Issue #407 extends the same reader to the attributes #349 left on ad
//! hoc parses: tab stops (`<w:tab w:pos>`), `<w:cols w:space>`, cell
//! margins (`<w:tcMar>` / `<w:tblCellMar>`), row heights
//! (`<w:trHeight>`), numbering-level indents, `<w:defaultTabStop>` — and,
//! through [`emu`] / [`attr_emu`] / [`text_emu`], to the DrawingML EMU
//! coordinates (`<wp:extent>`, `<wp:simplePos>`, `<wp:posOffset>`, the
//! anchor's wrap distances, text-box insets, `<a:ln w>`) and the CSS-ish
//! VML `style` lengths ([`css_length_emu`]), which used to turn `NaN` into
//! 0 and `inf` into `i64::MAX` through an `as i64` cast.
//!
//! The model never sees a non-finite value, so every verified passthrough
//! (the `sectPr` bytes, a paragraph's `pPr`) re-derives the same model
//! from the same bytes and a zero-edit save stays byte-identical — the
//! bytes themselves are never touched.

use crate::error::{DocxWarning, warn};
use crate::schema::ct_rpr::attr_val;
use quick_xml::events::BytesStart;

/// Word's largest page dimension, indent, paragraph spacing and exact line
/// height: 22 in = 31 680 twips (1584 pt).
pub(crate) const MAX_TWIPS: f64 = 31_680.0;

/// What one measure attribute may hold.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MeasureSpec {
    /// Negative values are legal (`ST_SignedTwipsMeasure`); for an
    /// unsigned type a negative value is unusable.
    pub signed: bool,
    /// Inclusive twips range a usable value is clamped into.
    pub min: f64,
    pub max: f64,
}

/// `ST_TwipsMeasure` — margins (left / right / header / footer), first-line
/// and hanging indents, spacing before / after, grid columns, widths.
pub(crate) const TWIPS: MeasureSpec = MeasureSpec {
    signed: false,
    min: 0.0,
    max: MAX_TWIPS,
};

/// `ST_SignedTwipsMeasure` — top / bottom page margins, start / end
/// indents, `w:line`, the table indent.
pub(crate) const SIGNED_TWIPS: MeasureSpec = MeasureSpec {
    signed: true,
    min: -MAX_TWIPS,
    max: MAX_TWIPS,
};

/// `<w:pgSz w:w|w:h>` — `ST_TwipsMeasure`, at least 0.1 in (Word's
/// smallest page) so layout never divides by a zero-sized page.
pub(crate) const PAGE_SIZE: MeasureSpec = MeasureSpec {
    signed: false,
    min: 144.0,
    max: MAX_TWIPS,
};

/// EMU per twip (914 400 EMU per inch / 1 440 twips per inch).
pub(crate) const EMU_PER_TWIP: f64 = 635.0;

/// Word's largest shape extent / offset, 22 in, in EMU (20 116 800) — the
/// same bound as [`MAX_TWIPS`], and exactly DrawingML's `ST_LineWidth`
/// maximum.
pub(crate) const MAX_EMU: f64 = MAX_TWIPS * EMU_PER_TWIP;

/// `ST_PositiveCoordinate` / `ST_WrapDistance` / `ST_LineWidth` (EMU) —
/// drawing extents, wrap distances, line widths: non-negative, ≤ 22 in.
pub(crate) const EMU_EXTENT: MeasureSpec = MeasureSpec {
    signed: false,
    min: 0.0,
    max: MAX_EMU,
};

/// `ST_Coordinate` / `ST_Coordinate32` (EMU) — positions and offsets:
/// signed, within ±22 in.
pub(crate) const EMU_COORD: MeasureSpec = MeasureSpec {
    signed: true,
    min: -MAX_EMU,
    max: MAX_EMU,
};

/// Twips per unit of a universal measure (ECMA-376 §22.9.2.15).
fn unit_twips(unit: &str) -> Option<f64> {
    Some(match unit {
        "" => 1.0,
        "pt" => 20.0,
        "in" => 1440.0,
        "cm" => 1440.0 / 2.54,
        "mm" => 144.0 / 2.54,
        "pc" | "pi" => 240.0,
        _ => return None,
    })
}

/// The outcome of reading one measure value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Measure {
    /// Usable and in range (twips).
    Ok(f64),
    /// Usable, clamped into range (twips).
    Clamped(f64),
    /// Unusable: the caller keeps its default.
    Invalid,
}

/// EMU per unit of a DrawingML coordinate: a bare integer is EMU; ISO
/// 29500 also lets `ST_Coordinate` carry a universal measure.
fn unit_emu(unit: &str) -> Option<f64> {
    Some(match unit {
        "" => 1.0,
        "pt" => 12_700.0,
        "in" => 914_400.0,
        "cm" => 360_000.0,
        "mm" => 36_000.0,
        "pc" | "pi" => 152_400.0,
        _ => return None,
    })
}

/// Read `raw` under `spec` (see the module docs).
pub(crate) fn measure(raw: &str, spec: MeasureSpec) -> Measure {
    read_number(raw, spec, unit_twips)
}

/// [`measure`] for a DrawingML EMU coordinate (`spec` in EMU — see
/// [`EMU_EXTENT`] / [`EMU_COORD`]).
pub(crate) fn emu(raw: &str, spec: MeasureSpec) -> Measure {
    read_number(raw, spec, unit_emu)
}

/// The shared number + unit reader: `per_unit` maps the (trimmed) unit
/// suffix to the spec's unit, `None` for a unit the type does not allow.
fn read_number(raw: &str, spec: MeasureSpec, per_unit: fn(&str) -> Option<f64>) -> Measure {
    let v = raw.trim();
    let split = v
        .find(|c: char| c.is_ascii_alphabetic() && c != 'e' && c != 'E')
        .unwrap_or(v.len());
    let (num, unit) = v.split_at(split);
    let Some(per) = per_unit(unit.trim()) else {
        return Measure::Invalid;
    };
    let num = num.trim();
    /* `f64::from_str` also takes `inf` / `NaN` spellings; a measure
    never does. */
    if num.is_empty()
        || !num
            .bytes()
            .all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b))
    {
        return Measure::Invalid;
    }
    let Ok(n) = num.parse::<f64>() else {
        return Measure::Invalid;
    };
    in_range(n * per, spec)
}

/// Range-check a converted value under `spec`.
fn in_range(v: f64, spec: MeasureSpec) -> Measure {
    if !v.is_finite() || (!spec.signed && v < 0.0) {
        return Measure::Invalid;
    }
    if v < spec.min {
        Measure::Clamped(spec.min)
    } else if v > spec.max {
        Measure::Clamped(spec.max)
    } else {
        Measure::Ok(v)
    }
}

/// Issue #407 — a CSS-ish VML length (`72pt`, `1in`, `2.54cm`, `10mm`,
/// `96px`, `914400emu`, a bare number in `bare_emu` units) → EMU under
/// `spec`, reporting to the reader report as `attr`.
///
/// VML `style` attributes legitimately hold spellings the model cannot
/// place (`100%`, `auto`, `em` units): those stay a silent `None`, as
/// before. A number that is not finite (`NaN`, `inf` — the old `as i64`
/// cast turned them into 0 and `i64::MAX`) is reported as
/// [`DocxWarning::InvalidMeasure`]; an out-of-range one is clamped and
/// reported as [`DocxWarning::EmuClamped`].
pub(crate) fn css_length_emu(
    raw: &str,
    bare_emu: f64,
    spec: MeasureSpec,
    attr: impl FnOnce() -> String,
) -> Option<i64> {
    let lower = raw.trim().to_ascii_lowercase();
    /* `emu` is the one unit that starts with the exponent letter. */
    let (num, unit) = match lower.strip_suffix("emu") {
        Some(num) => (num, "emu"),
        None => {
            let split = lower
                .find(|c: char| (c.is_ascii_alphabetic() && c != 'e') || c == '%')
                .unwrap_or(lower.len());
            lower.split_at(split)
        }
    };
    let (num, unit) = (num.trim(), unit.trim());
    let unsigned = lower.trim_start_matches(['+', '-']);
    let spelled_non_finite = unsigned.starts_with("nan") || unsigned.starts_with("inf");
    let per = match unit {
        "" => Some(bare_emu),
        "pt" => Some(12_700.0),
        "in" => Some(914_400.0),
        "cm" => Some(360_000.0),
        "mm" => Some(36_000.0),
        "px" => Some(9_525.0),
        "emu" => Some(1.0),
        _ => None,
    };
    let lexically_numeric = !num.is_empty()
        && num
            .bytes()
            .all(|b| b.is_ascii_digit() || b"+-.eE".contains(&b));
    let parsed = match (per, lexically_numeric) {
        (Some(per), true) => num.parse::<f64>().ok().map(|n| n * per),
        _ => None,
    };
    let outcome = match parsed {
        Some(v) => in_range(v, spec),
        None if spelled_non_finite => Measure::Invalid,
        None => return None,
    };
    report(outcome, raw, attr).map(|v| v.round() as i64)
}

/// Report a non-`Ok` outcome of an EMU read and return the usable value.
fn report(outcome: Measure, raw: &str, attr: impl FnOnce() -> String) -> Option<f64> {
    match outcome {
        Measure::Ok(v) => Some(v),
        Measure::Clamped(v) => {
            warn(DocxWarning::EmuClamped {
                attr: attr(),
                value: raw.to_string(),
                emu: v.round() as i64,
            });
            Some(v)
        }
        Measure::Invalid => {
            warn(DocxWarning::InvalidMeasure {
                attr: attr(),
                value: raw.to_string(),
            });
            None
        }
    }
}

/// Issue #407 — [`emu`] of attribute `key` of `e`, as whole EMU, reporting
/// an unusable or clamped value. `None` when absent or unusable.
pub(crate) fn attr_emu(e: &BytesStart, key: &[u8], spec: MeasureSpec) -> Option<i64> {
    let raw = attr_val(e, key)?;
    report(emu(&raw, spec), &raw, || describe(e, key)).map(|v| v.round() as i64)
}

/// Issue #407 — [`emu`] of an element's TEXT content (`<wp:posOffset>`),
/// reported as `element`. `None` when unusable.
pub(crate) fn text_emu(text: &str, spec: MeasureSpec, element: &str) -> Option<i64> {
    report(emu(text, spec), text.trim(), || element.to_string()).map(|v| v.round() as i64)
}

/// [`measure`] of attribute `key` of `e`, reporting an unusable or
/// clamped value to the reader report. `None` when the attribute is absent
/// or unusable.
pub(crate) fn attr_measure(e: &BytesStart, key: &[u8], spec: MeasureSpec) -> Option<f64> {
    let raw = attr_val(e, key)?;
    match measure(&raw, spec) {
        Measure::Ok(t) => Some(t),
        Measure::Clamped(t) => {
            warn(DocxWarning::MeasureClamped {
                attr: describe(e, key),
                value: raw,
                twips: t as i64,
            });
            Some(t)
        }
        Measure::Invalid => {
            warn(DocxWarning::InvalidMeasure {
                attr: describe(e, key),
                value: raw,
            });
            None
        }
    }
}

/// [`attr_measure`] in layout points (`twips / 20`, computed in `f32`
/// exactly as the pre-#349 `f32` parse did for an integer value).
pub(crate) fn attr_measure_pt(e: &BytesStart, key: &[u8], spec: MeasureSpec) -> Option<f32> {
    attr_measure(e, key, spec).map(|t| (t as f32) / 20.0)
}

/// [`attr_measure`] as whole twips (rounded).
pub(crate) fn attr_measure_twips(e: &BytesStart, key: &[u8], spec: MeasureSpec) -> Option<i32> {
    attr_measure(e, key, spec).map(|t| t.round() as i32)
}

/// `w:pgSz/@w:w`-style name of the attribute, for the reader report.
pub(crate) fn describe(e: &BytesStart, key: &[u8]) -> String {
    format!(
        "{}/@{}",
        String::from_utf8_lossy(e.name().as_ref()),
        String::from_utf8_lossy(key)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_and_units_read_as_twips() {
        assert_eq!(measure("11906", PAGE_SIZE), Measure::Ok(11906.0));
        assert_eq!(measure(" 720 ", TWIPS), Measure::Ok(720.0));
        assert_eq!(measure("720.5", TWIPS), Measure::Ok(720.5));
        assert_eq!(measure("8.5in", PAGE_SIZE), Measure::Ok(12240.0));
        assert_eq!(measure("36pt", TWIPS), Measure::Ok(720.0));
        assert_eq!(measure("1pc", TWIPS), Measure::Ok(240.0));
        assert_eq!(measure("1pi", TWIPS), Measure::Ok(240.0));
        assert!(matches!(measure("2.54cm", TWIPS), Measure::Ok(t) if (t - 1440.0).abs() < 1e-9));
        assert!(matches!(measure("25.4mm", TWIPS), Measure::Ok(t) if (t - 1440.0).abs() < 1e-9));
        assert_eq!(measure("-720", SIGNED_TWIPS), Measure::Ok(-720.0));
        assert_eq!(measure("-0.5in", SIGNED_TWIPS), Measure::Ok(-720.0));
    }

    #[test]
    fn non_finite_and_garbage_are_invalid() {
        for raw in [
            "NaN", "nan", "inf", "-inf", "infinity", "", "abc", "12px", "0x1F", "1..2",
        ] {
            assert_eq!(measure(raw, SIGNED_TWIPS), Measure::Invalid, "{raw}");
        }
        assert_eq!(measure("-5", TWIPS), Measure::Invalid, "unsigned type");
        assert_eq!(measure("-1in", PAGE_SIZE), Measure::Invalid);
        assert_eq!(measure("1e400", SIGNED_TWIPS), Measure::Invalid);
    }

    #[test]
    fn out_of_range_values_clamp() {
        assert_eq!(measure("1e30", PAGE_SIZE), Measure::Clamped(MAX_TWIPS));
        assert_eq!(measure("99999999999", TWIPS), Measure::Clamped(MAX_TWIPS));
        assert_eq!(
            measure("-99999", SIGNED_TWIPS),
            Measure::Clamped(-MAX_TWIPS)
        );
        assert_eq!(measure("0", PAGE_SIZE), Measure::Clamped(144.0));
        assert_eq!(measure("30in", PAGE_SIZE), Measure::Clamped(MAX_TWIPS));
    }

    /// Issue #407 — DrawingML EMU coordinates: bare integers are EMU,
    /// universal measures convert, non-finite / garbage is unusable,
    /// out-of-range clamps to ±22 in.
    #[test]
    fn emu_coordinates_read_validate_and_clamp() {
        assert_eq!(emu("914400", EMU_EXTENT), Measure::Ok(914_400.0));
        assert_eq!(emu(" -914400 ", EMU_COORD), Measure::Ok(-914_400.0));
        assert_eq!(emu("1in", EMU_EXTENT), Measure::Ok(914_400.0));
        assert_eq!(emu("72pt", EMU_EXTENT), Measure::Ok(914_400.0));
        assert_eq!(emu("2.54cm", EMU_EXTENT), Measure::Ok(914_400.0));
        for raw in ["NaN", "inf", "-inf", "", "x", "12px", "1e400"] {
            assert_eq!(emu(raw, EMU_COORD), Measure::Invalid, "{raw}");
        }
        assert_eq!(emu("-1", EMU_EXTENT), Measure::Invalid, "unsigned type");
        assert_eq!(emu("1e30", EMU_EXTENT), Measure::Clamped(MAX_EMU));
        assert_eq!(emu("-99999999999", EMU_COORD), Measure::Clamped(-MAX_EMU));
        assert_eq!(MAX_EMU, 20_116_800.0, "22 in, DrawingML's ST_LineWidth max");
    }

    /// Issue #407 — VML `style` lengths: units, the caller's bare unit,
    /// silent `None` for spellings the model cannot place, a report for
    /// non-finite numbers (the old `as i64` cast made NaN 0 and inf
    /// `i64::MAX`) and for out-of-range ones.
    #[test]
    fn css_lengths_convert_validate_and_report() {
        const PT: f64 = 12_700.0;
        let len = |raw: &str| css_length_emu(raw, PT, EMU_COORD, || "v:shape/@style width".into());
        for (raw, want) in [
            ("72pt", 914_400),
            ("1in", 914_400),
            ("2.54cm", 914_400),
            ("25.4mm", 914_400),
            ("96px", 914_400),
            ("914400emu", 914_400),
            ("914400EMU", 914_400),
            ("72", 914_400),
            ("-1in", -914_400),
            ("1e1pt", 127_000),
            ("0", 0),
        ] {
            assert_eq!(len(raw), Some(want), "{raw}");
        }
        let mut out = Vec::new();
        crate::error::collect_read_warnings(&mut out, |_| {
            for raw in ["50%", "auto", "2em", "x", ""] {
                assert_eq!(len(raw), None, "{raw}");
            }
        });
        assert!(out.is_empty(), "unplaceable spellings are silent: {out:?}");
        crate::error::collect_read_warnings(&mut out, |_| {
            for raw in ["NaN", "nanpt", "inf", "-Infinity", "1e400pt"] {
                assert_eq!(len(raw), None, "{raw}");
            }
            assert_eq!(len("99999in"), Some(MAX_EMU as i64));
        });
        assert_eq!(out.len(), 6, "{out:?}");
        assert!(matches!(
            &out[0],
            DocxWarning::InvalidMeasure { attr, value } if attr == "v:shape/@style width" && value == "NaN"
        ));
        assert!(matches!(
            &out[5],
            DocxWarning::EmuClamped { value, emu, .. } if value == "99999in" && *emu == 20_116_800
        ));
    }

    /// The pre-#349 `f32` parse and the measure reader agree bit for bit
    /// on every in-range integer (no pinned geometry moves).
    #[test]
    fn in_range_integers_keep_their_exact_f32_points() {
        for raw in ["11906", "16838", "1440", "720", "1134", "31680", "567"] {
            let old = raw.parse::<f32>().unwrap() / 20.0;
            let Measure::Ok(t) = measure(raw, TWIPS) else {
                panic!("{raw}");
            };
            assert_eq!(((t as f32) / 20.0).to_bits(), old.to_bits(), "{raw}");
        }
    }
}
