//! Issue #395 — paragraph borders defined on STYLES (`styles.xml`
//! `<w:pPr><w:pBdr>`): read into the style's paragraph properties,
//! cascaded `basedOn` → direct per edge (a direct `w:val="nil"` removes
//! the inherited edge), logical `<w:start>` / `<w:end>` resolved against
//! the PARAGRAPH's resolved direction — identically by the reader and by
//! the engine's re-cascade (ApplyStyle / ModifyStyle).

use engine::{
    Block, BlockPath, BorderStyle, DocumentTree, LogicalPos, ParaProperties, TextDirection,
};
use format_docx::test_fixtures::styled_paragraph_borders_docx;
use format_docx::{read_docx, write_docx};
use std::io::Read;

fn part(docx: &[u8], name: &str) -> String {
    let mut a = zip::ZipArchive::new(std::io::Cursor::new(docx)).expect("zip");
    let mut s = String::new();
    a.by_name(name)
        .expect("entry")
        .read_to_string(&mut s)
        .expect("utf8");
    s
}

/// The PAINTED edges of `props` (`BorderStyle::None` strokes paint
/// nothing), `T` / `L` / `B` / `R` + the stroke colour.
fn painted(props: &ParaProperties) -> String {
    let Some(b) = props.borders.as_ref() else {
        return String::new();
    };
    [
        ("T", &b.top),
        ("L", &b.left),
        ("B", &b.bottom),
        ("R", &b.right),
    ]
    .into_iter()
    .filter_map(|(side, edge)| {
        let s = edge.as_ref()?;
        if s.style == BorderStyle::None {
            return None;
        }
        let [r, g, bl, _] = s.color.unwrap_or([0, 0, 0, 255]);
        Some(format!("{side}:{r:02X}{g:02X}{bl:02X}"))
    })
    .collect::<Vec<_>>()
    .join(" ")
}

fn all_painted(doc: &DocumentTree) -> Vec<String> {
    (0..doc.paragraph_count())
        .map(|i| painted(&doc.nth_paragraph(i).unwrap().props))
        .collect()
}

const EXPECTED: [&str; 7] = [
    "B:4F81BD",
    "T:FF0000 L:FF0000 B:FF0000",
    "L:FF0000 B:FF0000",
    "L:0000FF",
    "R:0000FF",
    "R:0000FF",
    "L:0000FF",
];

