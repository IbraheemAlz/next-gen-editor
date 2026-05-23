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
use layout::{LayoutBlock, PageBox, ParagraphBox, TableBox, VisualRun};
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

/// Export `pages` to a PDF, appending the bytes to `out`.
///
/// Phase 6 — every `PageBox` in `pages` becomes one PDF page; each has its
/// own MediaBox sized from `page.size`, and a dedicated content stream.
/// `fonts` must contain every face referenced by any page's runs (full
/// embedding). `para_texts[i]` is the source text of the paragraph whose
/// `source_paragraph_id == i`; PDF resolves each laid-out paragraph's
/// glyph clusters against that table, so a paragraph split across pages
/// (head + tail) gets the same `/ToUnicode` mapping on both halves
/// (their `source_paragraph_id` is identical). `profile` selects plain
/// output or PDF/A-1b conformance.
pub fn export_pdf(
    pages: &[PageBox],
    fonts: &FontStack,
    para_texts: &[&str],
    profile: PdfProfile,
    out: &mut Vec<u8>,
) -> Result<(), String> {
    let pdfa = profile == PdfProfile::A1b;

    /* Distinct fonts referenced by every page, in first-seen order. */
    let mut used: Vec<&str> = Vec::new();
    for page in pages {
        for_each_paragraph(&page.blocks, &mut |para| {
            for line in &para.lines {
                for run in &line.runs {
                    if !used.contains(&run.font.as_str()) {
                        used.push(run.font.as_str());
                    }
                }
            }
        });
    }

    let mut pdf = Pdf::new();
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
    /* One page object + one content stream per `PageBox`. */
    let page_refs: Vec<(Ref, Ref)> = pages.iter().map(|_| (alloc(), alloc())).collect();
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
    let icc_id = if pdfa { Some(alloc()) } else { None };
    let metadata_id = if pdfa { Some(alloc()) } else { None };

    /* Build every page's content stream first — the digest in PDF/A `/ID`
    is keyed on the concatenated uncompressed content so split exports
    stay stable. */
    let contents: Vec<Vec<u8>> = pages
        .iter()
        .map(|page| build_content(page, &font_objs))
        .collect();
    if pdfa {
        let mut hash_in: Vec<u8> = Vec::new();
        for c in &contents {
            hash_in.extend_from_slice(c);
        }
        let id = document_id(&hash_in);
        pdf.set_file_id((id.to_vec(), id.to_vec()));
    }
    for ((_, content_id), content) in page_refs.iter().zip(contents.iter()) {
        let content_z = deflate(content);
        pdf.stream(*content_id, &content_z)
            .filter(Filter::FlateDecode);
    }

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

    /* `/Pages` references every page object; `/MediaBox` here is the
    document default — individual pages override per-`PageBox` size. */
    let default_size = pages
        .first()
        .map(|p| Rect::new(0.0, 0.0, p.size.width, p.size.height))
        .unwrap_or(Rect::new(0.0, 0.0, 595.0, 842.0));
    pdf.pages(pages_id)
        .kids(page_refs.iter().map(|(p, _)| *p))
        .count(page_refs.len() as i32)
        .media_box(default_size);
    for ((page_id, content_id), page) in page_refs.iter().zip(pages.iter()) {
        let media = Rect::new(0.0, 0.0, page.size.width, page.size.height);
        let mut p = pdf.page(*page_id);
        p.parent(pages_id);
        p.media_box(media);
        p.contents(*content_id);
        let mut resources = p.resources();
        let mut font_dict = resources.fonts();
        for (_, fo) in &font_objs {
            font_dict.pair(Name(fo.resource.as_bytes()), fo.type0);
        }
    }

    /* glyph-id → Unicode per font, harvested across every page. Split
    paragraphs contribute clusters from both halves into the same source
    text — the union is what the CMap actually wants. */
    let to_unicode = collect_to_unicode_pages(pages, para_texts);
    for (id, fo) in &font_objs {
        let face = fonts
            .face(id)
            .ok_or_else(|| format!("export_pdf: font `{id}` not in the stack"))?;
        embed_font(&mut pdf, id, face, fo);
        let cmap_z = deflate(&build_unicode_cmap(to_unicode.get(id)));
        pdf.stream(fo.to_unicode, &cmap_z)
            .filter(Filter::FlateDecode);
    }

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
///
/// Three passes per page so the text state machine and the graphics state
/// machine don't collide:
///
/// 1. **Shading.** Cell `<w:shd>` fills as `re` / `f` outside any text block.
/// 2. **Text.** Single `BT`/`ET` envelope; every paragraph (top-level + cell)
///    emits its glyphs via an absolute text matrix.
/// 3. **Borders.** Cell + table-outer strokes as `re` / `S` paths.
///
/// The order mirrors `crates/render/src/scene.rs::paint_table` so PDF and
/// Canvas2D agree on layering: shading sits behind content, borders sit on
/// top.
fn build_content(page: &PageBox, font_objs: &[(String, FontObj)]) -> Vec<u8> {
    let page_h = page.size.height;
    let content_x = page.margins.left;
    let content_y = page.margins.top;
    let mut content = Content::new();

    /* Pass 1 — cell shading. */
    for block in &page.blocks {
        if let LayoutBlock::Table(t) = block {
            emit_table_shading(&mut content, page_h, content_x, content_y, t);
        }
    }

    /* Pass 2 — every paragraph's glyphs (top-level + cell content),
    in document order. */
    content.begin_text();
    for block in &page.blocks {
        match block {
            LayoutBlock::Paragraph(p) => {
                emit_paragraph_text(&mut content, page_h, content_x, content_y, p, font_objs);
            }
            LayoutBlock::Table(t) => {
                emit_table_text(&mut content, page_h, content_x, content_y, t, font_objs);
            }
        }
    }
    content.end_text();

    /* Pass 3 — cell + table-outer borders. */
    for block in &page.blocks {
        if let LayoutBlock::Table(t) = block {
            emit_table_borders(&mut content, page_h, content_x, content_y, t);
        }
    }

    content.finish().to_vec()
}

/// Emit text for one `ParagraphBox`. `origin_x` / `origin_y` are the
/// containing block's origin in absolute layout-px (top-left); the
/// paragraph's own origin is added on top. PDF y is bottom-up — every
/// glyph's y inverts via `page_h - layout_y`.
fn emit_paragraph_text(
    content: &mut Content,
    page_h: f32,
    origin_x: f32,
    origin_y: f32,
    para: &ParagraphBox,
    font_objs: &[(String, FontObj)],
) {
    let para_x = origin_x + para.origin.x;
    let para_y = origin_y + para.origin.y;
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

/// Pass 1 — cell `<w:shd>` fills. Continue cells are skipped (the
/// matching Restart cell visually owns the merged region).
fn emit_table_shading(
    content: &mut Content,
    page_h: f32,
    origin_x: f32,
    origin_y: f32,
    t: &TableBox,
) {
    let tx = origin_x + t.origin.x;
    let ty = origin_y + t.origin.y;
    for row in &t.rows {
        let row_x = tx + row.origin.x;
        let row_y = ty + row.origin.y;
        for cell in &row.cells {
            if matches!(cell.v_merge, engine::VMergeRole::Continue) {
                continue;
            }
            let Some([r, g, b, _]) = cell.shading else {
                continue;
            };
            let cell_x = row_x + cell.origin.x;
            let cell_y = row_y + cell.origin.y;
            /* Layout-top-left → PDF-bottom-left: the rect's PDF y is
            the bottom of the cell band in PDF space, i.e.
            `page_h - (cell_y + cell.size.height)`. The rect grows
            upward by `cell.size.height`. */
            let pdf_y = page_h - (cell_y + cell.size.height);
            content.set_fill_rgb(
                f32::from(r) / 255.0,
                f32::from(g) / 255.0,
                f32::from(b) / 255.0,
            );
            content.rect(cell_x, pdf_y, cell.size.width, cell.size.height);
            content.fill_nonzero();
        }
    }
}

/// Pass 2 (text) — recurse into every cell's `LayoutBlock` content and
/// re-enter `emit_paragraph_text` with the cell's absolute origin.
fn emit_table_text(
    content: &mut Content,
    page_h: f32,
    origin_x: f32,
    origin_y: f32,
    t: &TableBox,
    font_objs: &[(String, FontObj)],
) {
    let tx = origin_x + t.origin.x;
    let ty = origin_y + t.origin.y;
    for row in &t.rows {
        let row_x = tx + row.origin.x;
        let row_y = ty + row.origin.y;
        for cell in &row.cells {
            if matches!(cell.v_merge, engine::VMergeRole::Continue) {
                continue;
            }
            let cell_x = row_x + cell.origin.x;
            let cell_y = row_y + cell.origin.y;
            for inner in &cell.content {
                match inner {
                    LayoutBlock::Paragraph(p) => {
                        emit_paragraph_text(content, page_h, cell_x, cell_y, p, font_objs);
                    }
                    LayoutBlock::Table(nested) => {
                        emit_table_text(content, page_h, cell_x, cell_y, nested, font_objs);
                    }
                }
            }
        }
    }
}

/// Pass 3 — every cell edge + the outer-table perimeter as PDF strokes.
/// Uses the same "right + bottom win" convention as `paint_table` so the
/// PDF doesn't double-stroke shared edges.
fn emit_table_borders(
    content: &mut Content,
    page_h: f32,
    origin_x: f32,
    origin_y: f32,
    t: &TableBox,
) {
    let tx = origin_x + t.origin.x;
    let ty = origin_y + t.origin.y;
    for row in &t.rows {
        let row_x = tx + row.origin.x;
        let row_y = ty + row.origin.y;
        for cell in &row.cells {
            if matches!(cell.v_merge, engine::VMergeRole::Continue) {
                continue;
            }
            let cell_x = row_x + cell.origin.x;
            let cell_y = row_y + cell.origin.y;
            let cx1 = cell_x + cell.size.width;
            let cy1 = cell_y + cell.size.height;
            stroke_border_edge(
                content,
                page_h,
                &cell.borders.top,
                cell_x,
                cell_y,
                cx1,
                cell_y,
            );
            stroke_border_edge(
                content,
                page_h,
                &cell.borders.left,
                cell_x,
                cell_y,
                cell_x,
                cy1,
            );
            stroke_border_edge(content, page_h, &cell.borders.right, cx1, cell_y, cx1, cy1);
            stroke_border_edge(content, page_h, &cell.borders.bottom, cell_x, cy1, cx1, cy1);
        }
    }
    let tx1 = tx + t.size.width;
    let ty1 = ty + t.size.height;
    stroke_border_edge(content, page_h, &t.outer_borders.top, tx, ty, tx1, ty);
    stroke_border_edge(content, page_h, &t.outer_borders.left, tx, ty, tx, ty1);
    stroke_border_edge(content, page_h, &t.outer_borders.right, tx1, ty, tx1, ty1);
    stroke_border_edge(content, page_h, &t.outer_borders.bottom, tx, ty1, tx1, ty1);
}

/// Stroke one cell-edge segment as a PDF path. `(x0, y0)` and `(x1, y1)`
/// are the edge's endpoints in *layout* coordinates (y-down); each is
/// inverted before the `m` / `l` ops emit. `<w:sz>` is eighths of a
/// point; 1 pt is the PDF user-space unit, so the line width is just
/// `size_eighth_pt / 8.0`. Clamped to ≥ 0.25 so a stroke stays visible
/// when a viewer renders at low zoom.
fn stroke_border_edge(
    content: &mut Content,
    page_h: f32,
    edge: &Option<engine::BorderStroke>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
) {
    let Some(stroke) = edge else { return };
    if matches!(stroke.style, engine::BorderStyle::None) {
        return;
    }
    let weight = ((stroke.size_eighth_pt as f32) / 8.0).max(0.25);
    let [r, g, b, _] = stroke.color.unwrap_or([0, 0, 0, 255]);
    content.set_stroke_rgb(
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    );
    content.set_line_width(weight);
    let py0 = page_h - y0;
    let py1 = page_h - y1;
    content.move_to(x0, py0);
    content.line_to(x1, py1);
    content.stroke();
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

/// Depth-first walk of every paragraph emitted by a page, in
/// document order: top-level paragraphs first, then each table's
/// rows × cells × cell.content (skipping `VMergeRole::Continue`
/// cells — they share their content with the Restart cell above
/// them and a second emission would double-count fonts + text). The
/// order matches `emit_table_text` so callers indexing into a
/// flattened paragraph list stay aligned.
fn for_each_paragraph<'a, F: FnMut(&'a ParagraphBox)>(blocks: &'a [LayoutBlock], f: &mut F) {
    for b in blocks {
        match b {
            LayoutBlock::Paragraph(p) => f(p),
            LayoutBlock::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        if matches!(cell.v_merge, engine::VMergeRole::Continue) {
                            continue;
                        }
                        for_each_paragraph(&cell.content, f);
                    }
                }
            }
        }
    }
}

