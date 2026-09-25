//! `CT_TblPr` / `CT_TrPr` / `CT_TcPr` (table, row, cell properties) —
//! shared schema knowledge for the table part parser and the writer.
//!
//! Issue #84 — the read side is still `parts::table`; this module only
//! publishes which children the model expresses (everything else is
//! captured verbatim into the owning grab bag) and where each child sits
//! in the schema sequence (so the writer can interleave bag fragments with
//! the modeled children in a schema-valid order).

use crate::schema::ct_rpr::schema_rank;

/// `true` for the `<w:tblPr>` children `parts::table` consumes.
pub fn tbl_pr_child_is_modeled(name: &[u8]) -> bool {
    matches!(
        name,
        b"w:tblStyle"
            | b"w:bidiVisual"
            | b"w:tblW"
            | b"w:jc"
            | b"w:tblInd"
            | b"w:tblBorders"
            | b"w:tblLayout"
            | b"w:tblCellMar"
    )
}

/// Rank in the `CT_TblPrBase` sequence (ECMA-376 §17.4.60) + the
/// `CT_TblPr` `tblPrChange` tail.
pub fn tbl_pr_child_rank(name: &[u8]) -> u16 {
    const ORDER: &[&[u8]] = &[
        b"w:tblStyle",
        b"w:tblpPr",
        b"w:tblOverlap",
        b"w:bidiVisual",
        b"w:tblStyleRowBandSize",
        b"w:tblStyleColBandSize",
        b"w:tblW",
        b"w:jc",
        b"w:tblCellSpacing",
        b"w:tblInd",
        b"w:tblBorders",
        b"w:shd",
        b"w:tblLayout",
        b"w:tblCellMar",
        b"w:tblLook",
        b"w:tblCaption",
        b"w:tblDescription",
    ];
    schema_rank(ORDER, name, b"w:tblPrChange")
}

/// `true` for the `<w:trPr>` children `parts::table` consumes.
pub fn tr_pr_child_is_modeled(name: &[u8]) -> bool {
    matches!(name, b"w:trHeight" | b"w:cantSplit" | b"w:tblHeader")
}

/// Rank in the `EG_TrPrBase` listing (ECMA-376 §17.4.82; an unbounded
/// choice, so any order validates — the listed order is what Word
/// writes) + the `CT_TrPr` tail (`ins`, `del`, `trPrChange`).
pub fn tr_pr_child_rank(name: &[u8]) -> u16 {
    const ORDER: &[&[u8]] = &[
        b"w:cnfStyle",
        b"w:divId",
        b"w:gridBefore",
        b"w:gridAfter",
        b"w:wBefore",
        b"w:wAfter",
        b"w:cantSplit",
        b"w:trHeight",
        b"w:tblHeader",
        b"w:tblCellSpacing",
        b"w:jc",
        b"w:hidden",
        b"w:ins",
        b"w:del",
    ];
    schema_rank(ORDER, name, b"w:trPrChange")
}

/// `true` for the `<w:tcPr>` children `parts::table` consumes.
pub fn tc_pr_child_is_modeled(name: &[u8]) -> bool {
    matches!(
        name,
        b"w:tcW"
            | b"w:gridSpan"
            | b"w:vMerge"
            | b"w:tcBorders"
            | b"w:shd"
            | b"w:tcMar"
            | b"w:vAlign"
    )
}

/// Rank in the `CT_TcPrBase` sequence (ECMA-376 §17.4.71) + the
/// `CT_TcPr` tail (`cellIns`, `cellDel`, `cellMerge`, `tcPrChange`).
pub fn tc_pr_child_rank(name: &[u8]) -> u16 {
    const ORDER: &[&[u8]] = &[
        b"w:cnfStyle",
        b"w:tcW",
        b"w:gridSpan",
        b"w:hMerge",
        b"w:vMerge",
        b"w:tcBorders",
        b"w:shd",
        b"w:noWrap",
        b"w:tcMar",
        b"w:textDirection",
        b"w:tcFitText",
        b"w:vAlign",
        b"w:hideMark",
        b"w:headers",
        b"w:cellIns",
        b"w:cellDel",
        b"w:cellMerge",
    ];
    schema_rank(ORDER, name, b"w:tcPrChange")
}

/// Dispatch by container: `(is_modeled, rank)` for a child of `parent`
/// (`w:tblPr` / `w:trPr` / `w:tcPr`), `None` for any other parent.
pub fn child_is_modeled(parent: &[u8], name: &[u8]) -> Option<bool> {
    match parent {
        b"w:tblPr" => Some(tbl_pr_child_is_modeled(name)),
        b"w:trPr" => Some(tr_pr_child_is_modeled(name)),
        b"w:tcPr" => Some(tc_pr_child_is_modeled(name)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_follow_schema_sequences() {
        assert!(tbl_pr_child_rank(b"w:bidiVisual") < tbl_pr_child_rank(b"w:tblW"));
        assert!(tbl_pr_child_rank(b"w:tblW") < tbl_pr_child_rank(b"w:jc"));
        assert!(tbl_pr_child_rank(b"w:jc") < tbl_pr_child_rank(b"w:tblInd"));
        assert!(tbl_pr_child_rank(b"w:tblBorders") < tbl_pr_child_rank(b"w:tblLayout"));
        assert!(tbl_pr_child_rank(b"w:tblCellMar") < tbl_pr_child_rank(b"w:tblLook"));
        assert!(tr_pr_child_rank(b"w:cnfStyle") < tr_pr_child_rank(b"w:cantSplit"));
        assert!(tr_pr_child_rank(b"w:cantSplit") < tr_pr_child_rank(b"w:trHeight"));
        assert!(tr_pr_child_rank(b"w:tblHeader") < tr_pr_child_rank(b"w:jc"));
        assert!(tc_pr_child_rank(b"w:shd") < tc_pr_child_rank(b"w:noWrap"));
        assert!(tc_pr_child_rank(b"w:tcMar") < tc_pr_child_rank(b"w:vAlign"));
        assert!(tc_pr_child_rank(b"w:vAlign") < tc_pr_child_rank(b"w:hideMark"));
    }

    #[test]
    fn unknown_ranks_before_change_record() {
        assert!(tbl_pr_child_rank(b"w14:custom") > tbl_pr_child_rank(b"w:tblDescription"));
        assert!(tbl_pr_child_rank(b"w14:custom") < tbl_pr_child_rank(b"w:tblPrChange"));
        assert_eq!(tc_pr_child_rank(b"w:tcPrChange"), u16::MAX);
    }

    #[test]
    fn modeled_predicates_match_the_writer_surface() {
        assert!(tbl_pr_child_is_modeled(b"w:tblW"));
        assert!(!tbl_pr_child_is_modeled(b"w:tblLook"));
        /* Issue #79 — modeled (`TableProperties::bidi_visual`), no longer bagged. */
        assert!(tbl_pr_child_is_modeled(b"w:bidiVisual"));
        assert!(tr_pr_child_is_modeled(b"w:trHeight"));
        assert!(!tr_pr_child_is_modeled(b"w:jc"));
        assert!(tc_pr_child_is_modeled(b"w:tcMar"));
        assert!(!tc_pr_child_is_modeled(b"w:noWrap"));
        assert_eq!(child_is_modeled(b"w:tblGrid", b"w:gridCol"), None);
    }
}
