//! Issue #181 — note bands vs. the viewport-culled band
//! (`LazyLayoutState` / `Command::ExpandLayout`).
//!
//! A culled band stops at a block boundary. Two note shapes end there:
//!
//! - a FOOTNOTE cut on the cull page whose continuation is still waiting
//!   for the next page's band — the band's `finish` drains it onto
//!   notes-only pages that a longer band fills with body text, so the
//!   pages from the cull page on are PROVISIONAL (`LazyLayoutInfo::
//!   open_from_page`, the #93 mechanism), never a `FastPathMismatch`;
//! - document-end ENDNOTES, which only a band that reaches the end can
//!   place. They trail the last body block, so their absence from a
//!   shorter band never disagrees with the body prefix the verifier
//!   checks.

use super::*;

const SENTINEL: char = '\u{FFFC}';

/// A paragraph `"<head>\u{FFFC}<tail>"` whose sentinel references note
/// `id` of `kind`.
fn note_ref_para(head: &str, kind: engine::NoteKind, id: u32, tail: &str) -> engine::Block {
    let reference = match kind {
        engine::NoteKind::Footnote => engine::InlineKind::FootnoteRef {
            id,
            custom_mark_follows: false,
        },
        engine::NoteKind::Endnote => engine::InlineKind::EndnoteRef {
            id,
            custom_mark_follows: false,
        },
    };
    engine::Block::Paragraph(engine::Paragraph {
        text: format!("{head}{SENTINEL}{tail}"),
        inline_objects: vec![engine::InlineObject {
            at: head.len() as u32,
            kind: reference,
            anchor: None,
            source_xml: None,
        }],
        ..Default::default()
    })
}

/// Word's note body shape: the self-mark, a space, the text.
fn note_story(kind: engine::NoteKind, id: u32, text: &str) -> engine::NoteStory {
    engine::NoteStory {
        id: id as i32,
        kind,
        note_type: engine::NoteType::Normal,
        body: vec![engine::Block::Paragraph(engine::Paragraph {
            text: format!("{SENTINEL} {text}"),
            inline_objects: vec![engine::InlineObject {
                at: 0,
                kind: engine::InlineKind::NoteSelfRef { kind },
                anchor: None,
                source_xml: None,
            }],
            ..Default::default()
        })],
        source_xml: None,
        dirty: false,
    }
}

fn prose(i: usize) -> engine::Block {
    engine::Block::Paragraph(engine::Paragraph {
        text: format!(
            "Paragraph {i}: sphinx of black quartz, judge my vow; pack my box with five dozen \
             liquor jugs."
        ),
        ..Default::default()
    })
}

/// Paragraph 0 references endnote 1 (document end); `before` prose
/// paragraphs follow; then a ONE-line paragraph referencing footnote 1,
/// whose body runs for several pages (it splits under its reference and
/// continues across the next pages' bands); then `after` prose.
/// Returns the document and the reference paragraph's block index.
fn long_footnote_doc(before: usize, after: usize) -> (DocumentTree, u32) {
    let mut d = DocumentTree::new();
    let mut blocks = vec![note_ref_para(
        "The opening line cites the appendix",
        engine::NoteKind::Endnote,
        1,
        ".",
    )];
    blocks.extend((1..=before).map(prose));
    let ref_idx = blocks.len() as u32;
    blocks.push(note_ref_para(
        "See the note",
        engine::NoteKind::Footnote,
        1,
        ".",
    ));
    blocks.extend((before + 1..=before + after).map(prose));
    d.blocks = blocks.into();
    let long_note = (0..70)
        .map(|i| format!("Note sentence {i} keeps the footnote running across the page."))
        .collect::<Vec<_>>()
        .join(" ");
    d.footnote_stories
        .insert(1, note_story(engine::NoteKind::Footnote, 1, &long_note));
    d.endnote_stories.insert(
        1,
        note_story(engine::NoteKind::Endnote, 1, "The appendix endnote."),
    );
    (d, ref_idx)
}

/// The page carrying (the head of) top-level block `block`.
fn page_of_block(paths: &[Vec<EngineBlockPath>], block: u32) -> Option<usize> {
    let want = EngineBlockPath::top(block);
    paths.iter().position(|pp| pp.contains(&want))
}

