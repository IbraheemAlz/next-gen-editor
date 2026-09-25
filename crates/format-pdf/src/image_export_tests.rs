//! Issue #121 — end-to-end image XObject tests: a laid-out page with an
//! inline PNG (with alpha), an inline JPEG and a floating image behind the
//! text, exported under every profile and parsed back structurally.

use super::image::test_images::{jpeg, png as make_png};
use super::*;
use engine::ImageBlob;
use layout::paragraph::InlineObjectInfoKind;
use layout::{
    FloatAnchorRef, FloatBox, FloatWrap, InlineObjectInfo, Margins, ParagraphConfig, Point, Size,
    StyleSpan, layout_paragraph,
};
use std::io::Read;
use std::sync::Arc;
use text_pipeline::{Alignment, ShapingDirection};

const TEXT: &str = "Pic \u{FFFC} and \u{FFFC} end";
/// PNG 4×3 RGBA: pixel (x, y) = (10x, 20y, 99, alpha) with alpha 255 on the
/// first row and 64 elsewhere.
const PNG_W: u32 = 4;
const PNG_H: u32 = 3;

fn png_pixels() -> Vec<u8> {
    let mut px = Vec::new();
    for y in 0..PNG_H {
        for x in 0..PNG_W {
            px.extend_from_slice(&[
                (10 * x) as u8,
                (20 * y) as u8,
                99,
                if y == 0 { 255 } else { 64 },
            ]);
        }
    }
    px
}

fn liberation_stack() -> FontStack {
    let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
    let face = LoadedFont::parse("liberation".into(), bytes).expect("parse font");
    let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
    faces.insert("liberation".to_string(), Arc::new(face));
    FontStack::from_faces(faces, "liberation")
}

fn media() -> HashMap<String, ImageBlob> {
    let mut m = HashMap::new();
    m.insert(
        "rIdPng".to_string(),
        ImageBlob {
            content_type: "image/png".into(),
            data: make_png(PNG_W, PNG_H, png::ColorType::Rgba, &png_pixels()),
        },
    );
    m.insert(
        "rIdJpg".to_string(),
        ImageBlob {
            content_type: "image/jpeg".into(),
            data: jpeg(16, 8, 3),
        },
    );
    m.insert(
        "rIdFloat".to_string(),
        ImageBlob {
            content_type: "image/jpeg".into(),
            data: jpeg(24, 12, 1),
        },
    );
    m
}

fn float(rel: &str, behind: bool, origin: Point, size: Size, z: u32) -> FloatBox {
    FloatBox {
        origin,
        size,
        rel_id: rel.to_string(),
        at: 0,
        anchor: FloatAnchorRef::Body {
            block: 0,
            cell: None,
        },
        z_order: z,
        behind_doc: behind,
        hidden: false,
        frame_origin: Point { x: 0.0, y: 0.0 },
        wrap: FloatWrap::default(),
        text_box: None,
    }
}

