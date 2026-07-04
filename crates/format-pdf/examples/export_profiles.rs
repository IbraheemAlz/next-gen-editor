//! Native validation helper (issue #28) — exports a fixture page under each
//! `PdfProfile` into `tmp/pdf-profiles/` so the outputs can be hashed for
//! byte-stability checks and fed to an external veraPDF binary without
//! booting the dev server + Web Worker that `tools/pdf-validate` drives.
//!
//! ```sh
//! cargo run -p format-pdf --example export_profiles
//! verapdf --flavour 1b tmp/pdf-profiles/a1b.pdf
//! verapdf --flavour 2u tmp/pdf-profiles/a2u.pdf
//! # veraPDF has no PDF/X flavour; inspect x3.pdf with `pdfinfo -box`.
//! ```

use format_pdf::{PdfProfile, export_pdf};
use layout::{LayoutBlock, Margins, PageBox, ParagraphConfig, Size, StyleSpan, layout_paragraph};
use std::collections::HashMap;
use std::sync::Arc;
use text_pipeline::{Alignment, FontStack, LoadedFont, ShapingDirection};

fn liberation_stack() -> FontStack {
    let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
    let face = LoadedFont::parse("liberation".into(), bytes).expect("parse font");
    let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
    faces.insert("liberation".to_string(), Arc::new(face));
    FontStack::from_faces(faces, "liberation")
}

fn hello_page(stack: &FontStack) -> PageBox {
    let text = "Hello world";
    let spans = [StyleSpan {
        start: 0,
        end: text.len() as u32,
        px_size: 18.0,
        color: [0, 0, 0, 255],
        bold: false,
        italic: false,
        underline: engine::UnderlineStyle::None,
        strike: false,
        bg_color: None,
        font_family: None,
        caps_transform: false,
        baseline_shift_px: 0.0,
    }];
    let mut para = layout_paragraph(ParagraphConfig {
        text,
        fonts: stack,
        spans: &spans,
        base_direction: ShapingDirection::Ltr,
        max_width: 451.0,
        line_height: 26.0,
        line_height_exact: false,
        alignment: Alignment::Start,
        indent_start_px: 0.0,
        indent_end_px: 0.0,
        first_line_indent_px: 0.0,
        hanging_indent_px: 0.0,
        marker_text: None,
        px_size_for_marker: 18.0,
        inline_objects: &[],
        tab_stops_px: &[],
    });
    para.source_paragraph_id = 0;
    PageBox {
        size: Size {
            width: 595.0,
            height: 842.0,
        },
        margins: Margins::uniform(72.0),
        blocks: vec![LayoutBlock::Paragraph(para)],
        header: None,
        footer: None,
        header_offset: 36.0,
        footer_offset: 36.0,
        footnotes: Vec::new(),
    }
}

fn main() {
    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp/pdf-profiles");
    std::fs::create_dir_all(&out_dir).expect("mkdir");
    let stack = liberation_stack();
    let page = hello_page(&stack);
    let profiles: &[(&str, PdfProfile)] = &[
        ("plain", PdfProfile::Plain),
        ("a1b", PdfProfile::A1b),
        ("a2u", PdfProfile::A2u),
        ("x3", PdfProfile::X3),
    ];
    for (name, profile) in profiles {
        let mut bytes = Vec::new();
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &["Hello world"],
            *profile,
            &mut bytes,
        )
        .expect("export");
        let path = out_dir.join(format!("{name}.pdf"));
        std::fs::write(&path, &bytes).expect("write pdf");
        println!("{name}: {} bytes -> {}", bytes.len(), path.display());
    }
}
