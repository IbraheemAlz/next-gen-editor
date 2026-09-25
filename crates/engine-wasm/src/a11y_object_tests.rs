//! Issue #215 — inline images and inline text boxes in the accessibility
//! mirror.
//!
//! Both used to leave their `U+FFFC` placeholder byte verbatim in
//! `A11yRun.text` (screen readers announce "object replacement
//! character"). Mirroring the #203 note-reference pattern, the sentinel
//! run now carries `object: Some(A11yObjectRef)` instead, with `text`
//! empty — the object IS the run, not a marker.

use super::*;

/// A single-paragraph document whose one inline object sits at byte `at`.
fn doc_with_object(text: &str, at: u32, kind: engine::InlineKind) -> DocumentTree {
    let mut d = DocumentTree::new();
    d.blocks = vec![engine::Block::Paragraph(engine::Paragraph {
        text: text.to_string(),
        inline_objects: vec![engine::InlineObject {
            at,
            kind,
            anchor: None,
            source_xml: None,
        }],
        ..Default::default()
    })]
    .into();
    d
}

fn run_texts(node: &A11yNode) -> Vec<String> {
    let A11yNode::Paragraph(p) = node else {
        panic!("expected a paragraph, got {node:?}");
    };
    p.runs.iter().map(|r| r.text.clone()).collect()
}

fn object_of(node: &A11yNode) -> &bridge::A11yObjectRef {
    let A11yNode::Paragraph(p) = node else {
        panic!("expected a paragraph, got {node:?}");
    };
    p.runs
        .iter()
        .find_map(|r| r.object.as_ref())
        .expect("object run")
}

/// An inline image's sentinel becomes an empty-text run carrying
/// `object: Some(A11yObjectRef { kind: Image, id: <media key>, alt })` —
/// never the raw `U+FFFC` byte.
#[test]
fn inline_image_becomes_an_object_run_with_its_media_key_and_alt() {
    let doc = doc_with_object(
        "a\u{FFFC}b",
        1,
        engine::InlineKind::Image {
            rel_id: "rId9".to_string(),
            width_emu: 914_400,
            height_emu: 914_400,
            media_key: Some("word/media/image7.png".to_string()),
        },
    );
    let mut d = doc.blocks[0].clone();
    let engine::Block::Paragraph(p) = &mut d else {
        unreachable!()
    };
    p.inline_objects[0].source_xml = Some(
        br#"<w:drawing><wp:inline><wp:docPr id="1" name="Diagram" descr="A flow diagram"/></wp:inline></w:drawing>"#
            .to_vec(),
    );
    let mut doc = doc;
    doc.blocks[0] = d;

    let engine = crate::tests::test_engine_with_doc(doc);
    let nodes = engine.build_a11y_nodes();
    assert_eq!(nodes.len(), 1, "{nodes:#?}");
    assert_eq!(
        run_texts(&nodes[0]),
        vec!["a", "", "b"],
        "the object run's text is empty, not U+FFFC"
    );
    for run in run_texts(&nodes[0]) {
        assert!(!run.contains('\u{FFFC}'), "no raw sentinel: {run:?}");
    }
    let obj = object_of(&nodes[0]);
    assert_eq!(obj.kind, bridge::A11yObjectKind::Image);
    assert_eq!(
        obj.id, "word/media/image7.png",
        "keyed by media key, not rel_id"
    );
    assert_eq!(obj.alt.as_deref(), Some("A flow diagram"));
}

/// An image with neither `media_key` nor a captured `descr`/`name` still
/// gets an object run (falling back to `rel_id`, `alt: None`) — never the
/// raw placeholder.
#[test]
fn inline_image_without_media_key_or_label_falls_back_cleanly() {
    let doc = doc_with_object(
        "\u{FFFC}",
        0,
        engine::InlineKind::Image {
            rel_id: "rId3".to_string(),
            width_emu: 100,
            height_emu: 100,
            media_key: None,
        },
    );
    let engine = crate::tests::test_engine_with_doc(doc);
    let nodes = engine.build_a11y_nodes();
    assert_eq!(run_texts(&nodes[0]), vec![""]);
    let obj = object_of(&nodes[0]);
    assert_eq!(obj.kind, bridge::A11yObjectKind::Image);
    assert_eq!(obj.id, "rId3", "no media_key => rel_id doubles as the key");
    assert_eq!(obj.alt, None);
}

/// An inline text box's sentinel becomes an object run whose `id` matches
/// the `A11yTextBox` region's own `id` — the mirror can link the
/// reference to the region the same way a note reference links to its
/// note.
#[test]
fn inline_text_box_object_id_matches_its_region_id() {
    let story = engine::TextBoxStory {
        source_xml: Some(
            r#"<w:drawing><wp:inline><wp:docPr id="2" name="Callout" descr="Side note"/></wp:inline></w:drawing>"#
                .to_string(),
        ),
        ..Default::default()
    };
    let doc = doc_with_object(
        "c\u{FFFC}d",
        1,
        engine::InlineKind::TextBox {
            width_emu: 914_400,
            height_emu: 457_200,
            story: Box::new(story),
        },
    );
    let engine = crate::tests::test_engine_with_doc(doc);
    let nodes = engine.build_a11y_nodes();
    assert_eq!(
        nodes.len(),
        2,
        "the anchor paragraph + the box region: {nodes:#?}"
    );
    assert_eq!(run_texts(&nodes[0]), vec!["c", "", "d"]);
    let obj = object_of(&nodes[0]);
    assert_eq!(obj.kind, bridge::A11yObjectKind::TextBox);
    assert_eq!(obj.alt.as_deref(), Some("Side note"));

    let A11yNode::TextBox(region) = &nodes[1] else {
        panic!("{:?}", nodes[1]);
    };
    assert_eq!(obj.id, region.id, "object ref addresses the SAME region");
}