/// The fixture page: inline PNG (40×30) + inline JPEG (50×20) in one
/// paragraph, a floating JPEG behind the text at (100, 400) 120×80, and —
/// reusing the PNG's relationship — an in-front float at (300, 500) 60×45.
fn image_page(stack: &FontStack) -> PageBox {
    let png_at = TEXT.find('\u{FFFC}').unwrap() as u32;
    let jpg_at = TEXT.rfind('\u{FFFC}').unwrap() as u32;
    let objects = [
        InlineObjectInfo {
            at: png_at,
            width_px: 40.0,
            height_px: 30.0,
            kind: InlineObjectInfoKind::Image {
                rel_id: "rIdPng".into(),
            },
        },
        InlineObjectInfo {
            at: jpg_at,
            width_px: 50.0,
            height_px: 20.0,
            kind: InlineObjectInfoKind::Image {
                rel_id: "rIdJpg".into(),
            },
        },
    ];
    let span = StyleSpan {
        start: 0,
        end: TEXT.len() as u32,
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
    };
    let mut para = layout_paragraph(ParagraphConfig {
        text: TEXT,
        fonts: stack,
        spans: &[span],
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
        inline_objects: &objects,
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
        footnotes: layout::NoteBand::default(),
        endnotes: layout::NoteBand::default(),
        hf_role: layout::HeaderRole::Default,
        page_number: 1,
        floats: vec![
            float(
                "rIdFloat",
                true,
                Point { x: 100.0, y: 400.0 },
                Size {
                    width: 120.0,
                    height: 80.0,
                },
                1,
            ),
            float(
                "rIdPng",
                false,
                Point { x: 300.0, y: 500.0 },
                Size {
                    width: 60.0,
                    height: 45.0,
                },
                2,
            ),
        ],
    }
}

fn export(profile: PdfProfile) -> (Vec<u8>, PdfExportReport) {
    let stack = liberation_stack();
    let page = image_page(&stack);
    let mut out = Vec::new();
    let report = export_pdf_with_media(
        std::slice::from_ref(&page),
        &stack,
        &[TEXT],
        &media(),
        profile,
        &mut out,
    )
    .expect("export");
    (out, report)
}

/// One parsed indirect object: its dictionary text and raw stream bytes.
struct Obj {
    num: u32,
    dict: String,
    stream: Option<Vec<u8>>,
}

fn find(h: &[u8], n: &[u8], from: usize) -> Option<usize> {
    h.get(from..)?
        .windows(n.len())
        .position(|w| w == n)
        .map(|p| p + from)
}

/// Minimal structural parser for `pdf-writer` output: every `N 0 obj` …
/// `endobj`, with the stream body sliced by its `/Length`.
fn objects(pdf: &[u8]) -> Vec<Obj> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = find(pdf, b" 0 obj\n", i) {
        let line_start = pdf[..p]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |q| q + 1);
        let num: u32 = std::str::from_utf8(&pdf[line_start..p])
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let body = p + b" 0 obj\n".len();
        let end = find(pdf, b"endobj", body).unwrap();
        let (dict, stream) = match find(pdf, b"\nstream\n", body).filter(|&s| s < end) {
            Some(s) => {
                let dict = String::from_utf8_lossy(&pdf[body..s]).into_owned();
                let len: usize = dict_int(&dict, "/Length").unwrap() as usize;
                let data = pdf[s + 8..s + 8 + len].to_vec();
                (dict, Some(data))
            }
            None => (String::from_utf8_lossy(&pdf[body..end]).into_owned(), None),
        };
        out.push(Obj { num, dict, stream });
        i = end;
    }
    out
}

fn dict_int(dict: &str, key: &str) -> Option<i64> {
    let p = dict.find(&format!("{key} "))? + key.len() + 1;
    dict[p..]
        .split(|c: char| !c.is_ascii_digit() && c != '-')
        .next()?
        .parse()
        .ok()
}

fn inflate(z: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(z)
        .read_to_end(&mut out)
        .expect("inflate");
    out
}

fn images(objs: &[Obj]) -> Vec<&Obj> {
    objs.iter()
        .filter(|o| o.dict.contains("/Subtype /Image"))
        .collect()
}

/// The six `cm` operands immediately before `/<name> Do`, every
/// occurrence in stream order.
fn placements(content: &str, name: &str) -> Vec<[f32; 6]> {
    let needle = format!("cm\n/{name} Do");
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = content[from..].find(&needle).map(|q| q + from) {
        let line_start = content[..p].rfind('\n').map_or(0, |q| q + 1);
        let nums: Vec<f32> = content[line_start..p]
            .split_whitespace()
            .map(|t| t.parse().unwrap())
            .collect();
        out.push(nums.try_into().expect("six cm operands"));
        from = p + needle.len();
    }
    out
}

fn page_content(objs: &[Obj]) -> String {
    /* The page's content stream is the only non-image FlateDecode stream
    whose inflated bytes open a text object. */
    objs.iter()
        .filter(|o| {
            o.stream.is_some() && !o.dict.contains("/Subtype") && !o.dict.contains("/Length1")
        })
        .map(|o| String::from_utf8_lossy(&inflate(o.stream.as_ref().unwrap())).into_owned())
        .find(|s| s.contains("BT"))
        .expect("page content stream")
}

