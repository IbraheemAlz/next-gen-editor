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
//!
//! # PDF/A-1b
//!
//! [`PdfProfile::A1b`] additionally emits what ISO 19005-1 level B requires for
//! archival conformance: a PDF 1.4 header, an `OutputIntent` referencing an
//! embedded sRGB ICC profile, an XMP metadata packet carrying the `pdfaid`
//! conformance keys, and a document `/ID`. The full font embedding the plain
//! path already does satisfies the font clauses. Strict /A-1**a** tagging
//! (structure tree, marked content) is out of scope — level B is a
//! visual-reproduction guarantee only.
//!
//! The ICC profile is **not** a vendored binary blob: `build.rs` synthesizes a
//! minimal valid sRGB v2 profile from plain Rust at build time and `lib.rs`
//! pulls it in from `OUT_DIR` with `include_bytes!`. Nothing binary lands in
//! the source tree, and the build stays hermetic — no network fetch.
//!
//! # Stream compression & text extraction
//!
//! Content streams and the embedded `FontFile2` programs are zlib-compressed
//! and tagged `/Filter /FlateDecode`; a `FontFile2`'s `/Length1` still records
//! the *uncompressed* program length, as the spec requires. Each `Type0` font
//! also carries a `/ToUnicode` CMap mapping the shaped glyph ids back to their
//! source characters, so a viewer can extract and copy the text — ligatures
//! and Kashida-justified Arabic included.

use flate2::Compression;
use flate2::write::ZlibEncoder;
use layout::{PageBox, VisualRun};
use pdf_writer::types::{CidFontType, FontFlags, OutputIntentSubtype, SystemInfo, UnicodeCmap};
use pdf_writer::{Content, Filter, Name, Pdf, Rect, Ref, Str, TextStr};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use text_pipeline::{FontStack, LoadedFont};

/// The synthesized sRGB ICC profile the PDF/A-1b output intent embeds. Built by
/// `build.rs` — see this module's docs for why it is generated, not vendored.
const SRGB_ICC: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/srgb-v2-micro.icc"));

/// XMP metadata packet for a PDF/A-1b document. The `pdfaid` keys are the
/// conformance claim veraPDF checks; no Info dictionary is written, so there is
/// nothing this must be kept consistent with (ISO 19005-1 §6.7.3).
const XMP_PACKET: &str = concat!(
    "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n",
    "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n",
    " <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n",
    "  <rdf:Description rdf:about=\"\"\n",
    "    xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\"\n",
    "    xmlns:dc=\"http://purl.org/dc/elements/1.1/\"\n",
    "    xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\">\n",
    "   <pdfaid:part>1</pdfaid:part>\n",
    "   <pdfaid:conformance>B</pdfaid:conformance>\n",
    "   <dc:format>application/pdf</dc:format>\n",
    "   <xmp:CreatorTool>next-gen-editor</xmp:CreatorTool>\n",
    "  </rdf:Description>\n",
    " </rdf:RDF>\n",
    "</x:xmpmeta>\n",
    "<?xpacket end=\"r\"?>",
);

/// Conformance target for [`export_pdf`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfProfile {
    /// A plain PDF — no archival conformance structures (the Phase 3 output).
    Plain,
    /// PDF/A-1b (ISO 19005-1 level B): adds a PDF 1.4 header, an sRGB
    /// `OutputIntent`, an XMP metadata packet and a document `/ID` on top of
    /// the full font embedding the plain path already produces.
    A1b,
}

/// The five indirect objects + resource name an embedded font occupies.
struct FontObj {
    type0: Ref,
    cid: Ref,
    descriptor: Ref,
    file: Ref,
    to_unicode: Ref,
    resource: String,
}

