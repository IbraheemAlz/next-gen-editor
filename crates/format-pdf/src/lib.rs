//! PDF export — translates the layout box tree into a single-page PDF with the
//! shaped glyphs placed at absolute positions and the fonts fully embedded.
//!
//! Full font embedding only (no subsetting): each used face goes into the PDF
//! as a `Type0` / `CIDFontType2` font with `Identity-H` encoding, so the glyph
//! ids the shaper already produced map straight into the content stream — no
//! re-encoding, and Arabic shaping is preserved exactly.
//!
//! Coordinate spaces differ. The layout engine puts its origin at the page
//! top-left with y growing **down**; PDF user space puts its origin at the
//! bottom-left with y growing **up**. Every glyph's y is therefore inverted:
//! `pdf_y = page_height - layout_y`.

use layout::PageBox;
use pdf_writer::types::{CidFontType, FontFlags, SystemInfo};
use pdf_writer::{Content, Name, Pdf, Rect, Ref, Str};
use text_pipeline::{FontStack, LoadedFont};

/// The four indirect objects + resource name an embedded font occupies.
struct FontObj {
    type0: Ref,
    cid: Ref,
    descriptor: Ref,
    file: Ref,
    resource: String,
}

/// Export `page` to a single-page PDF, appending the bytes to `out`.
///
/// `fonts` must contain every face referenced by the page's runs; each is
/// embedded in full.
pub fn export_pdf(page: &PageBox, fonts: &FontStack, out: &mut Vec<u8>) -> Result<(), String> {
    let page_w = page.size.width;
    let page_h = page.size.height;

    /* Distinct fonts referenced by the page, in first-seen order. */
    let mut used: Vec<&str> = Vec::new();
    for para in &page.paragraphs {
        for line in &para.lines {
            for run in &line.runs {
                if !used.contains(&run.font.as_str()) {
                    used.push(run.font.as_str());
                }
            }
        }
    }

    let mut pdf = Pdf::new();
    let mut next = 1_i32;
    let mut alloc = || {
        let r = Ref::new(next);
        next += 1;
        r
    };
    let catalog_id = alloc();
    let pages_id = alloc();
    let page_id = alloc();
    let content_id = alloc();
    let font_objs: Vec<(String, FontObj)> = used
        .iter()
        .enumerate()
        .map(|(i, id)| {
            (
                (*id).to_string(),
                FontObj {
                    type0: alloc(),
                    cid: alloc(),
                    descriptor: alloc(),
                    file: alloc(),
                    resource: format!("F{i}"),
                },
            )
        })
        .collect();

    /* Content stream — must be a live binding while its `Stream` writer runs. */
    let content = build_content(page, &font_objs);
    pdf.stream(content_id, &content);

    pdf.catalog(catalog_id).pages(pages_id);
    let media = Rect::new(0.0, 0.0, page_w, page_h);
    pdf.pages(pages_id)
        .kids([page_id])
        .count(1)
        .media_box(media);
    {
        let mut p = pdf.page(page_id);
        p.parent(pages_id);
        p.media_box(media);
        p.contents(content_id);
        let mut resources = p.resources();
        let mut font_dict = resources.fonts();
        for (_, fo) in &font_objs {
            font_dict.pair(Name(fo.resource.as_bytes()), fo.type0);
        }
    }

    for (id, fo) in &font_objs {
        let face = fonts
            .face(id)
            .ok_or_else(|| format!("export_pdf: font `{id}` not in the stack"))?;
        embed_font(&mut pdf, id, face, fo);
    }

    out.extend_from_slice(&pdf.finish());
    Ok(())
}