fn at(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

#[test]
fn style_borders_cascade_per_edge_by_the_paragraph_direction() {
    let doc = read_docx(&styled_paragraph_borders_docx())
        .expect("read")
        .document;
    assert_eq!(all_painted(&doc), EXPECTED);
    /* The style definitions carry them (the engine's mirror). */
    let title = doc.styles["Title"]
        .para
        .borders
        .clone()
        .expect("Title pBdr");
    let bottom = title.bottom.expect("bottom edge");
    assert_eq!(
        (bottom.style, bottom.size_eighth_pt, bottom.color),
        (BorderStyle::Single, 8, Some([0x4F, 0x81, 0xBD, 255]))
    );
    /* A style's logical edge sits by the style's OWN direction. */
    let rtl_rule = &doc.styles["RtlRule"].para;
    assert_eq!(rtl_rule.direction, Some(TextDirection::Rtl));
    assert!(rtl_rule.borders.as_ref().unwrap().right.is_some());
    assert!(rtl_rule.border_spelling.start);
    /* Direction: the direct bidi turns a style's start edge around. */
    let p = |i| doc.nth_paragraph(i).unwrap();
    assert_eq!(p(4).props.direction, Some(TextDirection::Rtl));
    assert_eq!(p(6).props.direction, Some(TextDirection::Ltr));
    assert!(p(4).props.border_spelling.start && p(6).props.border_spelling.start);
    /* The direct nil is a set "no border" edge on the direct overrides. */
    let direct = p(2).direct_overrides.borders.clone().expect("direct pBdr");
    assert_eq!(direct.top.map(|s| s.style), Some(BorderStyle::None));
    assert!(direct.bottom.is_none() && direct.left.is_none());
    assert!(
        p(4).direct_overrides.borders.is_none(),
        "inherited, not direct"
    );
}

#[test]
fn zero_edit_save_is_byte_identical() {
    let src = styled_paragraph_borders_docx();
    let archive = read_docx(&src).expect("read");
    let saved = write_docx(&archive, &archive.document).expect("write");
    for name in ["word/document.xml", "word/styles.xml"] {
        assert_eq!(part(&saved, name), part(&src, name), "{name}");
    }
}

/// The engine's re-cascade (ApplyStyle → `recompute_paragraph_props`)
/// lands every edge exactly where the reader did, and a re-applied style
/// keeps the paragraph's direct nil.
#[test]
fn engine_recascade_matches_the_reader() {
    let doc = read_docx(&styled_paragraph_borders_docx())
        .expect("read")
        .document;
    for i in 0..doc.paragraph_count() {
        let p = doc.nth_paragraph(i).unwrap();
        let len = p.text.len() as u32;
        let again = doc.set_paragraph_style(at(i, 0), at(i, len), p.style_id.clone());
        assert_eq!(
            again.nth_paragraph(i).unwrap().props,
            p.props,
            "paragraph {i}"
        );
    }
    /* `Box` + direct top nil → `StartRule`: the style's blue start, the
    nil top still set (and unpainted), nothing of `Box` left. */
    let restyled = doc.set_paragraph_style(at(2, 0), at(2, 6), Some("StartRule".into()));
    assert_eq!(
        painted(&restyled.nth_paragraph(2).unwrap().props),
        "L:0000FF"
    );
    /* `StartRule` → `RtlRule` on paragraph 4: RTL by its own direct
    bidi, so the new style's start lands right. */
    let restyled = doc.set_paragraph_style(at(4, 0), at(4, 9), Some("RtlRule".into()));
    assert_eq!(
        painted(&restyled.nth_paragraph(4).unwrap().props),
        "R:0000FF",
        "paragraph 4 is RTL by its own bidi"
    );
    /* `StartRule` → `RtlRule` on paragraph 3 (no direct bidi): the new
    style turns the paragraph RTL, and its start edge with it. */
    let restyled = doc.set_paragraph_style(at(3, 0), at(3, 9), Some("RtlRule".into()));
    let p3 = restyled.nth_paragraph(3).unwrap();
    assert_eq!(p3.props.direction, Some(TextDirection::Rtl));
    assert_eq!(painted(&p3.props), "R:0000FF");
}

/// ModifyStyle regenerates `styles.xml` from the engine's style table:
/// the borders (logical spelling included) are written back and re-read
/// into the same painted edges.
#[test]
fn modify_style_writes_the_style_borders_back() {
    let archive = read_docx(&styled_paragraph_borders_docx()).expect("read");
    let modified = archive.document.modify_style(
        "Box",
        Some(ParaProperties {
            keep_next: Some(true),
            ..Default::default()
        }),
        None,
        None,
        None,
    );
    assert!(modified.styles_dirty);
    let saved = write_docx(&archive, &modified).expect("write");
    let styles = part(&saved, "word/styles.xml");
    assert!(styles.contains("<w:pBdr>"), "{styles}");
    assert!(
        styles.contains("<w:start "),
        "logical spelling kept: {styles}"
    );
    let back = read_docx(&saved).expect("re-read").document;
    assert_eq!(all_painted(&back), EXPECTED);
}

/// Every paragraph regenerated (no source bytes left, so each `<w:pPr>`
/// is baked from the resolved model — the style's edges, the direct nil
/// as `w:val="none"`, a logical edge under its `w:start` name) re-reads
/// into the same painted edges.
#[test]
fn regenerated_paragraphs_paint_the_same_edges() {
    let archive = read_docx(&styled_paragraph_borders_docx()).expect("read");
    let mut doc = archive.document.clone();
    doc.blocks = doc
        .blocks
        .iter()
        .cloned()
        .map(|b| match b {
            Block::Paragraph(mut p) => {
                p.dirty = true;
                p.source_xml = None;
                p.source_markup = None;
                Block::Paragraph(p)
            }
            other => other,
        })
        .collect();
    let saved = write_docx(&archive, &doc).expect("write");
    format_docx::check_document_xml_well_formed(&saved).expect("well-formed");
    let xml = part(&saved, "word/document.xml");
    assert!(xml.contains("<w:pBdr>"), "{xml}");
    let back = read_docx(&saved).expect("re-read").document;
    assert_eq!(all_painted(&back), EXPECTED);
}