/// Absolute layout-space (x, baseline) of the inline object whose glyph
/// carries `rel`, walked exactly as the scene / PDF emitters do.
fn inline_rect(page: &PageBox, rel: &str) -> (f32, f32, f32, f32) {
    let LayoutBlock::Paragraph(para) = &page.blocks[0] else {
        unreachable!()
    };
    for line in &para.lines {
        let mut pen = 0.0;
        for run in &line.runs {
            for g in &run.glyphs {
                if g.inline_image_rel_id.as_deref() == Some(rel) {
                    let x = page.margins.left + para.origin.x + line.origin.x + pen;
                    let baseline = page.margins.top + para.origin.y + line.origin.y + line.baseline;
                    return (x, baseline, g.x_advance, g.inline_object_height);
                }
                pen += g.x_advance;
            }
        }
    }
    panic!("no inline object {rel}");
}

#[test]
fn three_media_parts_become_three_deduplicated_xobjects() {
    let (pdf, report) = export(PdfProfile::Plain);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(
        report.images_embedded, 3,
        "PNG reused by the in-front float is written once"
    );
    let objs = objects(&pdf);
    let imgs = images(&objs);
    /* 3 images + the PNG's soft mask. */
    assert_eq!(imgs.len(), 4);
    assert_eq!(imgs.iter().filter(|o| o.dict.contains("/SMask")).count(), 1);
}

#[test]
fn png_pixels_and_soft_mask_round_trip() {
    let (pdf, _) = export(PdfProfile::Plain);
    let objs = objects(&pdf);
    let png_obj = images(&objs)
        .into_iter()
        .find(|o| o.dict.contains("/SMask"))
        .expect("PNG XObject");
    assert_eq!(dict_int(&png_obj.dict, "/Width"), Some(PNG_W as i64));
    assert_eq!(dict_int(&png_obj.dict, "/Height"), Some(PNG_H as i64));
    assert!(png_obj.dict.contains("/FlateDecode"));
    assert!(png_obj.dict.contains("/DeviceRGB"));
    assert_eq!(dict_int(&png_obj.dict, "/BitsPerComponent"), Some(8));
    let rgb = inflate(png_obj.stream.as_ref().unwrap());
    assert_eq!(rgb.len(), (PNG_W * PNG_H * 3) as usize);
    /* Pixel probe: (x=3, y=2) = (30, 40, 99). */
    let at = ((2 * PNG_W + 3) * 3) as usize;
    assert_eq!(&rgb[at..at + 3], &[30, 40, 99]);

    let smask_num = dict_int(&png_obj.dict, "/SMask").unwrap() as u32;
    let mask = objs
        .iter()
        .find(|o| o.num == smask_num)
        .expect("SMask object");
    assert!(mask.dict.contains("/DeviceGray"));
    assert!(!mask.dict.contains("/SMask"), "a soft mask carries no mask");
    let alpha = inflate(mask.stream.as_ref().unwrap());
    assert_eq!(alpha.len(), (PNG_W * PNG_H) as usize);
    assert_eq!(&alpha[..4], &[255; 4]);
    assert!(alpha[4..].iter().all(|&a| a == 64));
}

#[test]
fn jpegs_pass_through_as_dct_with_sof_dimensions() {
    let (pdf, _) = export(PdfProfile::Plain);
    let objs = objects(&pdf);
    let dct: Vec<&Obj> = images(&objs)
        .into_iter()
        .filter(|o| o.dict.contains("/DCTDecode"))
        .collect();
    assert_eq!(dct.len(), 2);
    let rgb = dct.iter().find(|o| o.dict.contains("/DeviceRGB")).unwrap();
    assert_eq!(
        (
            dict_int(&rgb.dict, "/Width"),
            dict_int(&rgb.dict, "/Height")
        ),
        (Some(16), Some(8))
    );
    assert_eq!(rgb.stream.as_deref(), Some(jpeg(16, 8, 3).as_slice()));
    let gray = dct.iter().find(|o| o.dict.contains("/DeviceGray")).unwrap();
    assert_eq!(
        (
            dict_int(&gray.dict, "/Width"),
            dict_int(&gray.dict, "/Height")
        ),
        (Some(24), Some(12))
    );
    assert_eq!(gray.stream.as_deref(), Some(jpeg(24, 12, 1).as_slice()));
}

