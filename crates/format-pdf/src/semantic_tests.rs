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
        ..Default::default()
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

/* ================================================================
Task 2 — link annotations.
================================================================ */

fn uri(start: u32, end: u32, target: &str) -> LinkSpan {
    LinkSpan {
        start,
        end,
        target: LinkTarget::Uri(target.into()),
    }
}

/// Every `/Rect [x0 y0 x1 y1]` of a `/Subtype /Link` annotation, in file
/// order.
fn link_rects(pdf: &[u8]) -> Vec<[f32; 4]> {
    let text = String::from_utf8_lossy(pdf);
    let mut out = Vec::new();
    for (at, _) in text.match_indices("/Subtype /Link") {
        let obj_start = text[..at].rfind(" 0 obj").expect("obj");
        let dict = &text[obj_start..at + text[at..].find("endobj").unwrap()];
        let r = dict.find("/Rect [").expect("rect") + 7;
        let nums: Vec<f32> = dict[r..dict[r..].find(']').unwrap() + r]
            .split_whitespace()
            .map(|n| n.parse().unwrap())
            .collect();
        out.push([nums[0], nums[1], nums[2], nums[3]]);
    }
    out
}

#[test]
fn an_external_link_gets_a_uri_annotation_over_its_glyphs() {
    let stack = stack();
    let text = "Visit the example site today.";
    let pages = vec![page(vec![LayoutBlock::Paragraph(para(
        &stack, text, 0, None, false,
    ))])];
    let sem = PdfSemantics {
        paragraphs: vec![ParagraphSemantics {
            links: vec![uri(10, 22, "https://example.com/a b?q=é")],
            ..Default::default()
        }],
    };
    for profile in [PdfProfile::Plain, PdfProfile::A1b, PdfProfile::A2u] {
        let pdf = export_with(
            &pages,
            &stack,
            &[text],
            &sem,
            PdfExportOptions::new(profile),
        );
        let s = String::from_utf8_lossy(&pdf);
        assert_eq!(count(&pdf, b"/Subtype /Link"), 1, "{profile:?}");
        assert!(s.contains("/Border [0 0 0]"), "{profile:?}");
        assert!(s.contains("/F 4"), "{profile:?}");
        assert!(s.contains("/S /URI"), "{profile:?}");
        assert!(
            s.contains("/URI (https://example.com/a%20b?q=%C3%A9)"),
            "{profile:?}: percent-encoded ASCII URI"
        );
        assert!(s.contains("/Contents (example site)"), "{profile:?}");
        assert!(s.contains("/Annots ["), "{profile:?}");
        assert!(!s.contains("JavaScript"), "{profile:?}");
        /* The rect spans "example site" — inside the line, not the
        whole paragraph width, on the first line's band. */
        let [x0, y0, x1, y1] = link_rects(&pdf)[0];
        assert!(
            x0 > 72.0 + 30.0 && x1 < 72.0 + 200.0 && x1 > x0,
            "{x0}..{x1}"
        );
        assert!(
            y1 <= 770.0 + 0.01 && y0 < y1 && y1 - y0 < 30.0,
            "{y0}..{y1}"
        );
    }
}

#[test]
fn a_link_wrapping_over_lines_gets_one_annotation_per_line() {
    let stack = stack();
    let text = "Lead words then a much longer linked passage that keeps going \
                well past the right margin so the layout has to wrap it onto \
                the next line and maybe one more after that for good measure.";
    let pages = vec![page(vec![LayoutBlock::Paragraph(para(
        &stack, text, 0, None, false,
    ))])];
    let lines = pages[0].blocks[0].as_paragraph().unwrap().lines.len();
    assert!(lines >= 3, "fixture wraps: {lines} lines");
    let sem = PdfSemantics {
        paragraphs: vec![ParagraphSemantics {
            links: vec![uri(16, text.len() as u32, "https://example.com/")],
            ..Default::default()
        }],
    };
    let pdf = export_with(
        &pages,
        &stack,
        &[text],
        &sem,
        PdfExportOptions::new(PdfProfile::A2u),
    );
    let rects = link_rects(&pdf);
    assert_eq!(rects.len(), lines, "one annotation per line");
    for w in rects.windows(2) {
        assert!(w[1][3] <= w[0][1] + 0.01, "lines stack downward: {w:?}");
    }
}

