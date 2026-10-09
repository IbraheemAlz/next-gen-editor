//! Issue #365 — tracked table-row revisions (`<w:trPr><w:ins/>` /
//! `<w:del/>`): read as `RowProperties::revisions`, re-emitted verbatim
//! while unchanged (the verified `<w:trPr>` passthrough), regenerated in
//! schema order with a package-unique id when the engine records or
//! resolves one; the row's `<w:trPrChange>` / the table's
//! `<w:tblPrChange>` history rides the grab bags untouched.

use super::tests::document_xml_of;
use super::*;
use crate::opc::archive::read_docx;
use crate::test_fixtures::{TRACKED_ROWS_CELLS, tracked_table_rows_docx};
use engine::{BlockPath, LogicalPos, PathStep, RevisionKind};

fn open() -> (String, DocxArchive) {
    let bytes = tracked_table_rows_docx();
    let archive = read_docx(&bytes).expect("read fixture");
    (document_xml_of(&bytes), archive)
}

fn cell(row: u32, col: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(
        BlockPath {
            steps: vec![
                PathStep::Block(1),
                PathStep::Cell { row, col },
                PathStep::Block(0),
            ],
        },
        offset,
    )
}

fn rows(doc: &engine::DocumentTree) -> Vec<Vec<(RevisionKind, Option<u32>)>> {
    doc.table_at_path(&BlockPath::top(1))
        .expect("table")
        .rows
        .iter()
        .map(|r| r.props.revisions.iter().map(|x| (x.kind, x.id)).collect())
        .collect()
}

fn texts(doc: &engine::DocumentTree) -> Vec<Vec<String>> {
    doc.table_at_path(&BlockPath::top(1))
        .expect("table")
        .rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|c| {
                    c.blocks[0]
                        .as_paragraph()
                        .map_or("", |p| p.text.as_str())
                        .into()
                })
                .collect()
        })
        .collect()
}

