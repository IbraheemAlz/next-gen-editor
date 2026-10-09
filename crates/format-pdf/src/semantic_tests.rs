//! Issue #360 — outline, link annotations, XMP metadata and tagging at the
//! `format-pdf` level (box trees built by hand; the engine-wasm end-to-end
//! tests live in `crates/engine-wasm/src/pdf_semantics_tests.rs`).

use super::*;
use layout::{Margins, ParagraphConfig, Size, StyleSpan, layout_paragraph};
use std::sync::Arc;
use text_pipeline::{Alignment, ShapingDirection};

pub(crate) fn span(len: u32) -> StyleSpan {
    StyleSpan {
        start: 0,
        end: len,
        px_size: 14.0,
        color: [0, 0, 0, 255],
        bold: false,
        italic: false,
        underline: engine::UnderlineStyle::None,
        strike: false,
        bg_color: None,
        font_family: None,
        caps_transform: false,
        baseline_shift_px: 0.0,
    }
}

pub(crate) fn stack() -> FontStack {
    let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
    for (id, bytes) in [
        (
            "liberation",
            &include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf")[..],
        ),
        (
            "amiri",
            &include_bytes!("../../../ts/fonts/Amiri-Regular.ttf")[..],
        ),
    ] {
        let face = LoadedFont::parse(id.into(), bytes.to_vec()).expect("parse font");
        faces.insert(id.to_string(), Arc::new(face));
    }
    FontStack::from_faces(faces, "liberation")
}

/// One laid-out paragraph with source id `id`, `marker` in the gutter.
pub(crate) fn para(
    stack: &FontStack,
    text: &str,
    id: u32,
    marker: Option<&str>,
    rtl: bool,
) -> ParagraphBox {
    let spans = [StyleSpan {
        font_family: rtl.then(|| "amiri".into()),
        ..span(text.len() as u32)
    }];
    let mut p = layout_paragraph(ParagraphConfig {
        text,
        fonts: stack,
        spans: &spans,
        base_direction: if rtl {
            ShapingDirection::Rtl
        } else {
            ShapingDirection::Ltr
        },
        max_width: 451.0,
        line_height: 20.0,
        line_height_exact: false,
        alignment: Alignment::Start,
        indent_start_px: if marker.is_some() { 36.0 } else { 0.0 },
        indent_end_px: 0.0,
        first_line_indent_px: 0.0,
        hanging_indent_px: 0.0,
        marker_text: marker.map(str::to_string),
        px_size_for_marker: 14.0,
        inline_objects: &[],
        tab_stops_px: &[],
    });
    p.source_paragraph_id = id;
    p
}

/// An A4 page holding `blocks`, stacked from the content top.
pub(crate) fn page(mut blocks: Vec<LayoutBlock>) -> PageBox {
    let mut y = 0.0;
    for b in &mut blocks {
        b.set_origin(layout::Point { x: 0.0, y });
        y += b.size().height + 6.0;
    }
    PageBox {
        size: Size {
            width: 595.0,
            height: 842.0,
        },
        margins: Margins::uniform(72.0),
        blocks,
        header: None,
        footer: None,
        header_offset: 36.0,
        footer_offset: 36.0,
        footnotes: layout::NoteBand::default(),
        endnotes: layout::NoteBand::default(),
        hf_role: layout::HeaderRole::Default,
        page_number: 1,
        floats: Vec::new(),
    }
}

pub(crate) fn export_with(
    pages: &[PageBox],
    stack: &FontStack,
    texts: &[&str],
    sem: &PdfSemantics,
    options: PdfExportOptions,
) -> Vec<u8> {
    let mut out = Vec::new();
    export_pdf_document(pages, stack, texts, &HashMap::new(), sem, options, &mut out)
        .expect("export");
    out
}

pub(crate) fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

pub(crate) fn contains(hay: &[u8], needle: &[u8]) -> bool {
    count(hay, needle) > 0
}

/// The text of the indirect object `n 0 obj … endobj`.
pub(crate) fn object(pdf: &[u8], n: i32) -> String {
    let text = String::from_utf8_lossy(pdf);
    let head = format!("\n{n} 0 obj\n");
    let start = text.find(&head).unwrap_or_else(|| panic!("object {n}")) + head.len();
    let end = start + text[start..].find("endobj").expect("endobj");
    text[start..end].to_string()
}

/// The object number a `/Key N 0 R` entry in `dict` names.
pub(crate) fn ref_of(dict: &str, key: &str) -> i32 {
    let at = dict
        .find(&format!("/{key} "))
        .unwrap_or_else(|| panic!("/{key} in {dict}"));
    dict[at + key.len() + 2..]
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("/{key} ref in {dict}"))
}

fn heading(level: u8, title: &str) -> ParagraphSemantics {
    ParagraphSemantics {
        heading: Some(level),
        title: title.to_string(),
    }
}

/* ================================================================
Task 1 — the document outline.
================================================================ */