#[test]
fn images_land_at_layout_rects_with_float_z_order() {
    let stack = liberation_stack();
    let page = image_page(&stack);
    let (pdf, _) = export(PdfProfile::Plain);
    let objs = objects(&pdf);
    let content = page_content(&objs);
    /* Resource names follow first-seen paint-walk order: inline PNG,
    inline JPEG, then the floating JPEG. */
    let close = |a: [f32; 6], b: [f32; 6]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.01);

    let (x, baseline, w, h) = inline_rect(&page, "rIdPng");
    assert_eq!((w, h), (40.0, 30.0));
    let png = placements(&content, "Im0");
    assert_eq!(png.len(), 2, "inline + in-front float share Im0");
    assert!(
        close(png[0], [w, 0.0, 0.0, h, x, 842.0 - baseline]),
        "{:?}",
        png[0]
    );
    /* In-front float: bottom-left = (300, 842 - (500 + 45)). */
    assert!(
        close(png[1], [60.0, 0.0, 0.0, 45.0, 300.0, 297.0]),
        "{:?}",
        png[1]
    );

    let (x, baseline, w, h) = inline_rect(&page, "rIdJpg");
    let jpg = placements(&content, "Im1");
    assert_eq!(jpg.len(), 1);
    assert!(
        close(jpg[0], [w, 0.0, 0.0, h, x, 842.0 - baseline]),
        "{:?}",
        jpg[0]
    );

    let fl = placements(&content, "Im2");
    assert_eq!(fl.len(), 1);
    assert!(
        close(fl[0], [120.0, 0.0, 0.0, 80.0, 100.0, 362.0]),
        "{:?}",
        fl[0]
    );

    /* z-order: behindDoc before any text, in-front after all text. */
    let first_bt = content.find("BT").unwrap();
    let last_et = content.rfind("ET").unwrap();
    assert!(content.find("/Im2 Do").unwrap() < first_bt);
    assert!(content.rfind("/Im0 Do").unwrap() > last_et);

    /* Every page names exactly the XObjects it uses. */
    let page_obj = objs
        .iter()
        .find(|o| o.dict.contains("/Type /Page\n"))
        .unwrap();
    for name in ["/Im0", "/Im1", "/Im2"] {
        assert!(
            page_obj.dict.contains(name),
            "{name} missing from page resources"
        );
    }
}

#[test]
fn inline_image_glyphs_are_not_shown_as_text() {
    let stack = liberation_stack();
    let page = image_page(&stack);
    let LayoutBlock::Paragraph(para) = &page.blocks[0] else {
        unreachable!()
    };
    let shown_glyphs = para
        .lines
        .iter()
        .flat_map(|l| &l.runs)
        .flat_map(|r| &r.glyphs)
        .filter(|g| g.inline_image_rel_id.is_none() && g.float.is_none())
        .count();
    let (pdf, _) = export(PdfProfile::Plain);
    let content = page_content(&objects(&pdf));
    assert_eq!(content.matches(" Tj").count(), shown_glyphs);
}

#[test]
fn pdfa1b_and_x3_flatten_alpha_and_carry_no_soft_mask() {
    for profile in [PdfProfile::A1b, PdfProfile::X3] {
        let (pdf, report) = export(profile);
        assert_eq!(report.images_embedded, 3);
        assert!(
            find(&pdf, b"/SMask", 0).is_none(),
            "{profile:?} must not carry /SMask"
        );
        let objs = objects(&pdf);
        let imgs = images(&objs);
        assert_eq!(imgs.len(), 3);
        let png_obj = imgs
            .iter()
            .find(|o| o.dict.contains("/FlateDecode"))
            .unwrap();
        let rgb = inflate(png_obj.stream.as_ref().unwrap());
        /* Row 0 is opaque — unchanged; (3, 2) at alpha 64 over white. */
        assert_eq!(&rgb[0..3], &[0, 0, 99]);
        let at = ((2 * PNG_W + 3) * 3) as usize;
        let expect: Vec<u8> = [30u8, 40, 99]
            .iter()
            .map(|&c| image::flatten_on_white(c, 64))
            .collect();
        assert_eq!(&rgb[at..at + 3], expect.as_slice());
        /* PDF/A-1 also bans JPX and interpolation. */
        assert!(find(&pdf, b"/JPXDecode", 0).is_none());
        assert!(find(&pdf, b"/Interpolate", 0).is_none());
    }
}