/// Acceptance (#181): a band culled right after a paragraph whose
/// footnote continues across several pages flags the cull page
/// provisional — its drain pages (notes only) are what a longer band
/// fills with body text, which a plain prefix check rejects — and
/// expanding through it never demotes. The final geometry equals a full
/// layout, and the document-end endnote the band never placed appears
/// exactly once.
#[test]
fn a_band_culled_mid_footnote_continuation_expands_without_demotion() {
    let (doc, ref_idx) = long_footnote_doc(150, 120);
    let engine = tests::test_engine_with_doc(doc);
    let (full, _, paths, full_info) = engine.build_pages(1.0, false, None).expect("full");
    assert!(
        full_info.degradations.is_empty(),
        "{:?}",
        full_info.degradations
    );
    let p_ref = page_of_block(&paths, ref_idx).expect("reference page");
    assert!(p_ref >= 1, "the reference sits past the first page");
    let continued: usize = full
        .iter()
        .filter(|p| {
            p.footnotes
                .entries
                .iter()
                .any(|e| e.continued_from_previous)
        })
        .count();
    assert!(
        continued >= 2,
        "the footnote continues across at least two more pages, got {continued}"
    );
    assert_eq!(
        full.iter()
            .flat_map(|p| p.endnotes.entries.iter())
            .filter(|e| !e.continued_from_previous)
            .count(),
        1,
        "the full layout places the document-end endnote once"
    );
    /* Aim the cull at the top of the block AFTER the reference: the
    committed height there is the reference paragraph's bottom on its
    page (the cull only stops at block boundaries). */
    let gap = render::scene::PAGE_GAP_PT;
    let above: f32 = full[..p_ref].iter().map(|p| p.size.height + gap).sum();
    let ref_box = full[p_ref]
        .blocks
        .iter()
        .zip(&paths[p_ref])
        .find(|(_, p)| **p == EngineBlockPath::top(ref_idx))
        .map(|(b, _)| (b.origin().y, b.size().height))
        .expect("reference block");
    let runway = lazy_runway(engine.lazy_layout.viewport_h, 1.0);
    let target = (1..40)
        .map(|d| above + ref_box.0 + ref_box.1 - runway - d as f32 * 0.5)
        .find(|&target| {
            engine.invalidate_layout_snapshot();
            engine
                .ensure_layout_snapshot(1.0, false, Some(target))
                .expect("band 1");
            let snap = engine.layout_snapshot.borrow();
            let s = snap.as_ref().expect("snapshot");
            !s.info.is_full_layout && s.info.open_from_page == Some(p_ref)
        })
        .expect("a band culled while the footnote continuation is pending");
    {
        let snap = engine.layout_snapshot.borrow();
        let s = snap.as_ref().expect("snapshot");
        assert!(s.info.degradations.is_empty(), "{:?}", s.info.degradations);
        assert!(
            s.pages.len() > p_ref + 2,
            "the band drained the continuation onto notes-only pages"
        );
        assert!(
            s.pages[p_ref + 1..].iter().all(|p| p.blocks.is_empty()),
            "pages past the cull carry only the continuation"
        );
        assert!(
            layout::verify_prefix(&s.pages, &full).is_err(),
            "without the provisional flag the drain pages would demote"
        );
        assert_eq!(
            layout::verify_prefix_open(&s.pages, &full, Some(p_ref)),
            Ok(()),
            "every page before the cull page is final"
        );
        assert!(
            s.pages.iter().all(|p| p.endnotes.is_empty()),
            "a culled band never places the document-end endnote"
        );
    }
    engine
        .ensure_layout_snapshot(1.0, false, Some(target + 2500.0))
        .expect("band 2");
    {
        let snap = engine.layout_snapshot.borrow();
        let s = snap.as_ref().expect("snapshot");
        assert!(
            s.info.degradations.is_empty(),
            "no FastPathMismatch on expand: {:?}",
            s.info.degradations
        );
        assert!(!s.info.is_full_layout, "still a culled band");
    }
    engine
        .ensure_layout_snapshot(1.0, false, None)
        .expect("full");
    let snap = engine.layout_snapshot.borrow();
    let s = snap.as_ref().expect("snapshot");
    assert!(s.info.is_full_layout);
    assert!(s.info.degradations.is_empty(), "{:?}", s.info.degradations);
    let fp = layout::geometry_fingerprint(&s.pages);
    assert_eq!(
        fp,
        layout::geometry_fingerprint(&full),
        "final geometry equals a full layout"
    );
    eprintln!("NOTE BAND FINGERPRINT long_footnote = {fp:#x}");
    assert_eq!(fp, PINNED_LONG_FOOTNOTE, "long-footnote fixture moved");
}

const PINNED_LONG_FOOTNOTE: u64 = 0xde9dc2ca1a2f4f0c;

/// Issue #181 (a) — document-end endnotes alone never make a band
/// provisional: the band simply ends before them, verifies as a prefix
/// of the full layout, and the expand that reaches the end places them
/// without a demotion.
#[test]
fn document_end_endnotes_never_demote_an_expanding_band() {
    let (mut doc, _) = long_footnote_doc(300, 0);
    /* Drop the footnote: endnotes only. */
    doc.footnote_stories.clear();
    let engine = tests::test_engine_with_doc(doc);
    let (full, ..) = engine.build_pages(1.0, false, None).expect("full");
    assert!(
        full.last().is_some_and(|p| !p.endnotes.is_empty()),
        "the endnote trails the document"
    );
    engine
        .ensure_layout_snapshot(1.0, false, Some(600.0))
        .expect("band 1");
    {
        let snap = engine.layout_snapshot.borrow();
        let s = snap.as_ref().expect("snapshot");
        assert!(!s.info.is_full_layout);
        assert_eq!(s.info.open_from_page, None, "nothing provisional");
        assert!(s.pages.iter().all(|p| p.endnotes.is_empty()));
        assert_eq!(layout::verify_prefix(&s.pages, &full), Ok(()));
    }
    engine
        .ensure_layout_snapshot(1.0, false, Some(4000.0))
        .expect("band 2");
    engine
        .ensure_layout_snapshot(1.0, false, None)
        .expect("full");
    let snap = engine.layout_snapshot.borrow();
    let s = snap.as_ref().expect("snapshot");
    assert!(s.info.is_full_layout);
    assert!(s.info.degradations.is_empty(), "{:?}", s.info.degradations);
    assert_eq!(
        layout::geometry_fingerprint(&s.pages),
        layout::geometry_fingerprint(&full)
    );
}
