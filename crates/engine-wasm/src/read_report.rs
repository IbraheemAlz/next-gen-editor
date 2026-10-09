//! Issue #406 — the reader's warning report on the wire.
//!
//! `format_docx::read_docx_with_limits` collects every non-fatal
//! diagnostic of an open on `DocxArchive::warnings` (#111 table nesting,
//! #349 / #407 measures, #350 fields, #325 / #394 namespaces, #353 part
//! discovery). Before #406 the engine dropped them after the load, so a
//! document that opened with a clamped page margin or a regenerate-only
//! part looked exactly like a clean open. [`bridge_read_warnings`] lowers
//! them onto `Event::DocumentLoaded::warnings`: one bridge
//! [`ReadWarningKind`] per reader warning class (exhaustive — a new
//! `DocxWarning` variant does not compile until it is classified here),
//! identical warnings coalesced into one entry with a `count`, first
//! occurrence order kept.

use bridge::{ReadWarning, ReadWarningKind};
use format_docx::DocxWarning;

/// The bridge class of one reader warning.
fn kind_of(w: &DocxWarning) -> ReadWarningKind {
    match w {
        DocxWarning::TableNestingTooDeep { .. } => ReadWarningKind::TableNestingTooDeep,
        DocxWarning::InvalidMeasure { .. } => ReadWarningKind::InvalidMeasure,
        /* Issue #407 — an EMU coordinate clamped is the same class as a
        twips measure clamped; the detail names the unit. */
        DocxWarning::MeasureClamped { .. } | DocxWarning::EmuClamped { .. } => {
            ReadWarningKind::MeasureClamped
        }
        DocxWarning::UnclosedField { .. } => ReadWarningKind::UnclosedField,
        DocxWarning::StrayFieldChar { .. } => ReadWarningKind::StrayFieldChar,
        DocxWarning::FieldNestingTooDeep { .. } => ReadWarningKind::FieldNestingTooDeep,
        DocxWarning::NonCanonicalNamespaces { .. } => ReadWarningKind::NonCanonicalNamespaces,
        DocxWarning::NotWordprocessingMl => ReadWarningKind::NotWordprocessingMl,
        DocxWarning::MainPartFallback { .. } => ReadWarningKind::MainPartFallback,
        DocxWarning::UnsafeRelationshipTarget { .. } => ReadWarningKind::UnsafeRelationshipTarget,
    }
}

/// Lower an open's reader warnings onto the wire, coalescing identical
/// ones (same kind, part and detail) into one entry with a `count`.
pub(crate) fn bridge_read_warnings(warnings: &[DocxWarning]) -> Vec<ReadWarning> {
    let mut out: Vec<ReadWarning> = Vec::new();
    for w in warnings {
        let kind = kind_of(w);
        let part = w.part().map(str::to_owned);
        let detail = w.detail();
        match out
            .iter_mut()
            .find(|r| r.kind == kind && r.part == part && r.detail == detail)
        {
            Some(r) => r.count = r.count.saturating_add(1),
            None => out.push(ReadWarning {
                kind,
                part,
                detail,
                count: 1,
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_warnings_coalesce_in_first_occurrence_order() {
        let clamp = DocxWarning::MeasureClamped {
            attr: "w:pgMar/@w:top".into(),
            value: "99999".into(),
            twips: 31_680,
        };
        let out = bridge_read_warnings(&[
            clamp.clone(),
            DocxWarning::NonCanonicalNamespaces {
                part: "word/styles.xml".into(),
                detail: "WordprocessingML bound to `x:`".into(),
                normalized: true,
            },
            clamp,
            DocxWarning::InvalidMeasure {
                attr: "w:tab/@w:pos".into(),
                value: "NaN".into(),
            },
        ]);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].kind, ReadWarningKind::MeasureClamped);
        assert_eq!(out[0].count, 2);
        assert_eq!(out[0].part, None);
        assert_eq!(out[0].detail, "w:pgMar/@w:top = \"99999\" → 31680 twips");
        assert_eq!(out[1].kind, ReadWarningKind::NonCanonicalNamespaces);
        assert_eq!(out[1].part.as_deref(), Some("word/styles.xml"));
        assert_eq!(out[2].kind, ReadWarningKind::InvalidMeasure);
        assert_eq!(out[2].detail, "w:tab/@w:pos = \"NaN\"");
    }

    #[test]
    fn a_clean_open_has_no_warnings() {
        assert!(bridge_read_warnings(&[]).is_empty());
    }
}