#[test]
fn pdfa2u_keeps_the_soft_mask() {
    let (pdf, report) = export(PdfProfile::A2u);
    assert_eq!(report.images_embedded, 3);
    let objs = objects(&pdf);
    assert_eq!(images(&objs).len(), 4);
    assert!(find(&pdf, b"/SMask", 0).is_some());
}

#[test]
fn missing_and_unsupported_media_warn_and_paint_nothing() {
    let stack = liberation_stack();
    let page = image_page(&stack);
    let mut m = media();
    m.remove("rIdJpg");
    m.insert(
        "rIdFloat".into(),
        ImageBlob {
            content_type: "image/x-wmf".into(),
            data: vec![0xD7, 0xCD, 0xC6, 0x9A, 0, 0],
        },
    );
    let mut pdf = Vec::new();
    let report = export_pdf_with_media(
        std::slice::from_ref(&page),
        &stack,
        &[TEXT],
        &m,
        PdfProfile::Plain,
        &mut pdf,
    )
    .expect("export still succeeds");
    assert_eq!(report.images_embedded, 1);
    assert_eq!(
        report.warnings,
        vec![
            PdfWarning::ImageSkipped {
                rel_id: "rIdJpg".into(),
                reason: ImageSkipReason::MissingMedia,
            },
            PdfWarning::ImageSkipped {
                rel_id: "rIdFloat".into(),
                reason: ImageSkipReason::UnsupportedFormat { format: "WMF" },
            },
        ]
    );
    let content = page_content(&objects(&pdf));
    assert_eq!(
        content.matches(" Do").count(),
        2,
        "PNG inline + in-front only"
    );
}