/// Export `page` to a single-page PDF, appending the bytes to `out`.
///
/// `fonts` must contain every face referenced by the page's runs; each is
/// embedded in full. `para_texts[i]` is the source text of `page.paragraphs[i]`
/// — the `/ToUnicode` CMap resolves each glyph's cluster against it; a short or
/// empty slice just yields a sparser CMap. `profile` selects plain output or
/// PDF/A-1b conformance.
pub fn export_pdf(
    page: &PageBox,
    fonts: &FontStack,
    para_texts: &[&str],
    profile: PdfProfile,
    out: &mut Vec<u8>,
) -> Result<(), String> {
    let page_w = page.size.width;
    let page_h = page.size.height;
    let pdfa = profile == PdfProfile::A1b;

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
    /* PDF/A-1 is defined against PDF 1.4 — declare it so the header agrees. */
    if pdfa {
        pdf.set_version(1, 4);
    }

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
                    to_unicode: alloc(),
                    resource: format!("F{i}"),
                },
            )
        })
        .collect();
    /* PDF/A-1b adds two indirect objects — the ICC profile and the XMP packet.
    Allocated last so a plain export leaves the id space byte-identical. */
    let icc_id = if pdfa { Some(alloc()) } else { None };
    let metadata_id = if pdfa { Some(alloc()) } else { None };

    /* Content stream — must be a live binding while its `Stream` writer runs. */
    let content = build_content(page, &font_objs);
    if pdfa {
        /* PDF/A requires a trailer `/ID`. Derive it from the content so the
        same document exports to the same identifier every time. */
        let id = document_id(&content);
        pdf.set_file_id((id.to_vec(), id.to_vec()));
    }
    /* Compress the drawing commands; `document_id` above stays keyed on the
    uncompressed bytes so the PDF/A `/ID` is a stable, meaningful digest. */
    let content_z = deflate(&content);
    pdf.stream(content_id, &content_z)
        .filter(Filter::FlateDecode);

    /* Catalog — plus, for PDF/A-1b, the `/Metadata` and `/OutputIntents`. */
    {
        let mut catalog = pdf.catalog(catalog_id);
        catalog.pages(pages_id);
        if pdfa {
            catalog.metadata(metadata_id.expect("metadata id allocated for A1b"));
            let mut intents = catalog.output_intents();
            let mut intent = intents.push();
            intent
                .subtype(OutputIntentSubtype::PDFA)
                .output_condition_identifier(TextStr("sRGB IEC61966-2.1"))
                .output_condition(TextStr("sRGB IEC61966-2.1"))
                .registry_name(TextStr("http://www.color.org"))
                .info(TextStr("sRGB IEC61966-2.1"))
                .dest_output_profile(icc_id.expect("icc id allocated for A1b"));
        }
    }

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

    /* glyph-id → Unicode per font, harvested from the box-tree clusters. */
    let to_unicode = collect_to_unicode(page, para_texts);
    for (id, fo) in &font_objs {
        let face = fonts
            .face(id)
            .ok_or_else(|| format!("export_pdf: font `{id}` not in the stack"))?;
        embed_font(&mut pdf, id, face, fo);
        let cmap_z = deflate(&build_unicode_cmap(to_unicode.get(id)));
        pdf.stream(fo.to_unicode, &cmap_z)
            .filter(Filter::FlateDecode);
    }

    /* PDF/A-1b output intent objects: the embedded sRGB profile + XMP packet. */
    if pdfa {
        pdf.icc_profile(icc_id.expect("icc id allocated for A1b"), SRGB_ICC)
            .n(3);
        pdf.metadata(
            metadata_id.expect("metadata id allocated for A1b"),
            XMP_PACKET.as_bytes(),
        );
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

/// Embed one face as a `Type0` / `CIDFontType2` font with the TrueType bytes in
/// a FlateDecode-compressed `FontFile2` stream. The `/ToUnicode` reference is
/// wired into the `Type0` dict here; the CMap stream itself is a separate
/// object written by the caller.
fn embed_font(pdf: &mut Pdf, id: &str, face: &LoadedFont, fo: &FontObj) {
    let base = Name(id.as_bytes());
    let data = face.data();
    /* Metrics in 1000-unit em space — the PDF font-descriptor convention. */
    let m = face.metrics(1000.0);

    pdf.type0_font(fo.type0)
        .base_font(base)
        .encoding_predefined(Name(b"Identity-H"))
        .to_unicode(fo.to_unicode)
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

    /* Compress the font program; `/Length1` keeps the *uncompressed* length. */
    let file_z = deflate(data);
    pdf.stream(fo.file, &file_z)
        .filter(Filter::FlateDecode)
        .pair(Name(b"Length1"), data.len() as i32);
}

/// zlib-compress `data` for a PDF `/FlateDecode` stream. Deterministic for a
/// fixed input — the PDF/A `/ID` and the byte-stability test rely on that.
fn deflate(data: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data)
        .expect("zlib encode into a Vec cannot fail");
    enc.finish().expect("zlib finish into a Vec cannot fail")
}

