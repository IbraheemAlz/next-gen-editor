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

/// Read `raw` under `spec` (see the module docs).
pub(crate) fn measure(raw: &str, spec: MeasureSpec) -> Measure {
    let v = raw.trim();
    let split = v
        .find(|c: char| c.is_ascii_alphabetic() && c != 'e' && c != 'E')
        .unwrap_or(v.len());
    let (num, unit) = v.split_at(split);
    let Some(per) = unit_twips(unit.trim()) else {
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
    let twips = n * per;
    if !twips.is_finite() || (!spec.signed && twips < 0.0) {
        return Measure::Invalid;
    }
    if twips < spec.min {
        Measure::Clamped(spec.min)
    } else if twips > spec.max {
        Measure::Clamped(spec.max)
    } else {
        Measure::Ok(twips)
    }
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
fn describe(e: &BytesStart, key: &[u8]) -> String {
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