#[test]
fn an_internal_link_targets_the_bookmarked_paragraph() {
    let stack = stack();
    let texts = ["See the appendix.", "Appendix"];
    let p1 = page(vec![LayoutBlock::Paragraph(para(
        &stack, texts[0], 0, None, false,
    ))]);
    let p2 = page(vec![LayoutBlock::Paragraph(para(
        &stack, texts[1], 1, None, false,
    ))]);
    let sem = PdfSemantics {
        paragraphs: vec![
            ParagraphSemantics {
                links: vec![
                    LinkSpan {
                        start: 8,
                        end: 16,
                        target: LinkTarget::Bookmark("_Toc1".into()),
                    },
                    LinkSpan {
                        start: 0,
                        end: 3,
                        target: LinkTarget::Bookmark("missing".into()),
                    },
                ],
                ..Default::default()
            },
            ParagraphSemantics {
                bookmarks: vec!["_Toc1".into()],
                ..Default::default()
            },
        ],
    };
    let pdf = export_with(
        &[p1, p2],
        &stack,
        &texts,
        &sem,
        PdfExportOptions::new(PdfProfile::A1b),
    );
    let s = String::from_utf8_lossy(&pdf);
    assert_eq!(
        count(&pdf, b"/Subtype /Link"),
        1,
        "the dangling one is dropped"
    );
    /* Page 2 is object 5; the bookmark paragraph sits at the content top. */
    assert!(s.contains("/Dest [5 0 R /XYZ 72 770 0]"), "{s}");
    assert!(!s.contains("/S /URI"));
}

#[test]
fn script_uris_and_pdfx3_get_no_annotation() {
    let stack = stack();
    let text = "click me";
    let pages = vec![page(vec![LayoutBlock::Paragraph(para(
        &stack, text, 0, None, false,
    ))])];
    let sem = |target: &str| PdfSemantics {
        paragraphs: vec![ParagraphSemantics {
            links: vec![uri(0, 5, target)],
            ..Default::default()
        }],
    };
    for bad in [
        "javascript:alert(1)",
        " JavaScript :x",
        "data:text/html,x",
        "  ",
    ] {
        let pdf = export_with(
            &pages,
            &stack,
            &[text],
            &sem(bad),
            PdfExportOptions::new(PdfProfile::A2u),
        );
        assert!(!contains(&pdf, b"/Annots"), "{bad:?}");
    }
    let pdf = export_with(
        &pages,
        &stack,
        &[text],
        &sem("https://example.com"),
        PdfExportOptions::new(PdfProfile::X3),
    );
    assert!(
        !contains(&pdf, b"/Annots"),
        "X-3 writes no link annotations"
    );
}

#[test]
fn rtl_link_rect_covers_the_visual_glyphs() {
    let stack = stack();
    let text = "مرحبا بالعالم الجميل";
    let pages = vec![page(vec![LayoutBlock::Paragraph(para(
        &stack, text, 0, None, true,
    ))])];
    /* Link the middle word. */
    let start = text.find("بالعالم").unwrap() as u32;
    let end = start + "بالعالم".len() as u32;
    let sem = PdfSemantics {
        paragraphs: vec![ParagraphSemantics {
            links: vec![uri(start, end, "https://example.org")],
            ..Default::default()
        }],
    };
    let pdf = export_with(
        &pages,
        &stack,
        &[text],
        &sem,
        PdfExportOptions::new(PdfProfile::A2u),
    );
    let rects = link_rects(&pdf);
    assert_eq!(rects.len(), 1);
    let line = &pages[0].blocks[0].as_paragraph().unwrap().lines[0];
    let line_w: f32 = line
        .runs
        .iter()
        .flat_map(|r| &r.glyphs)
        .map(|g| g.x_advance)
        .sum();
    let [x0, _, x1, _] = rects[0];
    assert!(
        x1 - x0 > 5.0 && x1 - x0 < line_w * 0.8,
        "{x0}..{x1} of {line_w}"
    );
}

#[test]
fn sanitize_uri_percent_encodes_and_refuses_scripts() {
    use super::semantic::sanitize_uri;
    assert_eq!(
        sanitize_uri(" mailto:a@b.c ").as_deref(),
        Some(&b"mailto:a@b.c"[..])
    );
    assert_eq!(
        sanitize_uri("https://x.y/ü").as_deref(),
        Some(&b"https://x.y/%C3%BC"[..])
    );
    assert_eq!(sanitize_uri("VBScript:msgbox"), None);
    assert_eq!(sanitize_uri(""), None);
}