/// Harvest a glyph-id → Unicode map, per font, from the laid-out page.
///
/// `para_texts[i]` is the source text of `page.paragraphs[i]`. Every glyph
/// carries a `cluster` byte offset into its run's `source_range`; a run's
/// distinct clusters partition its source bytes into segments, and a glyph
/// decodes to the characters of its segment. A ligature glyph therefore maps to
/// *all* the characters it consumed — the property that lets shaped Arabic copy
/// back as real text.
fn collect_to_unicode(
    page: &PageBox,
    para_texts: &[&str],
) -> HashMap<String, BTreeMap<u16, Vec<char>>> {
    let mut out: HashMap<String, BTreeMap<u16, Vec<char>>> = HashMap::new();
    for (pi, para) in page.paragraphs.iter().enumerate() {
        let Some(&text) = para_texts.get(pi) else {
            continue;
        };
        for line in &para.lines {
            for run in &line.runs {
                let map = out.entry(run.font.clone()).or_default();
                add_run_mappings(run, text, map);
            }
        }
    }
    out
}

/// Fold one run's glyph→Unicode mappings into `map`. The first non-empty decode
/// seen for a glyph id wins — a glyph always renders the same characters, so
/// later repeats agree.
fn add_run_mappings(run: &VisualRun, text: &str, map: &mut BTreeMap<u16, Vec<char>>) {
    let base = run.source_range.start as usize;
    let run_end = (run.source_range.end as usize).min(text.len());

    /* Distinct absolute cluster offsets, sorted — they partition the run's
    source bytes; a glyph's segment runs up to the next-larger cluster. */
    let mut bounds: Vec<usize> = run
        .glyphs
        .iter()
        .filter(|g| !g.synthetic)
        .map(|g| base + g.cluster as usize)
        .collect();
    bounds.sort_unstable();
    bounds.dedup();

    for g in &run.glyphs {
        /* A synthetic Kashida tatweel is justification ink, not content —
        leaving it unmapped lets a viewer drop it on copy. */
        if g.synthetic {
            continue;
        }
        let start = base + g.cluster as usize;
        let end = bounds
            .iter()
            .copied()
            .find(|&b| b > start)
            .unwrap_or(run_end)
            .min(run_end);
        if start >= end || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            continue;
        }
        let chars: Vec<char> = text[start..end].chars().collect();
        if !chars.is_empty() {
            map.entry(g.id).or_insert(chars);
        }
    }
}

/// Serialize a `/ToUnicode` CMap for one font. `mappings` is `None` for a font
/// with no harvested text — the CMap is still structurally valid, just empty.
fn build_unicode_cmap(mappings: Option<&BTreeMap<u16, Vec<char>>>) -> Vec<u8> {
    let mut cmap = UnicodeCmap::<u16>::new(
        Name(b"Adobe-Identity-UCS"),
        SystemInfo {
            registry: Str(b"Adobe"),
            ordering: Str(b"UCS"),
            supplement: 0,
        },
    );
    if let Some(map) = mappings {
        for (&gid, chars) in map {
            cmap.pair_with_multiple(gid, chars.iter().copied());
        }
    }
    cmap.finish().into_vec()
}

/// A 16-byte document identifier derived from the content stream. Deterministic
/// for identical input, so re-exporting the same document yields the same
/// PDF/A `/ID`.
fn document_id(content: &[u8]) -> [u8; 16] {
    let a = fnv1a64(content);
    let b = fnv1a64(&a.to_be_bytes());
    let mut id = [0u8; 16];
    id[..8].copy_from_slice(&a.to_be_bytes());
    id[8..].copy_from_slice(&b.to_be_bytes());
    id
}