/// Two pages: "Intro" (H1) + body, then "Details" (H2), "Sub" (H3) and
/// "Next" (H1) on page 2.
fn outline_doc(stack: &FontStack) -> (Vec<PageBox>, Vec<&'static str>, PdfSemantics) {
    let texts = vec!["Intro", "Body text.", "Details", "Sub", "Next"];
    let p1 = page(vec![
        LayoutBlock::Paragraph(para(stack, texts[0], 0, None, false)),
        LayoutBlock::Paragraph(para(stack, texts[1], 1, None, false)),
    ]);
    let p2 = page(vec![
        LayoutBlock::Paragraph(para(stack, texts[2], 2, None, false)),
        LayoutBlock::Paragraph(para(stack, texts[3], 3, None, false)),
        LayoutBlock::Paragraph(para(stack, texts[4], 4, None, false)),
    ]);
    let sem = PdfSemantics {
        paragraphs: vec![
            heading(1, "Intro"),
            ParagraphSemantics::default(),
            heading(2, "Details"),
            heading(3, "Sub"),
            heading(1, "Next"),
        ],
    };
    (vec![p1, p2], texts, sem)
}

#[test]
fn headings_become_a_nested_outline_with_page_destinations() {
    let stack = stack();
    let (pages, texts, sem) = outline_doc(&stack);
    let pdf = export_with(
        &pages,
        &stack,
        &texts,
        &sem,
        PdfExportOptions::new(PdfProfile::A2u),
    );
    let catalog = object(&pdf, 1);
    assert!(catalog.contains("/PageMode /UseOutlines"), "{catalog}");
    let root = object(&pdf, ref_of(&catalog, "Outlines"));
    assert!(root.contains("/Type /Outlines"), "{root}");
    assert!(root.contains("/Count 4"), "{root}");

    let first = ref_of(&root, "First");
    let last = ref_of(&root, "Last");
    let intro = object(&pdf, first);
    let next = object(&pdf, last);
    assert!(intro.contains("/Title (Intro)"), "{intro}");
    assert!(next.contains("/Title (Next)"), "{next}");
    assert_eq!(ref_of(&intro, "Next"), last, "top-level siblings chain");
    assert!(
        intro.contains("/Count 2"),
        "Intro holds Details > Sub: {intro}"
    );
    let details = object(&pdf, ref_of(&intro, "First"));
    assert!(details.contains("/Title (Details)"), "{details}");
    assert_eq!(ref_of(&details, "Parent"), first);
    let sub = object(&pdf, ref_of(&details, "First"));
    assert!(sub.contains("/Title (Sub)"), "{sub}");

    /* Destinations point at the laid-out paragraph's page + top-left:
    Intro on page 1 (object 3), Details on page 2 (object 5), both at
    the content top: y = 842 - 72 = 770. */
    assert!(intro.contains("/Dest [3 0 R /XYZ 72 770 0]"), "{intro}");
    assert!(details.contains("/Dest [5 0 R /XYZ 72 770 0]"), "{details}");
}

#[test]
fn a_heading_split_across_pages_gets_one_entry_at_its_first_fragment() {
    let stack = stack();
    let texts = ["Title"];
    let pages = vec![
        page(vec![LayoutBlock::Paragraph(para(
            &stack, "Title", 0, None, false,
        ))]),
        page(vec![LayoutBlock::Paragraph(para(
            &stack, "Title", 0, None, false,
        ))]),
    ];
    let sem = PdfSemantics {
        paragraphs: vec![heading(1, "Title")],
    };
    let pdf = export_with(
        &pages,
        &stack,
        &texts,
        &sem,
        PdfExportOptions::new(PdfProfile::Plain),
    );
    assert_eq!(count(&pdf, b"/Title (Title)"), 1);
    assert!(contains(&pdf, b"/Dest [3 0 R /XYZ 72 770 0]"));
}

#[test]
fn no_headings_means_no_outline_and_identical_bytes() {
    let stack = stack();
    let (pages, texts, mut sem) = outline_doc(&stack);
    for p in &mut sem.paragraphs {
        p.heading = None;
    }
    for profile in [
        PdfProfile::Plain,
        PdfProfile::A1b,
        PdfProfile::A2u,
        PdfProfile::X3,
    ] {
        let with = export_with(&pages, &stack, &texts, &sem, PdfExportOptions::new(profile));
        let mut without = Vec::new();
        export_pdf(&pages, &stack, &texts, profile, &mut without).expect("export");
        assert_eq!(with, without, "{profile:?}");
        assert!(!contains(&with, b"/Outlines"), "{profile:?}");
        assert!(!contains(&with, b"/PageMode"), "{profile:?}");
    }
}

#[test]
fn an_empty_heading_title_gets_no_entry() {
    let stack = stack();
    let texts = ["\t"];
    let pages = vec![page(vec![LayoutBlock::Paragraph(para(
        &stack, "\t", 0, None, false,
    ))])];
    let sem = PdfSemantics {
        paragraphs: vec![heading(1, "  ")],
    };
    let pdf = export_with(
        &pages,
        &stack,
        &texts,
        &sem,
        PdfExportOptions::new(PdfProfile::A1b),
    );
    assert!(!contains(&pdf, b"/Outlines"));
}