#[test]
fn image_free_documents_are_byte_identical() {
    let stack = liberation_stack();
    let mut page = image_page(&stack);
    page.floats.clear();
    /* Plain `export_pdf` (no media) vs. the media-aware path with a media
    map the page never references. Every profile. */
    let plain_page = {
        let span_len = "Hello world".len() as u32;
        let mut p = page.clone();
        let para = layout_paragraph(ParagraphConfig {
            text: "Hello world",
            fonts: &stack,
            spans: &[StyleSpan {
                start: 0,
                end: span_len,
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
            }],
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
        p.blocks = vec![LayoutBlock::Paragraph(para)];
        p
    };
    for profile in [
        PdfProfile::Plain,
        PdfProfile::A1b,
        PdfProfile::A2u,
        PdfProfile::X3,
    ] {
        let mut a = Vec::new();
        export_pdf(
            std::slice::from_ref(&plain_page),
            &stack,
            &["Hello world"],
            profile,
            &mut a,
        )
        .unwrap();
        let mut b = Vec::new();
        let report = export_pdf_with_media(
            std::slice::from_ref(&plain_page),
            &stack,
            &["Hello world"],
            &media(),
            profile,
            &mut b,
        )
        .unwrap();
        assert_eq!(a, b, "{profile:?}");
        assert_eq!(report, PdfExportReport::default());
        assert!(find(&a, b"/XObject", 0).is_none());
    }
}

#[test]
fn shared_image_across_pages_is_embedded_once() {
    let stack = liberation_stack();
    let page = image_page(&stack);
    let pages = vec![page.clone(), page];
    let mut pdf = Vec::new();
    let report =
        export_pdf_with_media(&pages, &stack, &[TEXT], &media(), PdfProfile::A2u, &mut pdf)
            .unwrap();
    assert_eq!(report.images_embedded, 3);
    assert_eq!(images(&objects(&pdf)).len(), 4);
}

/* ---- Issue #189: GIF + WebP reach the PDF like PNG ------------------- */

/// The fixture page's three relationships re-pointed at a GIF (palette +
/// transparent index, 3×2), a lossless WebP with alpha (3×2) and a lossy
/// WebP with an alpha plane (4×4).
#[cfg(all(feature = "gif", feature = "webp"))]
fn gif_webp_media() -> HashMap<String, ImageBlob> {
    use super::image::test_images::{WEBP_LOSSY_ALPHA_4X4_BLUE, gif, webp_lossless_rgba};
    /* Red, green, blue, white; index 2 transparent. */
    let palette = [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255];
    let lossless = [
        255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, //
        10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 64,
    ];
    let mut m = HashMap::new();
    m.insert(
        "rIdPng".to_string(),
        ImageBlob {
            content_type: "image/gif".into(),
            data: gif(3, 2, &palette, 0, 0, 3, 2, &[0, 1, 2, 3, 2, 0], Some(2)),
        },
    );
    m.insert(
        "rIdJpg".to_string(),
        ImageBlob {
            content_type: "image/webp".into(),
            data: webp_lossless_rgba(3, 2, &lossless),
        },
    );
    m.insert(
        "rIdFloat".to_string(),
        ImageBlob {
            content_type: "image/webp".into(),
            data: WEBP_LOSSY_ALPHA_4X4_BLUE.to_vec(),
        },
    );
    m
}

#[cfg(all(feature = "gif", feature = "webp"))]
fn export_gif_webp(profile: PdfProfile) -> (Vec<u8>, PdfExportReport) {
    let stack = liberation_stack();
    let page = image_page(&stack);
    let mut out = Vec::new();
    let report = export_pdf_with_media(
        std::slice::from_ref(&page),
        &stack,
        &[TEXT],
        &gif_webp_media(),
        profile,
        &mut out,
    )
    .expect("export");
    (out, report)
}

/// The `nth` base (non-mask) RGB image XObject of the given size.
#[cfg(all(feature = "gif", feature = "webp"))]
fn base_image(objs: &[Obj], w: i64, h: i64, nth: usize) -> &Obj {
    images(objs)
        .into_iter()
        .filter(|o| o.dict.contains("/DeviceRGB"))
        .filter(|o| {
            dict_int(&o.dict, "/Width") == Some(w) && dict_int(&o.dict, "/Height") == Some(h)
        })
        .nth(nth)
        .expect("image XObject")
}

#[cfg(all(feature = "gif", feature = "webp"))]
fn soft_mask_of(objs: &[Obj], img: &Obj) -> Vec<u8> {
    let num = dict_int(&img.dict, "/SMask").expect("/SMask") as u32;
    let mask = objs.iter().find(|o| o.num == num).expect("mask object");
    assert!(mask.dict.contains("/DeviceGray"));
    inflate(mask.stream.as_ref().unwrap())
}

#[cfg(all(feature = "gif", feature = "webp"))]
#[test]
fn gif_and_webp_embed_as_flate_xobjects_with_soft_masks() {
    let (pdf, report) = export_gif_webp(PdfProfile::Plain);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert_eq!(report.images_embedded, 3);
    let objs = objects(&pdf);
    /* 3 images, each with transparency → 3 soft masks. */
    assert_eq!(images(&objs).len(), 6);
    assert_eq!(
        images(&objs)
            .iter()
            .filter(|o| o.dict.contains("/SMask"))
            .count(),
        3
    );
    for o in images(&objs) {
        assert!(o.dict.contains("/FlateDecode"), "{}", o.dict);
        assert_eq!(dict_int(&o.dict, "/BitsPerComponent"), Some(8));
    }

    /* GIF (first 3×2 RGB image in resource order): opaque red at (0,0),
    white at (0,1); alpha 0 on the transparent index. */
    let gif = base_image(&objs, 3, 2, 0);
    let rgb = inflate(gif.stream.as_ref().unwrap());
    assert_eq!(rgb.len(), 18);
    assert_eq!(&rgb[0..3], &[255, 0, 0]);
    assert_eq!(&rgb[9..12], &[255, 255, 255]);
    assert_eq!(soft_mask_of(&objs, gif), vec![255, 255, 0, 255, 0, 255]);

    /* Lossless WebP: bit-exact samples, alpha 128 / 0 / 64 in the mask. */
    let webp = base_image(&objs, 3, 2, 1);
    let rgb = inflate(webp.stream.as_ref().unwrap());
    assert_eq!(&rgb[9..18], &[10, 20, 30, 40, 50, 60, 70, 80, 90]);
    assert_eq!(soft_mask_of(&objs, webp), vec![255, 128, 0, 255, 255, 64]);

    /* Lossy WebP 4×4: top half opaque blue, bottom half clear. */
    let lossy = base_image(&objs, 4, 4, 0);
    let rgb = inflate(lossy.stream.as_ref().unwrap());
    assert_eq!(rgb.len(), 48);
    assert!(rgb[2] > 150 && rgb[0] < 80, "{:?}", &rgb[0..3]);
    let alpha = soft_mask_of(&objs, lossy);
    assert!(alpha[..8].iter().all(|&a| a == 255));
    assert!(alpha[8..].iter().all(|&a| a == 0));

    /* Every placement is painted (inline GIF + in-front GIF float, inline
    WebP, floating WebP). */
    let content = page_content(&objs);
    assert_eq!(content.matches(" Do").count(), 4);
}

#[cfg(all(feature = "gif", feature = "webp"))]
#[test]
fn gif_and_webp_flatten_on_white_under_pdfa1b_and_x3() {
    for profile in [PdfProfile::A1b, PdfProfile::X3] {
        let (pdf, report) = export_gif_webp(profile);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.images_embedded, 3);
        assert!(find(&pdf, b"/SMask", 0).is_none(), "{profile:?}");
        /* The tools/pdf-validate structural markers these profiles gate on. */
        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(find(&pdf, b"/OutputIntent", 0).is_some());
        assert!(find(&pdf, b"/DestOutputProfile", 0).is_some());
        assert!(find(&pdf, b"/JPXDecode", 0).is_none());
        assert!(pdf.trim_ascii_end().ends_with(b"%%EOF"));
        let objs = objects(&pdf);
        assert_eq!(images(&objs).len(), 3);

        let gif = inflate(base_image(&objs, 3, 2, 0).stream.as_ref().unwrap());
        assert_eq!(
            gif,
            vec![
                255, 0, 0, 0, 255, 0, 255, 255, 255, //
                255, 255, 255, 255, 255, 255, 255, 0, 0,
            ]
        );
        let webp = inflate(base_image(&objs, 3, 2, 1).stream.as_ref().unwrap());
        assert_eq!(&webp[6..9], &[255, 255, 255], "alpha 0 → white");
        let expect: Vec<u8> = [70u8, 80, 90]
            .iter()
            .map(|&c| image::flatten_on_white(c, 64))
            .collect();
        assert_eq!(&webp[15..18], expect.as_slice());
        let lossy = inflate(base_image(&objs, 4, 4, 0).stream.as_ref().unwrap());
        assert!(lossy[24..].iter().all(|&v| v == 255), "clear rows → white");
    }
}

#[cfg(all(feature = "gif", feature = "webp"))]
#[test]
fn corrupt_gif_and_webp_warn_and_paint_nothing() {
    let stack = liberation_stack();
    let page = image_page(&stack);
    let mut m = gif_webp_media();
    let gif = m["rIdPng"].data.clone();
    m.get_mut("rIdPng").unwrap().data = gif[..gif.len() / 2].to_vec();
    m.get_mut("rIdFloat").unwrap().data = b"RIFF\0\0\0\0WEBPjunk".to_vec();
    let mut pdf = Vec::new();
    let report = export_pdf_with_media(
        std::slice::from_ref(&page),
        &stack,
        &[TEXT],
        &m,
        PdfProfile::A2u,
        &mut pdf,
    )
    .expect("export still succeeds");
    assert_eq!(report.images_embedded, 1, "only the lossless WebP");
    let skipped: Vec<&str> = report
        .warnings
        .iter()
        .map(|w| match w {
            PdfWarning::ImageSkipped {
                rel_id,
                reason: ImageSkipReason::Malformed { .. },
            } => rel_id.as_str(),
            other => panic!("unexpected warning {other:?}"),
        })
        .collect();
    assert_eq!(skipped, vec!["rIdPng", "rIdFloat"]);
}