/// FNV-1a 64-bit hash — small, dependency-free, sufficient for a document id.
fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in data {
        h ^= u64::from(byte);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use layout::{Margins, ParagraphConfig, Size, StyleSpan, layout_paragraph};
    use std::collections::HashSet;
    use std::sync::Arc;
    use text_pipeline::{Alignment, ShapingDirection};

    /// Lay out "Hello world" on an A4 page with the bundled Liberation face.
    fn hello_page(stack: &FontStack) -> PageBox {
        let spans = [StyleSpan {
            start: 0,
            end: 11,
            px_size: 18.0,
            color: [0, 0, 0, 255],
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            bg_color: None,
            font_family: None,
        }];
        let para = layout_paragraph(ParagraphConfig {
            text: "Hello world",
            fonts: stack,
            spans: &spans,
            base_direction: ShapingDirection::Ltr,
            max_width: 451.0,
            line_height: 26.0,
            alignment: Alignment::Start,
            indent_start_px: 0.0,
            indent_end_px: 0.0,
            first_line_indent_px: 0.0,
            hanging_indent_px: 0.0,
        });
        PageBox {
            size: Size {
                width: 595.0,
                height: 842.0,
            },
            margins: Margins::uniform(72.0),
            paragraphs: vec![para],
        }
    }

    fn liberation_stack() -> FontStack {
        let bytes = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec();
        let face = LoadedFont::parse("liberation".into(), bytes).expect("parse font");
        let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
        faces.insert("liberation".to_string(), Arc::new(face));
        FontStack::from_faces(faces, "liberation")
    }

    #[test]
    fn exports_structural_pdf() {
        let stack = liberation_stack();
        let page = hello_page(&stack);

        let mut out = Vec::new();
        export_pdf(&page, &stack, &["Hello world"], PdfProfile::Plain, &mut out).expect("export");

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
        export_pdf(&page, &stack, &[], PdfProfile::Plain, &mut out).expect("export empty page");
        assert!(out.starts_with(b"%PDF-"), "missing PDF header");
        assert!(out.ends_with(b"%%EOF"), "missing EOF marker");
    }

    #[test]
    fn exports_pdfa1b_compliance_markers() {
        let stack = liberation_stack();
        let page = hello_page(&stack);

        let mut out = Vec::new();
        export_pdf(&page, &stack, &["Hello world"], PdfProfile::A1b, &mut out).expect("export A1b");

        let has = |needle: &[u8]| out.windows(needle.len()).any(|w| w == needle);
        assert!(out.starts_with(b"%PDF-1.4"), "PDF/A-1 must declare PDF 1.4");
        assert!(has(b"/OutputIntent"), "missing output intent");
        assert!(has(b"GTS_PDFA1"), "missing PDF/A output intent subtype");
        assert!(has(b"/DestOutputProfile"), "missing embedded ICC reference");
        assert!(has(b"acsp"), "ICC profile signature absent from stream");
        assert!(has(b"/Metadata"), "missing XMP metadata stream");
        assert!(has(b"pdfaid:part>1"), "missing pdfaid:part");
        assert!(has(b"pdfaid:conformance>B"), "missing pdfaid:conformance");
        assert!(has(b"/ID"), "missing trailer document id");
        assert!(has(b"/FontFile2"), "font not embedded");
        assert!(out.ends_with(b"%%EOF"), "missing EOF marker");
    }

    #[test]
    fn document_id_is_deterministic() {
        let stack = liberation_stack();
        let page = hello_page(&stack);
        let mut a = Vec::new();
        let mut b = Vec::new();
        export_pdf(&page, &stack, &["Hello world"], PdfProfile::A1b, &mut a).expect("export a");
        export_pdf(&page, &stack, &["Hello world"], PdfProfile::A1b, &mut b).expect("export b");
        assert_eq!(a, b, "A1b export must be byte-stable for identical input");
    }

    #[test]
    fn deflate_round_trips() {
        use flate2::read::ZlibDecoder;
        use std::io::Read;
        let data: &[u8] = b"PDF content PDF content PDF content PDF content";
        let z = deflate(data);
        let mut back = Vec::new();
        ZlibDecoder::new(&z[..])
            .read_to_end(&mut back)
            .expect("inflate");
        assert_eq!(back, data, "zlib round-trip must be lossless");
    }

    #[test]
    fn compressed_pdf_declares_filters_and_tounicode() {
        let stack = liberation_stack();
        let page = hello_page(&stack);
        let mut out = Vec::new();
        export_pdf(&page, &stack, &["Hello world"], PdfProfile::Plain, &mut out).expect("export");

        let has = |n: &[u8]| out.windows(n.len()).any(|w| w == n);
        assert!(has(b"/FlateDecode"), "streams must declare FlateDecode");
        assert!(
            has(b"/ToUnicode"),
            "Type0 font must reference a ToUnicode CMap"
        );
        /* FontFile2 keeps the uncompressed program length in Length1. */
        assert!(has(b"/Length1"), "FontFile2 missing Length1");
    }

    #[test]
    fn to_unicode_map_covers_source_chars() {
        let stack = liberation_stack();
        let page = hello_page(&stack);
        let map = collect_to_unicode(&page, &["Hello world"]);
        let liberation = map.get("liberation").expect("liberation font mapped");
        let covered: HashSet<char> = liberation.values().flatten().copied().collect();
        for ch in "Helo wrd".chars() {
            assert!(covered.contains(&ch), "ToUnicode map missing {ch:?}");
        }
    }
}