/// Harvest a glyph-id → Unicode map, per font, from the laid-out page.
///
/// `para_texts[i]` is the source text of `page.paragraphs[i]`. Every glyph
/// carries a `cluster` byte offset into its run's `source_range`; a run's
/// distinct clusters partition its source bytes into segments, and a glyph
/// decodes to the characters of its segment. A ligature glyph therefore maps to
/// *all* the characters it consumed — the property that lets shaped Arabic copy
/// back as real text.
/// Phase 6 — `collect_to_unicode` over every page in a paginated document.
/// Paragraphs look up their source text by `ParagraphBox::source_paragraph_id`
/// — split paragraphs share an id, so their two laid-out halves contribute
/// to the same per-font CMap entries without conflict.
fn collect_to_unicode_pages(
    pages: &[PageBox],
    para_texts: &[&str],
) -> HashMap<String, BTreeMap<u16, Vec<char>>> {
    let mut out: HashMap<String, BTreeMap<u16, Vec<char>>> = HashMap::new();
    for page in pages {
        for_each_paragraph(&page.blocks, &mut |para| {
            let text = if para.source_paragraph_id == layout::ParagraphBox::NO_SOURCE_ID {
                ""
            } else {
                para_texts
                    .get(para.source_paragraph_id as usize)
                    .copied()
                    .unwrap_or("")
            };
            for line in &para.lines {
                for run in &line.runs {
                    let map = out.entry(run.font.clone()).or_default();
                    add_run_mappings(run, text, map);
                }
            }
        });
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
        let mut para = layout_paragraph(ParagraphConfig {
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
            marker_text: None,
            px_size_for_marker: 26.0,
        });
        /* Phase 6 — `source_paragraph_id` is engine-wasm's job in
        production; the test stamps it manually so `/ToUnicode` lookups
        against the supplied `para_texts` resolve. */
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
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &["Hello world"],
            PdfProfile::Plain,
            &mut out,
        )
        .expect("export");

        assert!(out.starts_with(b"%PDF-"), "missing PDF header");
        assert!(out.ends_with(b"%%EOF"), "missing EOF marker");
        assert!(
            out.windows(10).any(|w| w == b"/FontFile2"),
            "font not embedded"
        );
    }

    /// Phase 5b — a `TableBox` exports cleanly. The previous stub
    /// silently skipped tables; this asserts the cell paragraph
    /// contributed a font + the PDF carries the rect-fill + path-
    /// stroke operators a viewer needs to paint shading + borders.
    #[test]
    fn table_exports_with_shading_and_borders() {
        let stack = liberation_stack();
        let spans = vec![StyleSpan {
            start: 0,
            end: "hi".len() as u32,
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
            text: "hi",
            fonts: &stack,
            spans: &spans,
            base_direction: ShapingDirection::Ltr,
            max_width: 200.0,
            line_height: 22.0,
            alignment: Alignment::Start,
            indent_start_px: 0.0,
            indent_end_px: 0.0,
            first_line_indent_px: 0.0,
            hanging_indent_px: 0.0,
            marker_text: None,
            px_size_for_marker: 22.0,
        });
        let cell_borders = engine::default_word_borders();
        let cell = layout::TableCellBox {
            origin: layout::Point { x: 0.0, y: 0.0 },
            size: layout::Size {
                width: 200.0,
                height: 30.0,
            },
            grid_span: 1,
            v_merge: engine::VMergeRole::None,
            borders: cell_borders.clone(),
            shading: Some([0xFF, 0xEB, 0x78, 0xFF]),
            content: vec![LayoutBlock::Paragraph(para)],
        };
        let row = layout::TableRowBox {
            origin: layout::Point { x: 0.0, y: 0.0 },
            size: layout::Size {
                width: 200.0,
                height: 30.0,
            },
            cells: vec![cell],
        };
        let table = TableBox {
            origin: layout::Point { x: 0.0, y: 100.0 },
            size: layout::Size {
                width: 200.0,
                height: 30.0,
            },
            columns: vec![200.0],
            rows: vec![row],
            outer_borders: cell_borders,
        };
        let page = PageBox {
            size: Size {
                width: 595.0,
                height: 842.0,
            },
            margins: Margins::uniform(72.0),
            blocks: vec![LayoutBlock::Table(table)],
            header: None,
            footer: None,
        };
        let mut out = Vec::new();
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &["hi"],
            PdfProfile::Plain,
            &mut out,
        )
        .expect("export table");
        assert!(out.starts_with(b"%PDF-"));
        assert!(out.ends_with(b"%%EOF"));
        /* The cell's paragraph must reach the font-embedding pass —
        without the cell-recursion fix this would be 0 fonts. */
        assert!(
            out.windows(10).any(|w| w == b"/FontFile2"),
            "cell paragraph must contribute its font to the embedded set"
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
            blocks: vec![],
            header: None,
            footer: None,
        };
        let mut out = Vec::new();
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &[],
            PdfProfile::Plain,
            &mut out,
        )
        .expect("export empty page");
        assert!(out.starts_with(b"%PDF-"), "missing PDF header");
        assert!(out.ends_with(b"%%EOF"), "missing EOF marker");
    }

    #[test]
    fn exports_pdfa1b_compliance_markers() {
        let stack = liberation_stack();
        let page = hello_page(&stack);

        let mut out = Vec::new();
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &["Hello world"],
            PdfProfile::A1b,
            &mut out,
        )
        .expect("export A1b");

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
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &["Hello world"],
            PdfProfile::A1b,
            &mut a,
        )
        .expect("export a");
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &["Hello world"],
            PdfProfile::A1b,
            &mut b,
        )
        .expect("export b");
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
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &["Hello world"],
            PdfProfile::Plain,
            &mut out,
        )
        .expect("export");

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
        let map = collect_to_unicode_pages(std::slice::from_ref(&page), &["Hello world"]);
        let liberation = map.get("liberation").expect("liberation font mapped");
        let covered: HashSet<char> = liberation.values().flatten().copied().collect();
        for ch in "Helo wrd".chars() {
            assert!(covered.contains(&ch), "ToUnicode map missing {ch:?}");
        }
    }
}