/// `xml` minus `inserted` at one place is `source`: the edit was a pure
/// insertion.
fn pure_insertion(source: &str, xml: &str) -> bool {
    let prefix = source
        .bytes()
        .zip(xml.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let rest = source.len() - prefix;
    let suffix = source
        .bytes()
        .rev()
        .zip(xml.bytes().rev())
        .take(rest)
        .take_while(|(a, b)| a == b)
        .count();
    prefix + suffix == source.len()
}

#[test]
fn row_revisions_read_into_the_model_not_the_grab_bag() {
    let (_, archive) = open();
    let doc = &archive.document;
    assert_eq!(
        rows(doc),
        vec![
            vec![],
            vec![(RevisionKind::Delete, Some(3))],
            vec![(RevisionKind::Insert, Some(8))]
        ]
    );
    let t = doc.table_at_path(&BlockPath::top(1)).unwrap();
    assert_eq!(
        t.rows[1].revision().map(|r| r.author.as_str()),
        Some("Author")
    );
    for row in &t.rows {
        let bag = row.props.grab_bag.as_deref();
        assert!(bag.is_none_or(|b| {
            b.fragments
                .iter()
                .all(|f| !f.starts_with(b"<w:del") && !f.starts_with(b"<w:ins"))
        }));
    }
    /* The property history stays verbatim in the bags. */
    let bag = t.rows[0].props.grab_bag.as_deref().expect("trPr bag");
    assert!(
        bag.fragments
            .iter()
            .any(|f| f.starts_with(b"<w:trPrChange"))
    );
    let bag = t.props.grab_bag.as_deref().expect("tblPr bag");
    assert!(
        bag.fragments
            .iter()
            .any(|f| f.starts_with(b"<w:tblPrChange"))
    );
    assert_eq!(
        texts(doc),
        TRACKED_ROWS_CELLS
            .iter()
            .map(|r| r.iter().map(|s| s.to_string()).collect::<Vec<_>>())
            .collect::<Vec<_>>()
    );
}

#[test]
fn untouched_and_edited_saves_keep_the_row_revision_bytes() {
    let (source, archive) = open();
    let untouched = write_docx(&archive, &archive.document).unwrap();
    assert_eq!(document_xml_of(&untouched), source);
    /* Typing in a cell regenerates the table: every trPr (row
    revisions, trPrChange) and the tblPr (tblPrChange) re-emit verbatim. */
    for (row, col) in [(0, 0), (1, 1), (2, 0)] {
        let edited = archive.document.insert_text(cell(row, col, 1), "X");
        let xml = document_xml_of(&write_docx(&archive, &edited).unwrap());
        assert!(pure_insertion(&source, &xml), "{row}:{col}\n{xml}");
    }
}

#[test]
fn resolving_the_rows_rewrites_trpr_without_them() {
    let (_, archive) = open();
    for accept in [true, false] {
        let resolved = archive.document.resolve_all_revisions(accept);
        let want: Vec<Vec<String>> = if accept {
            vec![
                vec!["kept A".into(), "kept B".into()],
                vec!["new A".into(), "new B".into()],
            ]
        } else {
            vec![
                vec!["kept A".into(), "kept B".into()],
                vec!["gone A".into(), "gone B".into()],
            ]
        };
        assert_eq!(texts(&resolved), want, "accept={accept}");
        assert!(!resolved.has_revisions());
        for bytes in [
            write_docx(&archive, &resolved).unwrap(),
            save_docx(&resolved).unwrap(),
        ] {
            let xml = document_xml_of(&bytes);
            assert!(
                !xml.contains("<w:del ") && !xml.contains("<w:ins "),
                "{xml}"
            );
            assert!(xml.contains("<w:trPrChange w:id=\"2\""), "{xml}");
            let reread = read_docx(&bytes).unwrap().document;
            assert_eq!(texts(&reread), want);
            assert!(!reread.has_revisions());
        }
    }
}

/// An engine-recorded row deletion on a source row regenerates its
/// `<w:trPr>` with a `<w:del>` (after the modeled children, before the
/// `trPrChange` tail) under a fresh id no other annotation uses.
#[test]
fn a_recorded_row_deletion_writes_trpr_del_with_a_fresh_id() {
    let (_, archive) = open();
    let t = archive
        .document
        .try_tracked_delete_range(cell(0, 0, 0), cell(0, 1, 6), "Me", "2026-10-09T00:00:00Z")
        .unwrap();
    /* One row, across its cells: per-cell text, no row revision. */
    assert_eq!(rows(&t.doc)[0], vec![]);
    let next_row = LogicalPos::new(
        BlockPath {
            steps: vec![
                PathStep::Block(1),
                PathStep::Cell { row: 2, col: 0 },
                PathStep::Block(0),
            ],
        },
        0,
    );
    let t = archive
        .document
        .try_tracked_delete_range(cell(0, 0, 0), next_row, "Me", "2026-10-09T00:00:00Z")
        .unwrap();
    assert_eq!(rows(&t.doc)[0], vec![(RevisionKind::Delete, None)]);
    let xml = document_xml_of(&write_docx(&archive, &t.doc).unwrap());
    let tr_pr = xml
        .split("<w:trPr>")
        .nth(1)
        .and_then(|s| s.split("</w:trPr>").next())
        .unwrap();
    /* (A regenerated `<w:trHeight>` spells its rule out — pre-existing.) */
    assert!(tr_pr.starts_with("<w:trHeight w:val=\"400\""), "{tr_pr}");
    assert!(tr_pr.contains("/><w:del w:id=\""), "{tr_pr}");
    assert!(tr_pr.contains("w:author=\"Me\" w:date=\"2026-10-09T00:00:00Z\"/><w:trPrChange"));
    let id: u32 = tr_pr
        .split("<w:del w:id=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .and_then(|s| s.parse().ok())
        .unwrap();
    assert!(id > 12, "fresh id above every source id: {id}");
    let reread = read_docx(&write_docx(&archive, &t.doc).unwrap())
        .unwrap()
        .document;
    assert_eq!(rows(&reread)[0], vec![(RevisionKind::Delete, Some(id))]);
}

/// A row one reviewer inserted and another deleted carries both, in
/// source order; accept-all removes it, reject-all removes it too (the
/// insertion is rejected).
#[test]
fn a_row_with_both_changes_resolves_in_order() {
    let bytes = crate::test_fixtures::docx_with_body(concat!(
        "<w:tbl><w:tblGrid><w:gridCol w:w=\"2000\"/></w:tblGrid>",
        "<w:tr><w:trPr><w:ins w:id=\"1\" w:author=\"A\"/><w:del w:id=\"2\" w:author=\"B\"/></w:trPr>",
        "<w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr>",
        "<w:tr><w:tc><w:p><w:r><w:t>y</w:t></w:r></w:p></w:tc></w:tr></w:tbl>",
        "<w:p/>"
    ));
    let doc = read_docx(&bytes).unwrap().document;
    let t = doc.table_at_path(&BlockPath::top(0)).unwrap();
    assert_eq!(
        t.rows[0]
            .props
            .revisions
            .iter()
            .map(|r| (r.kind, r.id))
            .collect::<Vec<_>>(),
        vec![
            (RevisionKind::Insert, Some(1)),
            (RevisionKind::Delete, Some(2))
        ]
    );
    for accept in [true, false] {
        let resolved = doc.resolve_all_revisions(accept);
        assert_eq!(
            resolved
                .table_at_path(&BlockPath::top(0))
                .unwrap()
                .rows
                .len(),
            1,
            "accept={accept}"
        );
    }
}
