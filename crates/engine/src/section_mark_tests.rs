//! Issue #367 — resolving a paragraph-mark revision whose mark carries a
//! SECTION BREAK. Word's rule (the one `delete_range` applies, #70):
//! deleting a section break makes the text before it part of the
//! FOLLOWING section, with that section's properties; the dropped
//! section's header / footer references backfill the surviving
//! section's empty slots (absence = link-to-previous).

use crate::{
    Block, BlockPath, DocumentTree, HeaderFooterRefs, LogicalPos, PageGeometry, Revision,
    RevisionKind, SectionProps, UndoStack,
};

const ME: &str = "R";
const DATE: &str = "2026-10-09T00:00:00Z";

fn pos(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

fn refs(default: Option<&str>, first: Option<&str>) -> HeaderFooterRefs {
    HeaderFooterRefs {
        default: default.map(str::to_string),
        first: first.map(str::to_string),
        even: None,
    }
}

/// A small landscape-ish first section (its own default + first header),
/// ending at paragraph 0.
fn first_section() -> SectionProps {
    SectionProps {
        geometry: PageGeometry::from_twips(8000, 6000, 720, 720, 720, 720, 360, 360),
        header_refs: refs(Some("rIdA"), Some("rIdAFirst")),
        footer_refs: refs(Some("rIdFootA"), None),
        title_pg: true,
        ..SectionProps::default()
    }
}

fn change(kind: RevisionKind, author: &str) -> Revision {
    Revision {
        start: 0,
        end: 0,
        kind,
        author: author.into(),
        date: DATE.into(),
        id: None,
        prev_attrs: None,
        move_name: None,
    }
}

/// `["first" (ends section 1, mark revision `kind`), "second", "third"]`,
/// the body section (A4) owning only a default header `rIdB`.
fn doc(kind: Option<RevisionKind>, author: &str) -> DocumentTree {
    let mut d = DocumentTree::from_paragraphs([
        "first".to_string(),
        "second".to_string(),
        "third".to_string(),
    ]);
    let mut blocks = d.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[0] {
        p.section_end = Some(Box::new(first_section()));
        p.mark_revisions = kind.map(|k| change(k, author)).into_iter().collect();
    }
    d.blocks = blocks;
    d.body_section.header_refs = refs(Some("rIdB"), None);
    d
}

fn texts(d: &DocumentTree) -> Vec<String> {
    d.blocks
        .iter()
        .filter_map(Block::as_paragraph)
        .map(|p| p.text.clone())
        .collect()
}

fn section_count(d: &DocumentTree) -> usize {
    d.effective_sections().len()
}

/// Accepting a deleted mark that carries a section break merges the
/// paragraphs: the text joins the FOLLOWING (body) section — one section
/// left, A4 — whose own default header wins while its empty first-page
/// header and default footer slots take the dropped section's parts.
#[test]
fn accepting_a_deleted_section_break_joins_the_following_section() {
    let d = doc(Some(RevisionKind::Delete), "Other");
    assert_eq!(section_count(&d), 2);
    let accepted = d.resolve_all_revisions(true);
    assert_eq!(texts(&accepted), vec!["firstsecond", "third"]);
    assert!(!accepted.has_revisions());
    assert_eq!(section_count(&accepted), 1);
    assert!(
        accepted
            .blocks
            .iter()
            .filter_map(Block::as_paragraph)
            .all(|p| p.section_end.is_none())
    );
    let body = &accepted.body_section;
    assert_eq!(
        body.geometry,
        PageGeometry::a4(),
        "the following section's page"
    );
    assert_eq!(body.header_refs, refs(Some("rIdB"), Some("rIdAFirst")));
    assert_eq!(body.footer_refs, refs(Some("rIdFootA"), None));
    let mut undo = UndoStack::new(d.clone(), 100);
    undo.push(accepted);
    /* Rejecting keeps the break, the revision goes. */
    let rejected = d.resolve_all_revisions(false);
    assert_eq!(texts(&rejected), vec!["first", "second", "third"]);
    assert_eq!(section_count(&rejected), 2);
    assert!(!rejected.has_revisions());
    assert_eq!(
        rejected.nth_paragraph(0).unwrap().section_end,
        d.nth_paragraph(0).unwrap().section_end
    );
}

/// The surviving section is the NEXT terminal when there is one: a
/// second break at paragraph 1 keeps its own properties; its empty slots
/// take the dropped section's refs.
#[test]
fn the_next_terminal_survives_and_backfills_its_empty_slots() {
    let mut d = doc(Some(RevisionKind::Delete), "Other");
    let mut blocks = d.blocks.clone();
    if let Block::Paragraph(p) = &mut blocks[1] {
        p.section_end = Some(Box::new(SectionProps {
            header_refs: refs(None, None),
            ..SectionProps::default()
        }));
    }
    d.blocks = blocks;
    assert_eq!(section_count(&d), 3);
    let accepted = d.resolve_all_revisions(true);
    assert_eq!(texts(&accepted), vec!["firstsecond", "third"]);
    assert_eq!(section_count(&accepted), 2);
    let merged = accepted.nth_paragraph(0).unwrap();
    let props = merged.section_end.as_deref().expect("paragraph 1's break");
    assert_eq!(props.geometry, SectionProps::default().geometry);
    assert_eq!(props.header_refs, refs(Some("rIdA"), Some("rIdAFirst")));
    /* The body section was not the surviving terminal: untouched. */
    assert_eq!(accepted.body_section, d.body_section);
}

/// A rejected INSERTED mark carrying a section break (a tracked section
/// break) merges the paragraphs back — the break goes; accepting keeps
/// it, clean.
#[test]
fn rejecting_an_inserted_section_break_removes_it() {
    let d = doc(Some(RevisionKind::Insert), "Other");
    let rejected = d.resolve_all_revisions(false);
    assert_eq!(texts(&rejected), vec!["firstsecond", "third"]);
    assert_eq!(section_count(&rejected), 1);
    let accepted = d.resolve_all_revisions(true);
    assert_eq!(texts(&accepted), vec!["first", "second", "third"]);
    assert_eq!(section_count(&accepted), 2);
    assert!(!accepted.has_revisions());
}

/// The single decision (the mark's row, by id) resolves the same way.
#[test]
fn the_single_mark_decision_merges_the_section_break_too() {
    let d = doc(Some(RevisionKind::Delete), "Other");
    let entry = d
        .revision_entries()
        .into_iter()
        .find(|e| e.at.path == BlockPath::top(0))
        .expect("the mark row");
    let one = d.resolve_revision(&entry.at, true).expect("resolved");
    assert_eq!(texts(&one), vec!["firstsecond", "third"]);
    assert_eq!(section_count(&one), 1);
}

/// A tracked deletion over a section-break mark: the reviewer's own
/// inserted mark is REMOVED (the paragraphs merge by the rule above —
/// it used to be marked deleted instead); anyone else's is marked.
#[test]
fn deleting_your_own_inserted_section_break_removes_it() {
    let own = doc(Some(RevisionKind::Insert), ME);
    let t = own
        .try_tracked_delete_range(pos(0, 5), pos(1, 0), ME, DATE)
        .expect("recorded");
    assert_eq!(texts(&t.doc), vec!["firstsecond", "third"]);
    assert_eq!(section_count(&t.doc), 1);
    assert!(!t.doc.has_revisions());
    assert_eq!(t.end, pos(0, 5));
    let other = doc(Some(RevisionKind::Insert), "Other");
    let t = other
        .try_tracked_delete_range(pos(0, 5), pos(1, 0), ME, DATE)
        .expect("recorded");
    assert_eq!(texts(&t.doc), vec!["first", "second", "third"]);
    let kinds: Vec<RevisionKind> = t
        .doc
        .nth_paragraph(0)
        .unwrap()
        .mark_revisions
        .iter()
        .map(|r| r.kind)
        .collect();
    assert_eq!(kinds, vec![RevisionKind::Insert, RevisionKind::Delete]);
    /* Accept: the deletion wins, the break goes. */
    let accepted = t.doc.resolve_all_revisions(true);
    assert_eq!(texts(&accepted), vec!["firstsecond", "third"]);
    assert_eq!(section_count(&accepted), 1);
    /* An untracked mark with a section break, deleted with tracking
    on: marked, and accept merges it. */
    let plain = doc(None, "Other");
    let t = plain
        .try_tracked_delete_range(pos(0, 5), pos(1, 0), ME, DATE)
        .expect("recorded");
    assert_eq!(section_count(&t.doc), 2);
    let accepted = t.doc.resolve_all_revisions(true);
    assert_eq!(section_count(&accepted), 1);
    assert_eq!(
        accepted.body_section.header_refs,
        refs(Some("rIdB"), Some("rIdAFirst"))
    );
}

/// Accepting the tracked merge gives the same tree shape as the
/// untracked delete of the same range (`delete_range` — the #70 rule).
#[test]
fn accepting_matches_the_untracked_delete() {
    let plain = doc(None, "Other");
    let untracked = plain.delete_range(pos(0, 5), pos(1, 0));
    let accepted = plain
        .try_tracked_delete_range(pos(0, 5), pos(1, 0), ME, DATE)
        .unwrap()
        .doc
        .resolve_all_revisions(true);
    assert_eq!(texts(&accepted), texts(&untracked));
    assert_eq!(accepted.body_section, untracked.body_section);
    assert_eq!(section_count(&accepted), section_count(&untracked));
}