/// Build the page content stream: one positioned glyph-show per glyph.
///
/// Glyphs are placed by an absolute text matrix rather than relying on the PDF
/// font's advances — our advances carry justification + Kashida adjustments the
/// font's own metrics do not.
fn build_content(page: &PageBox, font_objs: &[(String, FontObj)]) -> Vec<u8> {
    let page_h = page.size.height;
    let mut content = Content::new();
    content.begin_text();

    let content_x = page.margins.left;
    let content_y = page.margins.top;
    for para in &page.paragraphs {
        let para_x = content_x + para.origin.x;
        let para_y = content_y + para.origin.y;
        for line in &para.lines {
            let line_x = para_x + line.origin.x;
            /* Layout-space baseline — distance down from the page top. */
            let baseline = para_y + line.origin.y + line.baseline;
            let mut pen = 0.0_f32;
            for run in &line.runs {
                let Some((_, fo)) = font_objs.iter().find(|(id, _)| id == &run.font) else {
                    continue;
                };
                let [r, g, b, _] = run.attrs.color;
                content.set_font(Name(fo.resource.as_bytes()), run.attrs.px_size);
                content.set_fill_rgb(
                    f32::from(r) / 255.0,
                    f32::from(g) / 255.0,
                    f32::from(b) / 255.0,
                );
                for glyph in &run.glyphs {
                    let gx = line_x + pen + glyph.x_offset;
                    /* Invert the y axis: PDF origin is bottom-left. */
                    let gy = page_h - (baseline - glyph.y_offset);
                    content.set_text_matrix([1.0, 0.0, 0.0, 1.0, gx, gy]);
                    let id = glyph.id;
                    content.show(Str(&[(id >> 8) as u8, (id & 0xff) as u8]));
                    pen += glyph.x_advance;
                }
            }
        }
    }

    content.end_text();
    content.finish().to_vec()
}

/// Embed one face as a `Type0` / `CIDFontType2` font with the raw TrueType
/// bytes in a `FontFile2` stream.
fn embed_font(pdf: &mut Pdf, id: &str, face: &LoadedFont, fo: &FontObj) {
    let base = Name(id.as_bytes());
    let data = face.data();
    /* Metrics in 1000-unit em space — the PDF font-descriptor convention. */
    let m = face.metrics(1000.0);

    pdf.type0_font(fo.type0)
        .base_font(base)
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(fo.cid);

    {
        let mut cid = pdf.cid_font(fo.cid);
        cid.subtype(CidFontType::Type2)
            .base_font(base)
            .system_info(SystemInfo {
                registry: Str(b"Adobe"),
                ordering: Str(b"Identity"),
                supplement: 0,
            })
            .font_descriptor(fo.descriptor)
            .default_width(1000.0);
        cid.cid_to_gid_map_predefined(Name(b"Identity"));
    }

    pdf.font_descriptor(fo.descriptor)
        .name(base)
        .flags(FontFlags::SYMBOLIC)
        .bbox(Rect::new(-500.0, -500.0, 1500.0, 1500.0))
        .italic_angle(0.0)
        .ascent(m.ascent)
        .descent(-m.descent.abs())
        .cap_height(m.cap_height)
        .stem_v(80.0)
        .font_file2(fo.file);

    pdf.stream(fo.file, data)
        .pair(Name(b"Length1"), data.len() as i32);
}

#[cfg(test)]
mod tests {
    use super::*;
    use layout::{Margins, ParagraphConfig, Size, StyleSpan, layout_paragraph};
    use std::collections::HashMap;
    use std::sync::Arc;
    use text_pipeline::{Alignment, ShapingDirection};

    #[test]
    fn exports_structural_pdf() {
        let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
        let face = LoadedFont::parse("liberation".into(), bytes).expect("parse font");
        let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
        faces.insert("liberation".to_string(), Arc::new(face));
        let stack = FontStack::from_faces(faces, "liberation");

        let spans = [StyleSpan {
            start: 0,
            end: 11,
            px_size: 18.0,
            color: [0, 0, 0, 255],
        }];
        let para = layout_paragraph(ParagraphConfig {
            text: "Hello world",
            fonts: &stack,
            spans: &spans,
            base_direction: ShapingDirection::Ltr,
            max_width: 451.0,
            line_height: 26.0,
            alignment: Alignment::Start,
        });
        let page = PageBox {
            size: Size {
                width: 595.0,
                height: 842.0,
            },
            margins: Margins::uniform(72.0),
            paragraphs: vec![para],
        };

        let mut out = Vec::new();
        export_pdf(&page, &stack, &mut out).expect("export");

        assert!(out.starts_with(b"%PDF-"), "missing PDF header");
        assert!(out.ends_with(b"%%EOF"), "missing EOF marker");
        assert!(
            out.windows(10).any(|w| w == b"/FontFile2"),
            "font not embedded"
        );
    }

    #[test]
    fn empty_page_exports_valid_pdf() {
        let stack = FontStack::from_faces(HashMap::new(), "none");
        let page = PageBox {
            size: Size {
                width: 595.0,
                height: 842.0,
            },
            margins: Margins::uniform(72.0),
            paragraphs: vec![],
        };
        let mut out = Vec::new();
        export_pdf(&page, &stack, &mut out).expect("export empty page");
        assert!(out.starts_with(b"%PDF-"), "missing PDF header");
        assert!(out.ends_with(b"%%EOF"), "missing EOF marker");
    }
}
