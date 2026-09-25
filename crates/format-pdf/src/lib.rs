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
//! # PDF/A-2u
//!
//! [`PdfProfile::A2u`] targets ISO 19005-2 conformance level U: the same
//! sRGB `OutputIntent` + document `/ID` machinery as A-1b, a PDF 1.7 header
//! (PDF/A-2 is based on ISO 32000-1), and an XMP packet claiming
//! `pdfaid:part` 2 / `pdfaid:conformance` U. Level U's defining requirement —
//! every text string maps to Unicode — is carried by the `/ToUnicode` CMaps
//! every profile already emits. Level A tagging is out of scope, as for A-1.
//!
//! # PDF/X-3
//!
//! [`PdfProfile::X3`] targets PDF/X-3:2003 (ISO 15930-6): a PDF 1.4 header,
//! an `OutputIntent` with subtype `GTS_PDFX`, an Info dictionary carrying
//! `/Title`, `/GTS_PDFXVersion (PDF/X-3:2003)`, `/Trapped /False` and
//! creation/modification dates, a `/TrimBox` on every page (equal to the
//! MediaBox — this document model has no bleed, and X-3 forbids inventing a
//! BleedBox that lies), and a matching XMP packet. Honest deviations from a
//! strict print workflow:
//!
//! - The output condition is `Custom` with the synthesized sRGB ICC profile
//!   as `DestOutputProfile` — X-3 explicitly allows device-independent
//!   (ICC-characterized) data, but a real print shop would substitute a
//!   measured CMYK printing condition (e.g. FOGRA39).
//! - `/CreationDate`, `/ModDate` and the XMP dates are a **fixed, documented
//!   timestamp** (`X3_DATE_XMP`) so exports stay byte-deterministic
//!   (reproducible-build convention); they do not reflect wall-clock time.
//! - `/Title` is a fixed string (`X3_TITLE`) — the export API carries no
//!   document title today.
//!
//! # Images (issue #121)
//!
//! [`export_pdf_with_media`] embeds every image the layout places — inline
//! object glyphs in any story and picture floats from `PageBox::floats` —
//! as an image XObject drawn with one `cm` + `Do` into the exact layout
//! rect (y inverted like every glyph). Floats keep the scene's z-order:
//! the `behindDoc` group before any text, the in-front group last, each
//! sorted by `relativeHeight`. Each distinct media payload is written
//! once and shared by every page that references it. JPEG passes through
//! as `/DCTDecode` (dimensions from the SOF marker); PNG, GIF (first frame,
//! issue #189) and WebP (lossy + lossless, issue #189) are decoded in pure
//! Rust (`png` / `gif` / `image-webp`; the latter two behind the default
//! `gif` / `webp` features) and re-deflated. Transparency rule: plain
//! output and PDF/A-2u carry alpha as an `/SMask`; PDF/A-1b (which forbids
//! `/SMask`) and PDF/X-3:2003 (which forbids transparency) composite every
//! pixel onto opaque white instead. No per-image ICC profile is embedded —
//! samples are `DeviceRGB` / `DeviceGray` under the document's sRGB output
//! intent; a CMYK JPEG is embedded only in plain output. EMF / WMF (and GIF /
//! WebP with their feature off) and corrupt or oversized images are skipped
//! with a [`PdfWarning`].
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
use layout::{LayoutBlock, PageBox, ParagraphBox, TabLeaderKind, TableBox, VisualRun};
use pdf_writer::types::{
    CidFontType, FontFlags, OutputIntentSubtype, SystemInfo, TextRenderingMode, TrappingStatus,
    UnicodeCmap,
};
use pdf_writer::{Content, Date, Filter, Name, Pdf, Rect, Ref, Str, TextStr};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use text_pipeline::{FontStack, LoadedFont};

mod image;
#[cfg(test)]
mod image_export_tests;
#[doc(hidden)]
pub use image::test_images;
/// Issue #227 — the `format_pdf_image_decode` fuzz target drives
/// `prepare_image` end to end from OUTSIDE this crate (`fuzz/` is its own
/// workspace), so these need to be reachable at the crate root. Nothing
/// else changes: `image` itself stays a private module, and no new
/// dependency reaches the wasm build (`AlphaMode`/`PreparedImage`/
/// `ImageColor`/`ImageEncoding`/`prepare_image` were already compiled in,
/// just not nameable from outside).
pub use image::{AlphaMode, ImageColor, ImageEncoding, PreparedImage, prepare_image};
pub use image::{ImageSkipReason, MAX_IMAGE_PIXELS};

/// Issue #258 — the shared PDF content-stream / string-literal decoder.
/// See the module's own doc comment for why it lives here rather than in
/// `engine-wasm`'s test tree.
#[doc(hidden)]
pub mod test_support;

/// The synthesized sRGB ICC profile the PDF/A-1b output intent embeds. Built by
/// `build.rs` — see this module's docs for why it is generated, not vendored.
const SRGB_ICC: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/srgb-v2-micro.icc"));

/// XMP metadata packet for a PDF/A document. The `pdfaid` keys are the
/// conformance claim veraPDF checks; no Info dictionary is written, so there is
/// nothing this must be kept consistent with (ISO 19005-1 §6.7.3 / 19005-2
/// §6.6.4). `part` / `conformance` are `1`/`B` for A-1b and `2`/`U` for A-2u —
/// the packet bytes for `(1, 'B')` are identical to the pre-A2u constant, so
/// the A-1b output stays byte-stable.
fn pdfa_xmp(part: u8, conformance: char) -> String {
    format!(
        concat!(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n",
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n",
            " <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n",
            "  <rdf:Description rdf:about=\"\"\n",
            "    xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\"\n",
            "    xmlns:dc=\"http://purl.org/dc/elements/1.1/\"\n",
            "    xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\">\n",
            "   <pdfaid:part>{part}</pdfaid:part>\n",
            "   <pdfaid:conformance>{conformance}</pdfaid:conformance>\n",
            "   <dc:format>application/pdf</dc:format>\n",
            "   <xmp:CreatorTool>next-gen-editor</xmp:CreatorTool>\n",
            "  </rdf:Description>\n",
            " </rdf:RDF>\n",
            "</x:xmpmeta>\n",
            "<?xpacket end=\"r\"?>",
        ),
        part = part,
        conformance = conformance,
    )
}

/// `/Title` for the PDF/X-3 Info dictionary — the export API carries no
/// document title, so a fixed honest placeholder keeps the required key
/// present *and* the output byte-deterministic.
const X3_TITLE: &str = "next-gen-editor document";

/// The fixed timestamp PDF/X-3 output stamps into `/CreationDate`, `/ModDate`
/// and the XMP dates. X-3 *requires* both date keys, but a wall-clock value
/// would break the byte-determinism the document `/ID` and the stability
/// tests guarantee — so, per the reproducible-build convention, the date is a
/// documented constant, not the real export time.
const X3_DATE_XMP: &str = "2026-01-01T00:00:00+00:00";

/// [`X3_DATE_XMP`] in `pdf_writer::Date` form (`D:20260101000000+00'00`).
fn x3_date() -> Date {
    Date::new(2026)
        .month(1)
        .day(1)
        .hour(0)
        .minute(0)
        .second(0)
        .utc_offset_hour(0)
}

/// XMP metadata packet for a PDF/X-3 document, kept consistent with the Info
/// dictionary (`dc:title` ↔ `/Title`, `pdfxid:GTS_PDFXVersion` ↔
/// `/GTS_PDFXVersion`, `pdf:Trapped` ↔ `/Trapped`, dates ↔
/// `/CreationDate` + `/ModDate`).
fn x3_xmp() -> String {
    format!(
        concat!(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n",
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n",
            " <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n",
            "  <rdf:Description rdf:about=\"\"\n",
            "    xmlns:pdfxid=\"http://www.npes.org/pdfx/ns/id/\"\n",
            "    xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\"\n",
            "    xmlns:dc=\"http://purl.org/dc/elements/1.1/\"\n",
            "    xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\">\n",
            "   <pdfxid:GTS_PDFXVersion>PDF/X-3:2003</pdfxid:GTS_PDFXVersion>\n",
            "   <pdf:Trapped>False</pdf:Trapped>\n",
            "   <dc:format>application/pdf</dc:format>\n",
            "   <dc:title>\n",
            "    <rdf:Alt>\n",
            "     <rdf:li xml:lang=\"x-default\">{title}</rdf:li>\n",
            "    </rdf:Alt>\n",
            "   </dc:title>\n",
            "   <xmp:CreatorTool>next-gen-editor</xmp:CreatorTool>\n",
            "   <xmp:CreateDate>{date}</xmp:CreateDate>\n",
            "   <xmp:ModifyDate>{date}</xmp:ModifyDate>\n",
            "  </rdf:Description>\n",
            " </rdf:RDF>\n",
            "</x:xmpmeta>\n",
            "<?xpacket end=\"r\"?>",
        ),
        title = X3_TITLE,
        date = X3_DATE_XMP,
    )
}

/// Shear factor for faux italic — mirrors `render::synth::SHEAR` (~12.4°)
/// so the PDF oblique matches the canvas raster synthesis.
const FAUX_ITALIC_SHEAR: f32 = 0.22;

/// Conformance target for [`export_pdf`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfProfile {
    /// A plain PDF — no archival conformance structures (the Phase 3 output).
    Plain,
    /// PDF/A-1b (ISO 19005-1 level B): adds a PDF 1.4 header, an sRGB
    /// `OutputIntent`, an XMP metadata packet and a document `/ID` on top of
    /// the full font embedding the plain path already produces.
    A1b,
    /// PDF/A-2u (ISO 19005-2 level U): the A-1b structures with a PDF 1.7
    /// header and an XMP claim of `pdfaid:part` 2 / `pdfaid:conformance` U.
    /// Level U's Unicode-mapping requirement rides on the `/ToUnicode` CMaps
    /// every profile ships; level A tagging remains out of scope.
    A2u,
    /// PDF/X-3:2003 (ISO 15930-6): PDF 1.4, a `GTS_PDFX` `OutputIntent`
    /// (`Custom` condition backed by the synthesized sRGB ICC profile — X-3
    /// permits ICC-characterized RGB data), an Info dictionary with `/Title`,
    /// `/GTS_PDFXVersion`, `/Trapped /False` and fixed deterministic dates,
    /// and `/TrimBox` = MediaBox on every page (no bleed in this document
    /// model, so no BleedBox is invented). See the module docs for the honest
    /// deviations from a real print-shop workflow.
    X3,
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

/// Content-stream resources the page emitters resolve against: the font
/// resource table, and (issue #121) each embeddable image's resource name
/// keyed by relationship id. An image absent from `images` paints nothing.
///
/// Issue #144 — `stack` is the loaded-face counterpart of `fonts`: the
/// tab-leader painter needs an actual glyph id / advance-width lookup
/// (`LoadedFont::glyph_id` / `glyph_metrics`) for the dot/hyphen/
/// underscore fill character, which the `(String, FontObj)` resource
/// table alone cannot answer.
struct Res<'a> {
    fonts: &'a [(String, FontObj)],
    images: &'a HashMap<String, String>,
    stack: &'a FontStack,
}

/// A non-fatal export note (the `DocxWarning` counterpart): the PDF is
/// still produced, but something in the document did not make it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfWarning {
    /// Issue #121 — an image the layout places (inline or floating) was not
    /// embedded; its rect stays blank. One warning per relationship id.
    ImageSkipped {
        rel_id: String,
        reason: ImageSkipReason,
    },
}

/// What [`export_pdf_with_media`] did beyond writing bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdfExportReport {
    /// Distinct image XObjects written (after content deduplication; a
    /// soft mask is not counted separately).
    pub images_embedded: usize,
    pub warnings: Vec<PdfWarning>,
}

/// Export `pages` to a PDF, appending the bytes to `out`.
///
/// Image-free wrapper over [`export_pdf_with_media`]: every inline or
/// floating image rect is left blank (and reported in the discarded
/// report). Output for a document without images is identical.
pub fn export_pdf(
    pages: &[PageBox],
    fonts: &FontStack,
    para_texts: &[&str],
    profile: PdfProfile,
    out: &mut Vec<u8>,
) -> Result<(), String> {
    export_pdf_with_media(pages, fonts, para_texts, &HashMap::new(), profile, out).map(|_| ())
}

/// Export `pages` to a PDF with images, appending the bytes to `out`.
///
/// Issue #121 — `media` is the document's image parts keyed by
/// relationship id (`DocumentTree::media`). Every image the layout places —
/// inline object glyphs in any story (body, cells, header/footer bands,
/// notes, text boxes) and picture floats in `PageBox::floats` — becomes an
/// image XObject, **written once** per distinct media payload and shared by
/// every page that shows it. JPEG passes through as `/DCTDecode`; PNG
/// re-encodes as `/FlateDecode` samples. Transparency depends on the
/// profile: plain and PDF/A-2u keep PNG alpha as an `/SMask`; PDF/A-1b
/// (ISO 19005-1 forbids `/SMask`) and PDF/X-3:2003 (no transparency)
/// composite onto white. Anything that cannot be embedded is skipped with a
/// [`PdfWarning`] — never an error.
///
/// The rest of the contract is [`export_pdf`]'s original one:
///
/// Phase 6 — every `PageBox` in `pages` becomes one PDF page; each has its
/// own MediaBox sized from `page.size`, and a dedicated content stream.
/// `fonts` must contain every face referenced by any page's runs (full
/// embedding). `para_texts[i]` is the source text of the paragraph whose
/// `source_paragraph_id == i`; PDF resolves each laid-out paragraph's
/// glyph clusters against that table, so a paragraph split across pages
/// (head + tail) gets the same `/ToUnicode` mapping on both halves
/// (their `source_paragraph_id` is identical). `profile` selects plain
/// output or one of the PDF/A-1b, PDF/A-2u, PDF/X-3 conformance targets.
pub fn export_pdf_with_media(
    pages: &[PageBox],
    fonts: &FontStack,
    para_texts: &[&str],
    media: &HashMap<String, engine::ImageBlob>,
    profile: PdfProfile,
    out: &mut Vec<u8>,
) -> Result<PdfExportReport, String> {
    let pdfa = matches!(profile, PdfProfile::A1b | PdfProfile::A2u);
    let pdfx = profile == PdfProfile::X3;
    /* Every conformance target shares the ICC output intent, the XMP
    metadata stream and the deterministic document `/ID`. */
    let conformant = pdfa || pdfx;

    /* Distinct fonts referenced by every page, in first-seen order.
    Issue #71 — bands + footnotes collect too: `show_run` silently
    draws NOTHING for a font missing from `font_objs` (the pen still
    advances), so a header-only font would otherwise become invisible
    text with no error. */
    let mut used: Vec<String> = Vec::new();
    for page in pages {
        let mut collect = |para: &ParagraphBox| {
            if let Some(marker) = &para.marker
                && !used.iter().any(|u| u == &marker.run.font)
            {
                used.push(marker.run.font.clone());
            }
            for line in &para.lines {
                for run in &line.runs {
                    if !used.iter().any(|u| u == &run.font) {
                        used.push(run.font.clone());
                    }
                }
            }
        };
        for_each_paragraph(&page.blocks, &mut collect);
        if let Some(hf) = &page.header {
            for_each_paragraph(&hf.blocks, &mut collect);
        }
        if let Some(hf) = &page.footer {
            for_each_paragraph(&hf.blocks, &mut collect);
        }
        page.footnotes.for_each_paragraph(&mut collect);
        page.endnotes.for_each_paragraph(&mut collect);
        /* Issue #83 — text-box stories embed their fonts too. */
        for f in &page.floats {
            if let Some(tb) = f.text_box.as_deref() {
                /* Issue #165 — nested stories too. */
                tb.for_each_story_blocks(&mut |blocks| for_each_paragraph(blocks, &mut collect));
            }
        }
    }

    let mut pdf = Pdf::new();
    match profile {
        PdfProfile::Plain => {}
        /* PDF/A-1 is based on PDF 1.4; PDF/X-3:2003 likewise. */
        PdfProfile::A1b | PdfProfile::X3 => pdf.set_version(1, 4),
        /* PDF/A-2 is based on ISO 32000-1 (PDF 1.7). */
        PdfProfile::A2u => pdf.set_version(1, 7),
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
                id.clone(),
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
    let icc_id = if conformant { Some(alloc()) } else { None };
    let metadata_id = if conformant { Some(alloc()) } else { None };
    /* X-3 alone requires an Info dictionary (`/Title`, `/GTS_PDFXVersion`,
    `/Trapped`, dates); PDF/A deliberately writes none — see `pdfa_xmp`. */
    let info_id = if pdfx { Some(alloc()) } else { None };

    /* Issue #121 — images. Every rel id the pages place, in first-seen
    order; each is prepared once, deduplicated by payload bytes (two rel
    ids naming the same picture share one XObject), and allocated AFTER
    every pre-existing object so image-free output is byte-identical. */
    let mut report = PdfExportReport::default();
    let alpha_mode = match profile {
        PdfProfile::Plain | PdfProfile::A2u => AlphaMode::SoftMask,
        PdfProfile::A1b | PdfProfile::X3 => AlphaMode::FlattenOnWhite,
    };
    let page_rels: Vec<Vec<&str>> = pages.iter().map(page_image_rels).collect();
    let mut image_names: HashMap<String, String> = HashMap::new();
    let mut xobjs: Vec<ImageObj<'_>> = Vec::new();
    let mut tried: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for &rel in page_rels.iter().flatten() {
        if !tried.insert(rel) {
            continue;
        }
        let Some(blob) = media.get(rel) else {
            report.warnings.push(PdfWarning::ImageSkipped {
                rel_id: rel.to_string(),
                reason: ImageSkipReason::MissingMedia,
            });
            continue;
        };
        if let Some(existing) = xobjs.iter().find(|x| x.source == blob.data.as_slice()) {
            image_names.insert(rel.to_string(), existing.resource.clone());
            continue;
        }
        match image::prepare_image(&blob.data, &blob.content_type, alpha_mode, !conformant) {
            Ok(img) => {
                let resource = format!("Im{}", xobjs.len());
                image_names.insert(rel.to_string(), resource.clone());
                let id = alloc();
                let smask = img.alpha.as_ref().map(|_| alloc());
                xobjs.push(ImageObj {
                    id,
                    smask,
                    resource,
                    source: &blob.data,
                    img,
                });
            }
            Err(reason) => report.warnings.push(PdfWarning::ImageSkipped {
                rel_id: rel.to_string(),
                reason,
            }),
        }
    }
    report.images_embedded = xobjs.len();
    let res = Res {
        fonts: &font_objs,
        images: &image_names,
        stack: fonts,
    };

    /* Build every page's content stream first — the digest in PDF/A `/ID`
    is keyed on the concatenated uncompressed content so split exports
    stay stable. */
    let contents: Vec<Vec<u8>> = pages
        .iter()
        .map(|page| build_page_content(page, &res))
        .collect();
    if conformant {
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
        if conformant {
            catalog.metadata(metadata_id.expect("metadata id allocated for conformance"));
            let mut intents = catalog.output_intents();
            let mut intent = intents.push();
            if pdfa {
                /* GTS_PDFA1 is the subtype for every PDF/A part (1–4). */
                intent
                    .subtype(OutputIntentSubtype::PDFA)
                    .output_condition_identifier(TextStr("sRGB IEC61966-2.1"))
                    .output_condition(TextStr("sRGB IEC61966-2.1"))
                    .registry_name(TextStr("http://www.color.org"))
                    .info(TextStr("sRGB IEC61966-2.1"))
                    .dest_output_profile(icc_id.expect("icc id allocated for conformance"));
            } else {
                /* X-3: no registered printing condition — `Custom` with the
                embedded sRGB profile as the characterization. `/Info` is
                mandatory when the identifier names no registry entry. */
                intent
                    .subtype(OutputIntentSubtype::PDFX)
                    .output_condition_identifier(TextStr("Custom"))
                    .output_condition(TextStr("sRGB IEC61966-2.1"))
                    .info(TextStr("sRGB IEC61966-2.1"))
                    .dest_output_profile(icc_id.expect("icc id allocated for conformance"));
            }
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
    for (page_idx, ((page_id, content_id), page)) in page_refs.iter().zip(pages.iter()).enumerate()
    {
        let media = Rect::new(0.0, 0.0, page.size.width, page.size.height);
        let mut p = pdf.page(*page_id);
        p.parent(pages_id);
        p.media_box(media);
        if pdfx {
            /* X-3 §6.1: every page carries a TrimBox (or ArtBox). TrimBox is
            not inheritable, so it goes on each page — equal to the MediaBox
            because this document model has no bleed. */
            p.trim_box(media);
        }
        p.contents(*content_id);
        let mut resources = p.resources();
        {
            let mut font_dict = resources.fonts();
            for (_, fo) in &font_objs {
                font_dict.pair(Name(fo.resource.as_bytes()), fo.type0);
            }
        }
        /* Issue #121 — only the images this page shows; no `/XObject`
        key at all on an image-free page. */
        let mut names: Vec<&String> = page_rels[page_idx]
            .iter()
            .filter_map(|rel| image_names.get(*rel))
            .collect();
        names.sort();
        names.dedup();
        if !names.is_empty() {
            let mut xo = resources.x_objects();
            for name in names {
                if let Some(x) = xobjs.iter().find(|x| &x.resource == name) {
                    xo.pair(Name(name.as_bytes()), x.id);
                }
            }
        }
    }

    /* glyph-id → Unicode per font, harvested across every page. Split
    paragraphs contribute clusters from both halves into the same source
    text — the union is what the CMap actually wants. */
    let to_unicode = collect_to_unicode_pages(pages, para_texts, fonts);
    for (id, fo) in &font_objs {
        let face = fonts
            .face(id)
            .ok_or_else(|| format!("export_pdf: font `{id}` not in the stack"))?;
        embed_font(&mut pdf, id, face, fo);
        let cmap_z = deflate(&build_unicode_cmap(to_unicode.get(id)));
        pdf.stream(fo.to_unicode, &cmap_z)
            .filter(Filter::FlateDecode);
    }

    if let Some(info_id) = info_id {
        /* X-3 Info dictionary — every key mirrored into the XMP packet so
        the two stay consistent. The dates are the fixed deterministic
        timestamp; see `X3_DATE_XMP`. */
        let mut info = pdf.document_info(info_id);
        info.title(TextStr(X3_TITLE));
        info.creation_date(x3_date());
        info.modified_date(x3_date());
        info.trapped(TrappingStatus::NotTrapped);
        info.pair(Name(b"GTS_PDFXVersion"), TextStr("PDF/X-3:2003"));
    }

    if conformant {
        pdf.icc_profile(icc_id.expect("icc id allocated for conformance"), SRGB_ICC)
            .n(3);
        let xmp = match profile {
            PdfProfile::A1b => pdfa_xmp(1, 'B'),
            PdfProfile::A2u => pdfa_xmp(2, 'U'),
            PdfProfile::X3 => x3_xmp(),
            PdfProfile::Plain => unreachable!("Plain writes no metadata stream"),
        };
        pdf.metadata(
            metadata_id.expect("metadata id allocated for conformance"),
            xmp.as_bytes(),
        );
    }

    for x in &xobjs {
        write_image_xobject(&mut pdf, x);
    }

    out.extend_from_slice(&pdf.finish());
    Ok(report)
}

/// Issue #121 — one embedded image: its XObject ref, optional soft-mask
/// ref, resource name, and the source bytes it was prepared from (the
/// dedup key).
struct ImageObj<'a> {
    id: Ref,
    smask: Option<Ref>,
    resource: String,
    source: &'a [u8],
    img: PreparedImage,
}

/// Issue #121 — write an image XObject (+ its `/SMask`). 8 bits per
/// component throughout; no `/Interpolate`, `/Alternates` or `/OPI`
/// (all forbidden or discouraged in PDF/A).
fn write_image_xobject(pdf: &mut Pdf, x: &ImageObj<'_>) {
    let img = &x.img;
    let (bytes, filter) = match img.encoding {
        ImageEncoding::Dct => (img.data.clone(), Filter::DctDecode),
        ImageEncoding::Raw => (deflate(&img.data), Filter::FlateDecode),
    };
    {
        let mut xo = pdf.image_xobject(x.id, &bytes);
        xo.filter(filter);
        xo.width(img.width as i32);
        xo.height(img.height as i32);
        match img.color {
            ImageColor::Gray => xo.color_space().device_gray(),
            ImageColor::Rgb => xo.color_space().device_rgb(),
            ImageColor::Cmyk => xo.color_space().device_cmyk(),
        }
        xo.bits_per_component(8);
        if img.invert_cmyk {
            xo.decode([1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0]);
        }
        if let Some(sm) = x.smask {
            xo.s_mask(sm);
        }
    }
    if let (Some(sm), Some(alpha)) = (x.smask, img.alpha.as_ref()) {
        let z = deflate(alpha);
        let mut mask = pdf.image_xobject(sm, &z);
        mask.filter(Filter::FlateDecode);
        mask.width(img.width as i32);
        mask.height(img.height as i32);
        mask.color_space().device_gray();
        mask.bits_per_component(8);
    }
}

/// Issue #121 — relationship ids of every image `build_page_content` can
/// paint on `page`, in paint-walk order (duplicates kept; callers dedupe):
/// inline image glyphs in the body, header/footer bands, note bands and
/// visible text-box stories, then visible picture floats. Degenerate
/// (zero-area) objects paint nothing and are not collected.
fn page_image_rels<'a>(page: &'a PageBox) -> Vec<&'a str> {
    let mut rels: Vec<&'a str> = Vec::new();
    let mut collect = |para: &'a ParagraphBox| {
        for line in &para.lines {
            for run in &line.runs {
                for g in &run.glyphs {
                    if let Some(rel) = g.inline_image_rel_id.as_deref()
                        && g.x_advance > 0.0
                        && g.inline_object_height > 0.0
                    {
                        rels.push(rel);
                    }
                }
            }
        }
    };
    if let Some(hf) = &page.header {
        for_each_paragraph(&hf.blocks, &mut collect);
    }
    for_each_paragraph(&page.blocks, &mut collect);
    page.endnotes.for_each_paragraph(&mut collect);
    page.footnotes.for_each_paragraph(&mut collect);
    if let Some(hf) = &page.footer {
        for_each_paragraph(&hf.blocks, &mut collect);
    }
    let visible = |f: &&layout::FloatBox| !f.hidden && f.size.width > 0.0 && f.size.height > 0.0;
    let mut frame_floats: Vec<&'a str> = Vec::new();
    for f in page.floats.iter().filter(visible) {
        if let Some(tb) = f.text_box.as_deref() {
            collect_frame_image_rels(tb, &mut collect, &mut frame_floats);
        }
    }
    rels.extend(frame_floats);
    for f in page.floats.iter().filter(visible) {
        if f.text_box.is_none() {
            rels.push(&f.rel_id);
        }
    }
    rels
}

/// Issue #165 / #197 — the images one visible text-box frame paints:
/// inline image glyphs of its story, then (depth-first, bounded by the
/// layout's nesting cap) every visible float of the story — a nested
/// box recurses, a floating picture contributes its own rel id.
fn collect_frame_image_rels<'a>(
    tb: &'a layout::TextBoxFrame,
    collect: &mut dyn FnMut(&'a ParagraphBox),
    rels: &mut Vec<&'a str>,
) {
    for_each_paragraph(&tb.blocks, &mut |p| collect(p));
    for f in &tb.floats {
        if f.hidden || f.size.width <= 0.0 || f.size.height <= 0.0 {
            continue;
        }
        match f.text_box.as_deref() {
            Some(inner) => collect_frame_image_rels(inner, collect, rels),
            None => rels.push(&f.rel_id),
        }
    }
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
/// 1. **Shading.** Cell `<w:shd>` + paragraph `<w:pPr><w:shd>` fills as
///    `re` / `f` outside any text block.
/// 2. **Text.** Per paragraph: run highlight (`bg_color`) rects, then a
///    `BT`/`ET` block placing glyphs via an absolute text matrix, then
///    underline / strike decoration fills over the glyphs.
/// 3. **Borders.** Cell + table-outer + paragraph `<w:pBdr>` strokes.
///
/// The order mirrors `crates/render/src/scene.rs::paint_table` so PDF and
/// Canvas2D agree on layering: shading sits behind content, borders sit on
/// top.
#[cfg(test)]
fn build_content(page: &PageBox, font_objs: &[(String, FontObj)], fonts: &FontStack) -> Vec<u8> {
    build_page_content(
        page,
        &Res {
            fonts: font_objs,
            images: &HashMap::new(),
            stack: fonts,
        },
    )
}

fn build_page_content(page: &PageBox, res: &Res<'_>) -> Vec<u8> {
    let page_h = page.size.height;
    let content_x = page.margins.left;
    let content_y = page.margins.top;
    let mut content = Content::new();

    /* Issue #83 / #121 — the behind-text float group (pictures + text
    boxes, z-ordered) paints first (scene.rs order). */
    emit_floats(&mut content, page, true, res);

    /* Issue #71 — header band BEFORE body (mirrors
    `render/scene.rs::build_document_scene` ordering: header, body,
    footnotes, footer). Band origin comes from the SAME shared
    placement methods the canvas paints with. */
    if let Some(hf) = &page.header {
        emit_band_blocks(
            &mut content,
            page_h,
            content_x,
            page.header_band_top(),
            &hf.blocks,
            res,
        );
    }

    /* Pass 1 — cell + paragraph shading. */
    for block in &page.blocks {
        emit_block_shading(&mut content, page_h, content_x, content_y, block);
    }

    /* Pass 2 — every paragraph's highlights + glyphs + decorations
    (top-level + cell content), in document order. */
    for block in &page.blocks {
        match block {
            LayoutBlock::Paragraph(p) => {
                emit_paragraph_text(&mut content, page_h, content_x, content_y, p, res);
            }
            LayoutBlock::Table(t) => {
                emit_table_text(&mut content, page_h, content_x, content_y, t, res);
            }
        }
    }

    /* Pass 3 — cell + table-outer + paragraph borders. */
    for block in &page.blocks {
        emit_block_borders(&mut content, page_h, content_x, content_y, block);
    }

    /* Issue #80 — note bands at the paginator's `NoteBand::y` (the
    SAME placement scene.rs paints): separator rule (30 % of the content
    width, full width for a continuation) + every entry's blocks. */
    for band in [&page.endnotes, &page.footnotes] {
        if band.is_empty() {
            continue;
        }
        let band_top = band.y;
        let content_w = page.size.width - page.margins.left - page.margins.right;
        let rule_w = if band.continuation {
            content_w
        } else {
            content_w * 0.3
        };
        let rule_y = band_top - 6.0;
        content.save_state();
        content.set_fill_rgb(
            0x55 as f32 / 255.0,
            0x55 as f32 / 255.0,
            0x55 as f32 / 255.0,
        );
        content.rect(content_x, page_h - (rule_y + 0.75), rule_w, 0.75);
        content.fill_nonzero();
        content.restore_state();
        for entry in &band.entries {
            let entry_x = content_x + entry.origin.x;
            let entry_top = band_top + entry.origin.y;
            for block in &entry.blocks {
                emit_block_shading(&mut content, page_h, entry_x, entry_top, block);
            }
            for block in &entry.blocks {
                match block {
                    LayoutBlock::Paragraph(p) => {
                        emit_paragraph_text(&mut content, page_h, entry_x, entry_top, p, res);
                    }
                    LayoutBlock::Table(t) => {
                        emit_table_text(&mut content, page_h, entry_x, entry_top, t, res);
                    }
                }
            }
            for block in &entry.blocks {
                emit_block_borders(&mut content, page_h, entry_x, entry_top, block);
            }
        }
    }

    /* Issue #71 — footer band last (bottom-anchored via
    `footer_band_top`, which already subtracts the laid content
    height). */
    if let Some(hf) = &page.footer {
        emit_band_blocks(
            &mut content,
            page_h,
            content_x,
            page.footer_band_top(),
            &hf.blocks,
            res,
        );
    }

    /* Issue #83 / #121 — the in-front float group closes the page. */
    emit_floats(&mut content, page, false, res);

    content.finish().to_vec()
}

/// Issue #83 / #121 / #165 — one z-order group of the page's floating
/// objects (the scene's `paint_floats` twin: `behind` selects the
/// `behindDoc` group, sorted stably by z-order).
fn emit_floats(content: &mut Content, page: &PageBox, behind: bool, res: &Res<'_>) {
    emit_float_group(
        content,
        page.size.height,
        &page.floats,
        0.0,
        0.0,
        behind,
        res,
    );
}

/// One z-order group of `floats` whose origins are relative to
/// `(base_x, base_y)` in layout space — the page for page floats, the
/// parent's content rect for nested boxes (issue #165). A picture paints
/// its image XObject into the float's wrap-independent rect (issue #121);
/// a text box paints shape fill, the story clipped to the shape rect
/// (with its own nested boxes), outline (issue #83).
fn emit_float_group(
    content: &mut Content,
    page_h: f32,
    floats: &[layout::FloatBox],
    base_x: f32,
    base_y: f32,
    behind: bool,
    res: &Res<'_>,
) {
    let mut group: Vec<&layout::FloatBox> = floats
        .iter()
        .filter(|f| f.behind_doc == behind && !f.hidden)
        .collect();
    group.sort_by_key(|f| f.z_order);
    for f in group {
        if f.size.width <= 0.0 || f.size.height <= 0.0 {
            continue;
        }
        match f.text_box.as_deref() {
            Some(tb) => emit_text_box(content, page_h, f, tb, base_x, base_y, res),
            None => emit_image(
                content,
                page_h,
                &f.rel_id,
                [
                    base_x + f.origin.x,
                    base_y + f.origin.y,
                    f.size.width,
                    f.size.height,
                ],
                res,
            ),
        }
    }
}

/// Issue #121 — paint one image XObject into a layout-space rect
/// `[x, top, w, h]` (`x`, `top` = the rect's top-left, y-down). The image unit square is
/// mapped by one `cm`: scale to `w`×`h`, translate to the rect's PDF-space
/// bottom-left (`page_h - (top + h)`). An image the exporter could not
/// embed (missing / unsupported / corrupt media) paints nothing.
fn emit_image(
    content: &mut Content,
    page_h: f32,
    rel_id: &str,
    [x, top, w, h]: [f32; 4],
    res: &Res<'_>,
) {
    let Some(name) = res.images.get(rel_id) else {
        return;
    };
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    content.save_state();
    content.transform([w, 0.0, 0.0, h, x, page_h - (top + h)]);
    content.x_object(Name(name.as_bytes()));
    content.restore_state();
}

/// Issue #83 — one text box: shape fill, the clipped story (with its
/// nested boxes — the `behindDoc` group under the story text, the rest
/// over it, issue #165), outline. Origins are relative to
/// `(base_x, base_y)` in layout space.
fn emit_text_box(
    content: &mut Content,
    page_h: f32,
    f: &layout::FloatBox,
    tb: &layout::TextBoxFrame,
    base_x: f32,
    base_y: f32,
    res: &Res<'_>,
) {
    let (x, w, h) = (base_x + f.origin.x, f.size.width, f.size.height);
    let pdf_y = page_h - (base_y + f.origin.y + h);
    if let Some([r, g, b, _]) = tb.source.fill {
        content.save_state();
        content.set_fill_rgb(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
        );
        content.rect(x, pdf_y, w, h);
        content.fill_nonzero();
        content.restore_state();
    }
    if let Some((origin, _)) = f.text_box_content_rect() {
        let (cx, cy) = (base_x + origin.x, base_y + origin.y);
        content.save_state();
        content.rect(x, pdf_y, w, h);
        content.clip_nonzero();
        content.end_path();
        emit_float_group(content, page_h, &tb.floats, cx, cy, true, res);
        for block in &tb.blocks {
            emit_block_shading(content, page_h, cx, cy, block);
        }
        for block in &tb.blocks {
            match block {
                LayoutBlock::Paragraph(p) => {
                    emit_paragraph_text(content, page_h, cx, cy, p, res);
                }
                LayoutBlock::Table(t) => {
                    emit_table_text(content, page_h, cx, cy, t, res);
                }
            }
        }
        for block in &tb.blocks {
            emit_block_borders(content, page_h, cx, cy, block);
        }
        emit_float_group(content, page_h, &tb.floats, cx, cy, false, res);
        content.restore_state();
    }
    if let Some(([r, g, b, _], lw)) = tb.source.outline
        && lw > 0.0
    {
        content.save_state();
        content.set_stroke_rgb(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
        );
        content.set_line_width(lw);
        content.rect(x, pdf_y, w, h);
        content.stroke();
        content.restore_state();
    }
}

/// Issue #71 — one header/footer band: the same shading → text →
/// borders pass ordering the body uses, at the band's own origin.
fn emit_band_blocks(
    content: &mut Content,
    page_h: f32,
    band_x: f32,
    band_y: f32,
    blocks: &[LayoutBlock],
    res: &Res<'_>,
) {
    for block in blocks {
        emit_block_shading(content, page_h, band_x, band_y, block);
    }
    for block in blocks {
        match block {
            LayoutBlock::Paragraph(p) => {
                emit_paragraph_text(content, page_h, band_x, band_y, p, res);
            }
            LayoutBlock::Table(t) => {
                emit_table_text(content, page_h, band_x, band_y, t, res);
            }
        }
    }
    for block in blocks {
        emit_block_borders(content, page_h, band_x, band_y, block);
    }
}

/// Pass 1 dispatcher — shading fills for one block (recurses into cells).
fn emit_block_shading(
    content: &mut Content,
    page_h: f32,
    origin_x: f32,
    origin_y: f32,
    block: &LayoutBlock,
) {
    match block {
        LayoutBlock::Paragraph(p) => emit_paragraph_shading(content, page_h, origin_x, origin_y, p),
        LayoutBlock::Table(t) => emit_table_shading(content, page_h, origin_x, origin_y, t),
    }
}

/// Pass 3 dispatcher — border strokes for one block (recurses into cells).
fn emit_block_borders(
    content: &mut Content,
    page_h: f32,
    origin_x: f32,
    origin_y: f32,
    block: &LayoutBlock,
) {
    match block {
        LayoutBlock::Paragraph(p) => emit_paragraph_borders(content, page_h, origin_x, origin_y, p),
        LayoutBlock::Table(t) => emit_table_borders(content, page_h, origin_x, origin_y, t),
    }
}

/// Pass 1 (paragraph) — `<w:pPr><w:shd>` fill behind the paragraph's
/// bounding rect. Mirrors `render/scene.rs::paint_paragraph`'s shading
/// fill; borders + glyphs draw on top in the later passes.
fn emit_paragraph_shading(
    content: &mut Content,
    page_h: f32,
    origin_x: f32,
    origin_y: f32,
    para: &ParagraphBox,
) {
    let Some([r, g, b, _]) = para.shading else {
        return;
    };
    let para_x = origin_x + para.origin.x;
    let para_y = origin_y + para.origin.y;
    let pdf_y = page_h - (para_y + para.size.height);
    content.set_fill_rgb(
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    );
    content.rect(para_x, pdf_y, para.size.width, para.size.height);
    content.fill_nonzero();
}

/// Pass 3 (paragraph) — `<w:pPr><w:pBdr>` strokes at the paragraph's
/// bounding rect, reusing the cell-border edge primitive.
fn emit_paragraph_borders(
    content: &mut Content,
    page_h: f32,
    origin_x: f32,
    origin_y: f32,
    para: &ParagraphBox,
) {
    let Some(b) = para.borders.as_ref() else {
        return;
    };
    let x0 = origin_x + para.origin.x;
    let y0 = origin_y + para.origin.y;
    let x1 = x0 + para.size.width;
    let y1 = y0 + para.size.height;
    stroke_border_edge(content, page_h, &b.top, x0, y0, x1, y0);
    stroke_border_edge(content, page_h, &b.left, x0, y0, x0, y1);
    stroke_border_edge(content, page_h, &b.right, x1, y0, x1, y1);
    stroke_border_edge(content, page_h, &b.bottom, x0, y1, x1, y1);
}

/// Emit text for one `ParagraphBox`. `origin_x` / `origin_y` are the
/// containing block's origin in absolute layout-px (top-left); the
/// paragraph's own origin is added on top. PDF y is bottom-up — every
/// glyph's y inverts via `page_h - layout_y`.
///
/// Three sub-passes mirror `render/scene.rs::paint_paragraph` layering:
/// run highlight rects first (behind the glyphs), then the marker + line
/// glyphs in one `BT`/`ET` block, then underline / strike fills on top.
fn emit_paragraph_text(
    content: &mut Content,
    page_h: f32,
    origin_x: f32,
    origin_y: f32,
    para: &ParagraphBox,
    res: &Res<'_>,
) {
    let para_x = origin_x + para.origin.x;
    let para_y = origin_y + para.origin.y;

    /* Background highlights — a run's `bg_color` fills the line's full
    height across the run's advance so adjacent highlights tile. */
    for line in &para.lines {
        let line_x = para_x + line.origin.x;
        let line_top = para_y + line.origin.y;
        let mut pen = 0.0_f32;
        for run in &line.runs {
            let advance = run_advance(run);
            if let Some([r, g, b, _]) = run.attrs.bg_color
                && advance > 0.0
            {
                content.set_fill_rgb(
                    f32::from(r) / 255.0,
                    f32::from(g) / 255.0,
                    f32::from(b) / 255.0,
                );
                content.rect(
                    line_x + pen,
                    page_h - (line_top + line.height),
                    advance,
                    line.height,
                );
                content.fill_nonzero();
            }
            pen += advance;
        }
    }

    /* Issue #121 — inline images, over the highlights and under the
    glyphs. Same rect as `render/scene.rs::paint_paragraph`: the pen
    position, `x_advance` wide, bottom edge flush with the LINE baseline,
    `inline_object_height` tall. */
    if !res.images.is_empty() {
        for line in &para.lines {
            let line_x = para_x + line.origin.x;
            let baseline = para_y + line.origin.y + line.baseline;
            let mut pen = 0.0_f32;
            for run in &line.runs {
                for glyph in &run.glyphs {
                    if let Some(rel) = glyph.inline_image_rel_id.as_deref() {
                        let h = glyph.inline_object_height;
                        emit_image(
                            content,
                            page_h,
                            rel,
                            [line_x + pen, baseline - h, glyph.x_advance, h],
                            res,
                        );
                    }
                    pen += glyph.x_advance;
                }
            }
        }
    }

    content.begin_text();
    /* List marker — its own gutter run, baseline-aligned with the first
    line (mirrors `render/scene.rs::paint_paragraph`). */
    if let Some(marker) = &para.marker {
        let m_x = para_x + marker.origin.x;
        let m_baseline = para_y + marker.origin.y + marker.baseline;
        show_run(content, page_h, &marker.run, m_x, m_baseline, res);
    }
    for line in &para.lines {
        let line_x = para_x + line.origin.x;
        /* Layout-space baseline — distance down from the page top. */
        let baseline = para_y + line.origin.y + line.baseline;
        let mut pen = 0.0_f32;
        for run in &line.runs {
            pen += show_run(content, page_h, run, line_x + pen, baseline, res);
        }
    }
    content.end_text();

    /* Issue #144 — tab leaders (`<w:tab w:leader>`, e.g. TOC dot
    leaders). `show_run` draws nothing for a leadered tab glyph (see
    above); its fill is painted here instead, outside any `BT`/`ET`
    block — `re`/`f` (and the leader glyphs' own nested `BT`/`ET`) are
    not legal *inside* the paragraph's main text object. Dot / hyphen /
    underscore tile REAL repeated glyphs from the run's own font (so PDF
    text extraction returns the leader characters, matching what a
    viewer copies out of Word); heavy / middleDot — and any font that
    can't shape the fill character — fall back to a plain filled rule,
    the PDF twin of `render/scene.rs::push_tab_leader`. */
    for line in &para.lines {
        let line_x = para_x + line.origin.x;
        let baseline = para_y + line.origin.y + line.baseline;
        let mut pen = 0.0_f32;
        for run in &line.runs {
            for glyph in &run.glyphs {
                let x0 = line_x + pen;
                let x1 = x0 + glyph.x_advance;
                if let Some(kind) = glyph.leader {
                    let [r, g, b, _] = run.attrs.color;
                    content.set_fill_rgb(
                        f32::from(r) / 255.0,
                        f32::from(g) / 255.0,
                        f32::from(b) / 255.0,
                    );
                    let mut drew = false;
                    if let Some(ch) = leader_fill_char(kind)
                        && let Some(face) = res.stack.face(&run.font)
                        && let Some((_, fo)) = res.fonts.iter().find(|(id, _)| id == &run.font)
                    {
                        let font = LeaderFont {
                            face,
                            resource: &fo.resource,
                            px_size: run.attrs.px_size,
                        };
                        drew = emit_tab_leader_glyphs(content, page_h, &font, ch, x0, x1, baseline);
                    }
                    if !drew {
                        emit_tab_leader_rule(
                            content,
                            page_h,
                            kind,
                            x0,
                            x1,
                            baseline,
                            run.attrs.px_size,
                        );
                    }
                }
                pen += glyph.x_advance;
            }
        }
    }

    /* Decoration fills — over the glyphs, same metrics as
    `render/scene.rs` (underline just below the baseline, strike centred
    ~a quarter em above it, thickness scaling with the px size). Both
    anchor to the line baseline, not the shifted run baseline. */
    for line in &para.lines {
        let line_x = para_x + line.origin.x;
        let baseline = para_y + line.origin.y + line.baseline;
        let mut pen = 0.0_f32;
        for run in &line.runs {
            let advance = run_advance(run);
            let x0 = line_x + pen;
            let x1 = x0 + advance;
            pen += advance;
            let underline_visible = run.attrs.underline.is_visible();
            if (!underline_visible && !run.attrs.strike) || x1 <= x0 {
                continue;
            }
            let px = run.attrs.px_size;
            let thickness = (px * 0.06).max(1.0);
            let [r, g, b, _] = run.attrs.color;
            content.set_fill_rgb(
                f32::from(r) / 255.0,
                f32::from(g) / 255.0,
                f32::from(b) / 255.0,
            );
            if underline_visible {
                let top = baseline + px * 0.10;
                emit_underline_pattern(
                    content,
                    page_h,
                    run.attrs.underline,
                    x0,
                    x1,
                    top,
                    thickness,
                );
            }
            if run.attrs.strike {
                let mid = baseline - px * 0.25;
                fill_decoration_rect(content, page_h, x0, x1, mid - thickness / 2.0, thickness);
            }
        }
    }
}

/// Total advance of a run's glyphs — the run's x-extent on its line.
fn run_advance(run: &VisualRun) -> f32 {
    run.glyphs.iter().map(|g| g.x_advance).sum()
}

/// Show one run's glyphs inside an open text object. `run_x` is the run's
/// absolute pen start; `baseline` the layout-space line baseline. Returns
/// the run's total advance so the caller's pen stays in sync even when the
/// run's font is missing from `res`.
fn show_run(
    content: &mut Content,
    page_h: f32,
    run: &VisualRun,
    run_x: f32,
    baseline: f32,
    res: &Res<'_>,
) -> f32 {
    let Some((_, fo)) = res.fonts.iter().find(|(id, _)| id == &run.font) else {
        return run_advance(run);
    };
    let [r, g, b, _] = run.attrs.color;
    content.set_font(Name(fo.resource.as_bytes()), run.attrs.px_size);
    content.set_fill_rgb(
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    );
    if run.attrs.faux_bold {
        /* Faux bold — fill + stroke (`2 Tr`). The stroke width mirrors
        `render::synth::embolden`: its dilation radius is px_size / 22,
        and a centred stroke of that width adds the same total ink. */
        content.set_stroke_rgb(
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
        );
        content.set_line_width((run.attrs.px_size / 22.0).max(0.25));
        content.set_text_rendering_mode(TextRenderingMode::FillStroke);
    }
    let shear = if run.attrs.faux_italic {
        FAUX_ITALIC_SHEAR
    } else {
        0.0
    };
    let mut pen = 0.0_f32;
    for glyph in &run.glyphs {
        /* Issue #69 — a floating object's sentinel glyph reserves no
        width and shows nothing in the line (the object is positioned
        from `PageBox::floats`); skip it so a font that maps U+FFFC to a
        real glyph never prints a zero-advance mark. */
        /* Issue #121 — likewise an INLINE image's object-replacement
        glyph: the image paints as an XObject (`emit_paragraph_text`),
        the glyph itself never shows (scene.rs parity). */
        if glyph.float.is_some() || glyph.inline_image_rel_id.is_some() {
            pen += glyph.x_advance;
            continue;
        }
        /* Issue #144 — a leadered tab glyph shows nothing here; its
        leader fill (real repeated glyphs or a rule) is painted by
        `emit_paragraph_text` in a separate pass once this `BT`/`ET`
        block closes — `re`/`f` are not legal text-object operators, and
        a leader glyph's own show needs its own nested text object. */
        if glyph.leader.is_some() {
            pen += glyph.x_advance;
            continue;
        }
        /* Issue #258 (found via the tier-a `toc-leaders.docx` corpus
        fixture — veraPDF flagged it under PDF/A-2u, ISO 19005-2 §6.2.11.8,
        "the document contains a reference to the .notdef glyph") — a
        control character with no visual form in the run's font (the
        engine's own `\u{000C}` FORM FEED encoding of `<w:br
        w:type="page"/>` is the case that surfaced this; any character a
        face can't shape hits the same path) shapes to glyph id 0. `render/
        scene.rs`'s Canvas2D paint already skips it ("glyph id 0 is
        .notdef — advance the pen, draw nothing"); this mirrors that
        exactly — PDF/A-1 and -2 both forbid `.notdef` in ANY text-showing
        operator, so it must never reach a `Tj`, not even one a viewer
        would render as nothing. */
        if glyph.id != 0 {
            let gx = run_x + pen + glyph.x_offset;
            /* Invert the y axis: PDF origin is bottom-left. `<w:vertAlign>`
            baseline shift lifts (positive) / drops (negative) the run. */
            let gy = page_h - (baseline - glyph.y_offset - run.attrs.baseline_shift_px);
            content.set_text_matrix([1.0, 0.0, shear, 1.0, gx, gy]);
            let id = glyph.id;
            content.show(Str(&[(id >> 8) as u8, (id & 0xff) as u8]));
        }
        pen += glyph.x_advance;
    }
    if run.attrs.faux_bold {
        /* Text state persists across BT/ET — restore fill-only. */
        content.set_text_rendering_mode(TextRenderingMode::Fill);
    }
    pen
}

/// Issue #144 — the Unicode character a "repeated glyph" tab leader tiles.
/// `None` for the two kinds Word draws as a plain rule instead of a
/// character run (`Heavy`, `MiddleDot`) — see `emit_tab_leader_rule`.
fn leader_fill_char(kind: TabLeaderKind) -> Option<char> {
    match kind {
        TabLeaderKind::Dot => Some('.'),
        TabLeaderKind::Hyphen => Some('-'),
        TabLeaderKind::Underscore => Some('_'),
        TabLeaderKind::Heavy | TabLeaderKind::MiddleDot => None,
    }
}

/// The run font's face + PDF resource name + point size — bundled so
/// `emit_tab_leader_glyphs` stays under clippy's argument-count lint.
struct LeaderFont<'a> {
    face: &'a LoadedFont,
    resource: &'a str,
    px_size: f32,
}

/// Issue #144 — repeat `ch` as REAL shown glyphs across `x0..x1` at
/// `baseline`, using the font's own glyph advance so consecutive
/// characters tile without gaps or overlap (the way Word repeats a
/// leader character). A PDF viewer's text extraction therefore returns
/// the leader characters themselves, not just ink. Returns `false` when
/// the font has no glyph for `ch` (or no measurable advance) so the
/// caller can fall back to `emit_tab_leader_rule` — a leader must never
/// silently vanish because a font lacks a period.
///
/// Must run outside any `BT`/`ET` block: it opens its own nested text
/// object, which is only legal once the paragraph's main text object has
/// already been closed with `content.end_text()`.
fn emit_tab_leader_glyphs(
    content: &mut Content,
    page_h: f32,
    font: &LeaderFont,
    ch: char,
    x0: f32,
    x1: f32,
    baseline: f32,
) -> bool {
    let Some(gid) = font.face.glyph_id(ch) else {
        return false;
    };
    let Ok(metrics) = font.face.glyph_metrics(ch, font.px_size) else {
        return false;
    };
    let step = metrics.advance_width;
    if step <= 0.0 {
        return false;
    }
    let pad = font.px_size * 0.15;
    let lo = x0 + pad;
    let hi = x1 - pad;
    if hi <= lo {
        return true;
    }
    let mut x = (lo / step).ceil() * step;
    if x + step > hi {
        return true;
    }
    let gy = page_h - baseline;
    let bytes = [(gid >> 8) as u8, (gid & 0xff) as u8];
    content.begin_text();
    content.set_font(Name(font.resource.as_bytes()), font.px_size);
    while x + step <= hi {
        content.set_text_matrix([1.0, 0.0, 0.0, 1.0, x, gy]);
        content.show(Str(&bytes));
        x += step;
    }
    content.end_text();
    true
}

/// Issue #144 — the PDF twin of `render/scene.rs::push_tab_leader`: fills
/// a leadered tab's advance with ink (not real glyphs) using the same
/// per-kind pattern math. Used for `Heavy` / `MiddleDot` (Word renders
/// these as a rule, not a repeated character) and as the defensive
/// fallback when the run's font can't shape the dot/hyphen/underscore
/// fill character. `re`/`f` are not legal `BT`/`ET` operators, so this
/// must run outside any open text object.
fn emit_tab_leader_rule(
    content: &mut Content,
    page_h: f32,
    kind: TabLeaderKind,
    x0: f32,
    x1: f32,
    baseline: f32,
    px: f32,
) {
    let px = px.max(1.0);
    let pad = px * 0.15;
    let (lo, hi) = (x0 + pad, x1 - pad);
    if hi <= lo {
        return;
    }
    let dot = (px * 0.08).max(1.0);
    let rect = |content: &mut Content, a: f32, top: f32, b: f32, bottom: f32| {
        content.rect(a, page_h - bottom, b - a, bottom - top);
        content.fill_nonzero();
    };
    match kind {
        TabLeaderKind::Dot | TabLeaderKind::MiddleDot => {
            let step = px * 0.33;
            let y = if matches!(kind, TabLeaderKind::Dot) {
                baseline - dot
            } else {
                baseline - px * 0.3
            };
            let mut x = (lo / step).ceil() * step;
            while x + dot <= hi {
                rect(content, x, y, x + dot, y + dot);
                x += step;
            }
        }
        TabLeaderKind::Hyphen => {
            let step = px * 0.4;
            let dash = px * 0.25;
            let y = baseline - px * 0.28;
            let mut x = (lo / step).ceil() * step;
            while x + dash <= hi {
                rect(content, x, y, x + dash, y + dot);
                x += step;
            }
        }
        TabLeaderKind::Underscore | TabLeaderKind::Heavy => {
            let t = if matches!(kind, TabLeaderKind::Heavy) {
                dot * 2.0
            } else {
                dot
            };
            let y = baseline + px * 0.1;
            rect(content, lo, y, hi, y + t);
        }
    }
}

/// Fill one decoration rectangle given its layout-space top edge. The
/// current fill colour is the run's text colour, set by the caller.
fn fill_decoration_rect(
    content: &mut Content,
    page_h: f32,
    x0: f32,
    x1: f32,
    top: f32,
    thickness: f32,
) {
    if x1 <= x0 {
        return;
    }
    content.rect(x0, page_h - (top + thickness), x1 - x0, thickness);
    content.fill_nonzero();
}

/// Emit the underline rectangles for a single `(x0..x1, top)` span —
/// the PDF twin of `render/scene.rs::push_underline_pattern`, with the
/// same pattern math (all multiples of `thickness`) so PDF and canvas
/// agree on the dotted / dashed / wavy approximations.
fn emit_underline_pattern(
    content: &mut Content,
    page_h: f32,
    style: engine::UnderlineStyle,
    x0: f32,
    x1: f32,
    top: f32,
    thickness: f32,
) {
    use engine::UnderlineStyle::*;
    match style {
        None => {}
        Single => fill_decoration_rect(content, page_h, x0, x1, top, thickness),
        Double => {
            fill_decoration_rect(content, page_h, x0, x1, top, thickness);
            fill_decoration_rect(content, page_h, x0, x1, top + thickness * 2.0, thickness);
        }
        Dotted => {
            let pitch = (thickness * 2.0).max(2.0);
            let mut x = x0;
            while x < x1 {
                let end = (x + thickness).min(x1);
                fill_decoration_rect(content, page_h, x, end, top, thickness);
                x += pitch;
            }
        }
        Dashed => {
            let dash = (thickness * 4.0).max(3.0);
            let gap = dash;
            let mut x = x0;
            while x < x1 {
                let end = (x + dash).min(x1);
                fill_decoration_rect(content, page_h, x, end, top, thickness);
                x += dash + gap;
            }
        }
        Wavy => {
            /* Sawtooth: tile pairs of short rects on alternating rows.
            Period = `4 * thickness`; each half-period is one short rect. */
            let half = (thickness * 2.0).max(2.0);
            let top_band = top - thickness;
            let bottom_band = top + thickness;
            let mut x = x0;
            let mut up = true;
            while x < x1 {
                let end = (x + half).min(x1);
                let band_top = if up { top_band } else { bottom_band };
                fill_decoration_rect(content, page_h, x, end, band_top, thickness);
                up = !up;
                x += half;
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
    /* Recurse — cell paragraphs + nested tables carry their own shading.
    Same origins as `emit_table_text` so fills align with the glyphs. */
    for row in &t.rows {
        let row_x = tx + row.origin.x;
        let row_y = ty + row.origin.y;
        for cell in &row.cells {
            if matches!(cell.v_merge, engine::VMergeRole::Continue) {
                continue;
            }
            let cell_x = row_x + cell.origin.x;
            let cell_y = row_y + cell.origin.y;
            /* Content sits inside the resolved <w:tcMar> padding — the
            frame rect above stays unpadded (fills cover the whole cell),
            but nested content recurses from the padded origin, matching
            render/scene.rs paint_table. */
            let content_x = cell_x + cell.padding_left;
            let content_y = cell_y + cell.padding_top;
            with_cell_clip(
                content,
                page_h,
                row,
                cell,
                cell_x,
                cell_y,
                |content, inner| {
                    emit_block_shading(content, page_h, content_x, content_y, inner);
                },
            );
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
    res: &Res<'_>,
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
            /* Issue #31 — content origin is offset by the cell's resolved
            <w:tcMar>/<w:tblCellMar> padding (boxes.rs contract: inner
            origins are frame-relative, the consumer adds the padding),
            exactly like render/scene.rs paint_table's content pass. */
            let content_x = cell_x + cell.padding_left;
            let content_y = cell_y + cell.padding_top;
            with_cell_clip(
                content,
                page_h,
                row,
                cell,
                cell_x,
                cell_y,
                |content, inner| match inner {
                    LayoutBlock::Paragraph(p) => {
                        emit_paragraph_text(content, page_h, content_x, content_y, p, res);
                    }
                    LayoutBlock::Table(nested) => {
                        emit_table_text(content, page_h, content_x, content_y, nested, res);
                    }
                },
            );
        }
    }
}

/// Issue #169 — emit one cell's content blocks, clipped to the cell rect
/// when its row is `<w:trHeight w:hRule="exact">` (the row does not grow,
/// so content overflows it). Lines starting at or past the cell bottom
/// are dropped (`LayoutBlock::clip_lines_at`, the same cut the renderer
/// makes) and the straddling line is cut by the clip path. Rows that grow
/// to fit pass straight through, byte-identical to the unclipped path.
fn with_cell_clip(
    content: &mut Content,
    page_h: f32,
    row: &layout::TableRowBox,
    cell: &layout::TableCellBox,
    cell_x: f32,
    cell_y: f32,
    mut emit: impl FnMut(&mut Content, &LayoutBlock),
) {
    if !row.exact_height {
        for inner in &cell.content {
            emit(content, inner);
        }
        return;
    }
    content.save_state();
    content.rect(
        cell_x,
        page_h - (cell_y + cell.size.height),
        cell.size.width,
        cell.size.height,
    );
    content.clip_nonzero();
    content.end_path();
    let limit = cell.size.height - cell.padding_top;
    for inner in &cell.content {
        match inner.clip_lines_at(limit) {
            Some(clipped) => emit(content, &clipped),
            None if inner.origin().y < limit => emit(content, inner),
            None => {}
        }
    }
    content.restore_state();
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
            /* Recurse — cell paragraphs + nested tables carry their own
            borders. Same padded content origins as `emit_table_text`. */
            let content_x = cell_x + cell.padding_left;
            let content_y = cell_y + cell.padding_top;
            with_cell_clip(
                content,
                page_h,
                row,
                cell,
                cell_x,
                cell_y,
                |content, inner| {
                    emit_block_borders(content, page_h, content_x, content_y, inner);
                },
            );
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
            .default_width(0.0);
        cid.cid_to_gid_map_predefined(Name(b"Identity"));
        /* PDF/A-1b §6.3.5: per-CID widths in /W must match the font program's
        glyph advances. Use the font's own hmtx values in 1000-em units. */
        cid.widths().consecutive(0, face.widths_em1000());
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
    fonts: &FontStack,
) -> HashMap<String, BTreeMap<u16, Vec<char>>> {
    let mut out: HashMap<String, BTreeMap<u16, Vec<char>>> = HashMap::new();
    for page in pages {
        let mut collect = |para: &ParagraphBox| {
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
                    add_leader_mappings(run, fonts, map);
                    add_kashida_mappings(run, fonts, map);
                }
            }
        };
        for_each_paragraph(&page.blocks, &mut collect);
        /* Issue #71 — band + footnote glyphs need /ToUnicode coverage
        too (a header-only character otherwise copies as nothing).
        `source_paragraph_id` for band paragraphs indexes the SAME
        `para_texts` table — engine-wasm appends part texts after the
        body walk. */
        if let Some(hf) = &page.header {
            for_each_paragraph(&hf.blocks, &mut collect);
        }
        if let Some(hf) = &page.footer {
            for_each_paragraph(&hf.blocks, &mut collect);
        }
        page.footnotes.for_each_paragraph(&mut collect);
        page.endnotes.for_each_paragraph(&mut collect);
        for f in &page.floats {
            if let Some(tb) = f.text_box.as_deref() {
                /* Issue #165 — nested stories too. */
                tb.for_each_story_blocks(&mut |blocks| for_each_paragraph(blocks, &mut collect));
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
        /* A synthetic glyph (a Kashida tatweel, a tab leader placeholder)
        carries no source cluster — nothing here to look up. Issue #258:
        the tatweel case still gets a `/ToUnicode` entry, just via
        `add_kashida_mappings` (mapped to the real character it draws,
        U+0640 TATWEEL) rather than a cluster lookup — PDF/A-1b/2u (ISO
        19005 §6.2.11.7.2) requires every used glyph code to map to
        Unicode, with no "decorative ink" exception, and U+0640 is not a
        fiction: it is the actual Unicode character the glyph shapes. */
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

/// Issue #144 — a leadered tab glyph shows a REPEATED FILL CHARACTER (see
/// `emit_tab_leader_glyphs`), synthesized on the fly rather than shaped by
/// the layout pass, so it carries no source cluster for `add_run_mappings`
/// to find. Map its glyph id straight to the fill character so text
/// extraction returns the dots/hyphens/underscores, matching what a viewer
/// copies out of a real Word TOC. A no-op for `Heavy` / `MiddleDot`
/// (`leader_fill_char` returns `None`) or a font that can't shape the fill
/// character — `emit_tab_leader_rule` paints those as plain ink instead.
fn add_leader_mappings(run: &VisualRun, fonts: &FontStack, map: &mut BTreeMap<u16, Vec<char>>) {
    for glyph in &run.glyphs {
        let Some(kind) = glyph.leader else {
            continue;
        };
        let Some(ch) = leader_fill_char(kind) else {
            continue;
        };
        let Some(face) = fonts.face(&run.font) else {
            continue;
        };
        let Some(gid) = face.glyph_id(ch) else {
            continue;
        };
        map.entry(gid).or_insert_with(|| vec![ch]);
    }
}

/// Issue #258 (found via the `tools/pdf-validate` tier-a Arabic-Kashida
/// corpus fixture — veraPDF flagged it under PDF/A-2u, ISO 19005-2
/// §6.2.11.7.2, "glyph can not be mapped to Unicode") — `inject_kashida`
/// (`crates/layout/src/paragraph.rs`) synthesizes a real Tatweel glyph
/// (U+0640) to elongate a justified Arabic line; it is marked
/// `synthetic: true` (no source cluster, like a tab leader) so
/// `add_run_mappings` skips it, which otherwise left it out of
/// `/ToUnicode` entirely. Re-derive the same font's Tatweel glyph id
/// `inject_kashida` used and map every synthetic occurrence of it straight
/// to `'\u{0640}'` — the same "map the placeholder to the real character it
/// draws" technique `add_leader_mappings` already uses for tab leaders, and
/// the correct Unicode value (not a guess): a Tatweel elongation genuinely
/// IS U+0640, so copy-paste extracting it is accurate, not decorative
/// noise. A no-op when the run's font has no Tatweel glyph (nothing to
/// match) or the run injected none.
fn add_kashida_mappings(run: &VisualRun, fonts: &FontStack, map: &mut BTreeMap<u16, Vec<char>>) {
    let Some(face) = fonts.face(&run.font) else {
        return;
    };
    let Some(tatweel_gid) = face.glyph_id('\u{0640}') else {
        return;
    };
    for glyph in &run.glyphs {
        if glyph.synthetic && glyph.leader.is_none() && glyph.id == tatweel_gid {
            map.entry(glyph.id).or_insert_with(|| vec!['\u{0640}']);
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
    use layout::{
        Margins, MarkerBox, ParagraphConfig, Point, PositionedGlyph, Size, StyleSpan, TextAttrs,
        layout_paragraph,
    };
    use std::collections::HashSet;
    use std::sync::Arc;
    use text_pipeline::{Alignment, ShapingDirection};

    /// One full-coverage plain span at 18 px — tests override fields as needed.
    fn plain_span(len: u32) -> StyleSpan {
        StyleSpan {
            start: 0,
            end: len,
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
        }
    }

    /// Lay out `text` (one paragraph, optional list marker) on an A4 page.
    fn page_with(
        stack: &FontStack,
        text: &str,
        spans: &[StyleSpan],
        marker: Option<&str>,
    ) -> PageBox {
        let mut para = layout_paragraph(ParagraphConfig {
            text,
            fonts: stack,
            spans,
            base_direction: ShapingDirection::Ltr,
            max_width: 451.0,
            line_height: 26.0,
            line_height_exact: false,
            alignment: Alignment::Start,
            indent_start_px: if marker.is_some() { 36.0 } else { 0.0 },
            indent_end_px: 0.0,
            first_line_indent_px: 0.0,
            hanging_indent_px: 0.0,
            marker_text: marker.map(str::to_string),
            px_size_for_marker: 18.0,
            inline_objects: &[],
            tab_stops_px: &[],
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
            header_offset: 36.0,
            footer_offset: 36.0,
            footnotes: layout::NoteBand::default(),
            endnotes: layout::NoteBand::default(),
            hf_role: layout::HeaderRole::Default,
            page_number: 1,
            floats: Vec::new(),
        }
    }

    /// Lay out "Hello world" on an A4 page with the bundled Liberation face.
    fn hello_page(stack: &FontStack) -> PageBox {
        page_with(stack, "Hello world", &[plain_span(11)], None)
    }

    /// Font-resource table for driving `build_content` directly — the object
    /// refs are dummies; only `resource` reaches the content stream.
    fn test_font_objs(ids: &[&str]) -> Vec<(String, FontObj)> {
        ids.iter()
            .enumerate()
            .map(|(i, id)| {
                let base = (i * 5) as i32;
                (
                    (*id).to_string(),
                    FontObj {
                        type0: Ref::new(base + 1),
                        cid: Ref::new(base + 2),
                        descriptor: Ref::new(base + 3),
                        file: Ref::new(base + 4),
                        to_unicode: Ref::new(base + 5),
                        resource: format!("F{i}"),
                    },
                )
            })
            .collect()
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
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
            underline: engine::UnderlineStyle::None,
            strike: false,
            bg_color: None,
            font_family: None,
            caps_transform: false,
            baseline_shift_px: 0.0,
        }];
        let para = layout_paragraph(ParagraphConfig {
            text: "hi",
            fonts: &stack,
            spans: &spans,
            base_direction: ShapingDirection::Ltr,
            max_width: 200.0,
            line_height: 22.0,
            line_height_exact: false,
            alignment: Alignment::Start,
            indent_start_px: 0.0,
            indent_end_px: 0.0,
            first_line_indent_px: 0.0,
            hanging_indent_px: 0.0,
            marker_text: None,
            px_size_for_marker: 22.0,
            inline_objects: &[],
            tab_stops_px: &[],
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
            padding_left: 0.0,
            padding_top: 0.0,
            padding_right: 0.0,
            padding_bottom: 0.0,
            content_offset: 0,
        };
        let row = layout::TableRowBox {
            origin: layout::Point { x: 0.0, y: 0.0 },
            size: layout::Size {
                width: 200.0,
                height: 30.0,
            },
            cells: vec![cell],
            header: false,
            cant_split: false,
            source_row: 0,
            exact_height: false,
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
            placement_dx: 0.0,
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
            header_offset: 36.0,
            footer_offset: 36.0,
            footnotes: layout::NoteBand::default(),
            endnotes: layout::NoteBand::default(),
            hf_role: layout::HeaderRole::Default,
            page_number: 1,
            floats: Vec::new(),
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

    /// Issue #31 — cell content in the exported PDF is offset by the
    /// resolved `<w:tcMar>` padding, matching render/scene.rs. The padded
    /// export's first glyph text matrix must sit exactly (+left) in x and
    /// (−top) in PDF y relative to the zero-padding export.
    #[test]
    fn cell_padding_offsets_pdf_cell_content() {
        fn first_tm_xy(content: &[u8]) -> (f32, f32) {
            let s = String::from_utf8_lossy(content);
            let tokens: Vec<&str> = s.split_whitespace().collect();
            let idx = tokens.iter().position(|t| *t == "Tm").expect("Tm op");
            (
                tokens[idx - 2].parse().expect("Tm x operand"),
                tokens[idx - 1].parse().expect("Tm y operand"),
            )
        }
        fn table_page(stack: &FontStack, pad_left: f32, pad_top: f32) -> PageBox {
            let para = layout_paragraph(ParagraphConfig {
                text: "hi",
                fonts: stack,
                spans: &[plain_span("hi".len() as u32)],
                base_direction: ShapingDirection::Ltr,
                max_width: 200.0,
                line_height: 22.0,
                line_height_exact: false,
                alignment: Alignment::Start,
                indent_start_px: 0.0,
                indent_end_px: 0.0,
                first_line_indent_px: 0.0,
                hanging_indent_px: 0.0,
                marker_text: None,
                px_size_for_marker: 22.0,
                inline_objects: &[],
                tab_stops_px: &[],
            });
            let cell = layout::TableCellBox {
                origin: layout::Point { x: 0.0, y: 0.0 },
                size: layout::Size {
                    width: 200.0,
                    height: 40.0,
                },
                grid_span: 1,
                v_merge: engine::VMergeRole::None,
                borders: engine::default_word_borders(),
                shading: None,
                content: vec![LayoutBlock::Paragraph(para)],
                padding_left: pad_left,
                padding_top: pad_top,
                padding_right: 0.0,
                padding_bottom: 0.0,
                content_offset: 0,
            };
            let row = layout::TableRowBox {
                origin: layout::Point { x: 0.0, y: 0.0 },
                size: layout::Size {
                    width: 200.0,
                    height: 40.0,
                },
                cells: vec![cell],
                header: false,
                cant_split: false,
                source_row: 0,
                exact_height: false,
            };
            let table = TableBox {
                origin: layout::Point { x: 0.0, y: 100.0 },
                size: layout::Size {
                    width: 200.0,
                    height: 40.0,
                },
                columns: vec![200.0],
                rows: vec![row],
                outer_borders: engine::default_word_borders(),
                placement_dx: 0.0,
            };
            PageBox {
                size: Size {
                    width: 595.0,
                    height: 842.0,
                },
                margins: Margins::uniform(72.0),
                blocks: vec![LayoutBlock::Table(table)],
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
        let stack = liberation_stack();
        let fo = test_font_objs(&["liberation"]);
        let (x0, y0) = first_tm_xy(&build_content(&table_page(&stack, 0.0, 0.0), &fo, &stack));
        let (x1, y1) = first_tm_xy(&build_content(&table_page(&stack, 12.0, 8.0), &fo, &stack));
        assert!(
            (x1 - x0 - 12.0).abs() < 0.01,
            "padding_left must inset cell text in x: unpadded {x0}, padded {x1}"
        );
        assert!(
            (y0 - y1 - 8.0).abs() < 0.01,
            "padding_top must lower cell text (smaller PDF y): unpadded {y0}, padded {y1}"
        );
    }

    /// Issue #169 — an exact-height row clips its cell content: the cell
    /// rect becomes a clip path (`W n`) and the lines starting below the
    /// row are not emitted at all; a row that grows to fit emits every
    /// line with no clip.
    #[test]
    fn exact_height_row_clips_cell_content() {
        fn page(stack: &FontStack, exact: bool) -> PageBox {
            let para = layout_paragraph(ParagraphConfig {
                text: "aa bb cc",
                fonts: stack,
                spans: &[plain_span("aa bb cc".len() as u32)],
                base_direction: ShapingDirection::Ltr,
                max_width: 30.0,
                line_height: 22.0,
                line_height_exact: true,
                alignment: Alignment::Start,
                indent_start_px: 0.0,
                indent_end_px: 0.0,
                first_line_indent_px: 0.0,
                hanging_indent_px: 0.0,
                marker_text: None,
                px_size_for_marker: 22.0,
                inline_objects: &[],
                tab_stops_px: &[],
            });
            assert_eq!(para.lines.len(), 3, "one word per line");
            let h = if exact { 30.0 } else { para.size.height };
            let cell = layout::TableCellBox {
                origin: layout::Point { x: 0.0, y: 0.0 },
                size: layout::Size {
                    width: 200.0,
                    height: h,
                },
                grid_span: 1,
                v_merge: engine::VMergeRole::None,
                borders: engine::default_word_borders(),
                shading: None,
                content: vec![LayoutBlock::Paragraph(para)],
                padding_left: 0.0,
                padding_top: 0.0,
                padding_right: 0.0,
                padding_bottom: 0.0,
                content_offset: 0,
            };
            let row = layout::TableRowBox {
                origin: layout::Point { x: 0.0, y: 0.0 },
                size: layout::Size {
                    width: 200.0,
                    height: h,
                },
                cells: vec![cell],
                header: false,
                cant_split: exact,
                source_row: 0,
                exact_height: exact,
            };
            let table = TableBox {
                origin: layout::Point { x: 0.0, y: 100.0 },
                size: layout::Size {
                    width: 200.0,
                    height: h,
                },
                columns: vec![200.0],
                rows: vec![row],
                outer_borders: engine::default_word_borders(),
                placement_dx: 0.0,
            };
            PageBox {
                size: Size {
                    width: 595.0,
                    height: 842.0,
                },
                margins: Margins::uniform(72.0),
                blocks: vec![LayoutBlock::Table(table)],
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
        fn ops(content: &[u8], op: &str) -> usize {
            String::from_utf8_lossy(content)
                .split_whitespace()
                .filter(|t| *t == op)
                .count()
        }
        let stack = liberation_stack();
        let fo = test_font_objs(&["liberation"]);
        let exact = build_content(&page(&stack, true), &fo, &stack);
        let grown = build_content(&page(&stack, false), &fo, &stack);
        /* One clip per content pass (shading, text, borders). */
        assert_eq!(ops(&exact, "W"), 3, "the exact cell is clipped");
        assert_eq!(ops(&grown, "W"), 0, "a growing row is not clipped");
        /* 30 pt of a 3 × 22 pt paragraph: lines 1-2 start inside the row
        (line 2 straddles and is cut by the clip), line 3 is dropped. */
        let (te, tg) = (ops(&exact, "Tm"), ops(&grown, "Tm"));
        assert!(te > 0 && te < tg, "exact {te} vs grown {tg} glyph matrices");
    }

    /// Issue #79 — a `<w:bidiVisual>` table (mirrored by
    /// `layout::mirror_bidi_visual`) paints its LOGICAL first cell at the
    /// right: the cell texts' text-matrix x operands, in emission (logical)
    /// order, descend — the order a right-to-left reader / `pdftotext`
    /// reconstructs. The unmirrored table ascends.
    #[test]
    fn bidi_visual_table_paints_logical_first_cell_rightmost() {
        fn tm_xs(content: &[u8]) -> Vec<f32> {
            let s = String::from_utf8_lossy(content);
            let tokens: Vec<&str> = s.split_whitespace().collect();
            tokens
                .iter()
                .enumerate()
                .filter(|(_, t)| **t == "Tm")
                .map(|(i, _)| tokens[i - 2].parse().expect("Tm x operand"))
                .collect()
        }
        fn page(stack: &FontStack, mirrored: bool) -> PageBox {
            let cell = |text: &str, x: f32| {
                let para = layout_paragraph(ParagraphConfig {
                    text,
                    fonts: stack,
                    spans: &[plain_span(text.len() as u32)],
                    base_direction: ShapingDirection::Ltr,
                    max_width: 90.0,
                    line_height: 22.0,
                    line_height_exact: false,
                    alignment: Alignment::Start,
                    indent_start_px: 0.0,
                    indent_end_px: 0.0,
                    first_line_indent_px: 0.0,
                    hanging_indent_px: 0.0,
                    marker_text: None,
                    px_size_for_marker: 22.0,
                    inline_objects: &[],
                    tab_stops_px: &[],
                });
                layout::TableCellBox {
                    origin: layout::Point { x, y: 0.0 },
                    size: layout::Size {
                        width: 100.0,
                        height: 30.0,
                    },
                    grid_span: 1,
                    v_merge: engine::VMergeRole::None,
                    borders: engine::default_word_borders(),
                    shading: None,
                    content: vec![LayoutBlock::Paragraph(para)],
                    padding_left: 2.0,
                    padding_top: 0.0,
                    padding_right: 6.0,
                    padding_bottom: 0.0,
                    content_offset: 0,
                }
            };
            let row = layout::TableRowBox {
                origin: layout::Point { x: 0.0, y: 0.0 },
                size: layout::Size {
                    width: 300.0,
                    height: 30.0,
                },
                cells: vec![cell("a", 0.0), cell("b", 100.0), cell("c", 200.0)],
                header: false,
                cant_split: false,
                source_row: 0,
                exact_height: false,
            };
            let mut table = TableBox {
                origin: layout::Point { x: 0.0, y: 100.0 },
                size: layout::Size {
                    width: 300.0,
                    height: 30.0,
                },
                columns: vec![100.0; 3],
                rows: vec![row],
                outer_borders: engine::default_word_borders(),
                placement_dx: 0.0,
            };
            if mirrored {
                layout::mirror_bidi_visual(&mut table);
            }
            PageBox {
                size: Size {
                    width: 595.0,
                    height: 842.0,
                },
                margins: Margins::uniform(72.0),
                blocks: vec![LayoutBlock::Table(table)],
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
        let stack = liberation_stack();
        let fo = test_font_objs(&["liberation"]);
        let ltr = tm_xs(&build_content(&page(&stack, false), &fo, &stack));
        let rtl = tm_xs(&build_content(&page(&stack, true), &fo, &stack));
        assert_eq!(ltr.len(), 3, "{ltr:?}");
        assert_eq!(rtl.len(), 3, "{rtl:?}");
        assert!(ltr[0] < ltr[1] && ltr[1] < ltr[2], "LTR ascends: {ltr:?}");
        assert!(rtl[0] > rtl[1] && rtl[1] > rtl[2], "RTL descends: {rtl:?}");
        /* Cell 1's text starts at the mirrored cell's visual-left edge
        plus the swapped padding (its `end` margin, 6pt). */
        assert!((rtl[0] - (72.0 + 200.0 + 6.0)).abs() < 0.01, "{rtl:?}");
        assert!((ltr[0] - (72.0 + 2.0)).abs() < 0.01, "{ltr:?}");
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
            header_offset: 36.0,
            footer_offset: 36.0,
            footnotes: layout::NoteBand::default(),
            endnotes: layout::NoteBand::default(),
            hf_role: layout::HeaderRole::Default,
            page_number: 1,
            floats: Vec::new(),
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
    fn exports_pdfa2u_compliance_markers() {
        let stack = liberation_stack();
        let page = hello_page(&stack);

        let mut out = Vec::new();
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &["Hello world"],
            PdfProfile::A2u,
            &mut out,
        )
        .expect("export A2u");

        let has = |needle: &[u8]| out.windows(needle.len()).any(|w| w == needle);
        assert!(out.starts_with(b"%PDF-1.7"), "PDF/A-2 must declare PDF 1.7");
        assert!(has(b"/OutputIntent"), "missing output intent");
        assert!(
            has(b"GTS_PDFA1"),
            "missing PDF/A output intent subtype (GTS_PDFA1 covers parts 1-4)"
        );
        assert!(has(b"/DestOutputProfile"), "missing embedded ICC reference");
        assert!(has(b"acsp"), "ICC profile signature absent from stream");
        assert!(has(b"/Metadata"), "missing XMP metadata stream");
        assert!(has(b"pdfaid:part>2"), "missing pdfaid:part 2");
        assert!(has(b"pdfaid:conformance>U"), "missing pdfaid:conformance U");
        assert!(has(b"/ID"), "missing trailer document id");
        assert!(has(b"/FontFile2"), "font not embedded");
        assert!(
            has(b"/ToUnicode"),
            "level U requires the text-to-Unicode mapping"
        );
        assert!(out.ends_with(b"%%EOF"), "missing EOF marker");
    }

    #[test]
    fn a2u_export_is_byte_stable() {
        let stack = liberation_stack();
        let page = hello_page(&stack);
        let mut a = Vec::new();
        let mut b = Vec::new();
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &["Hello world"],
            PdfProfile::A2u,
            &mut a,
        )
        .expect("export a");
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &["Hello world"],
            PdfProfile::A2u,
            &mut b,
        )
        .expect("export b");
        assert_eq!(a, b, "A2u export must be byte-stable for identical input");
    }

    #[test]
    fn exports_pdfx3_markers() {
        let stack = liberation_stack();
        let page = hello_page(&stack);

        let mut out = Vec::new();
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &["Hello world"],
            PdfProfile::X3,
            &mut out,
        )
        .expect("export X3");

        let has = |needle: &[u8]| out.windows(needle.len()).any(|w| w == needle);
        assert!(
            out.starts_with(b"%PDF-1.4"),
            "PDF/X-3:2003 must declare PDF 1.4"
        );
        assert!(has(b"/OutputIntent"), "missing output intent");
        assert!(
            has(b"/S /GTS_PDFX"),
            "output intent subtype must be GTS_PDFX"
        );
        assert!(
            has(b"/OutputConditionIdentifier"),
            "missing output condition identifier"
        );
        assert!(has(b"/DestOutputProfile"), "missing embedded ICC reference");
        assert!(has(b"acsp"), "ICC profile signature absent from stream");
        assert!(
            has(b"GTS_PDFXVersion"),
            "missing GTS_PDFXVersion in the Info dictionary"
        );
        assert!(has(b"PDF/X-3:2003"), "missing PDF/X version string");
        assert!(has(b"/Title"), "X-3 requires /Title in the Info dictionary");
        assert!(has(b"/CreationDate"), "X-3 requires /CreationDate");
        assert!(has(b"/ModDate"), "X-3 requires /ModDate");
        assert!(
            has(b"/Trapped /False"),
            "X-3 requires an explicit trapped state"
        );
        assert!(has(b"/TrimBox"), "X-3 requires a TrimBox on every page");
        assert!(has(b"/ID"), "missing trailer document id");
        assert!(has(b"/FontFile2"), "font not embedded");
        assert!(
            has(b"pdfxid:GTS_PDFXVersion"),
            "XMP must mirror the Info dict's PDF/X version claim"
        );
        assert!(out.ends_with(b"%%EOF"), "missing EOF marker");
    }

    /// The X-3 TrimBox rides on every page and equals the MediaBox — the
    /// non-X profiles must not grow one.
    #[test]
    fn trim_box_is_x3_only() {
        let stack = liberation_stack();
        let page = hello_page(&stack);
        for profile in [PdfProfile::Plain, PdfProfile::A1b, PdfProfile::A2u] {
            let mut out = Vec::new();
            export_pdf(
                std::slice::from_ref(&page),
                &stack,
                &["Hello world"],
                profile,
                &mut out,
            )
            .expect("export");
            assert!(
                !out.windows(8).any(|w| w == b"/TrimBox"),
                "{profile:?} must not emit a TrimBox"
            );
        }
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
        let map = collect_to_unicode_pages(std::slice::from_ref(&page), &["Hello world"], &stack);
        let liberation = map.get("liberation").expect("liberation font mapped");
        let covered: HashSet<char> = liberation.values().flatten().copied().collect();
        for ch in "Helo wrd".chars() {
            assert!(covered.contains(&ch), "ToUnicode map missing {ch:?}");
        }
    }

    /* ================================================================
    Issue #144 — tab leaders (`<w:tab w:leader>`; TOC dot leaders) reach
    the exported PDF as real, extractable ink.
    ================================================================ */

    /// One line, a right tab stop near the end, optionally leadered —
    /// the TOC entry shape ("Entry" + tab + "1").
    fn page_with_tab_leader(stack: &FontStack, leader: Option<TabLeaderKind>) -> PageBox {
        let text = "Entry\t1";
        let mut para = layout_paragraph(ParagraphConfig {
            text,
            fonts: stack,
            spans: &[plain_span(text.len() as u32)],
            base_direction: ShapingDirection::Ltr,
            max_width: 300.0,
            line_height: 22.0,
            line_height_exact: false,
            alignment: Alignment::Start,
            indent_start_px: 0.0,
            indent_end_px: 0.0,
            first_line_indent_px: 0.0,
            hanging_indent_px: 0.0,
            marker_text: None,
            px_size_for_marker: 18.0,
            inline_objects: &[],
            tab_stops_px: &[(250.0, layout::paragraph::TabKind::Right, leader)],
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
            floats: Vec::new(),
        }
    }

    /// The dot-leader tiling in `emit_tab_leader_glyphs` must add exactly
    /// one more shown glyph each time the tab's advance grows by exactly
    /// one period's width — the invariant a correctly fixed-pitch tiler
    /// holds regardless of the exact padding/anchoring constants it uses
    /// internally. This is the "expected count" half of #144's acceptance
    /// (the text-extraction half is `tab_leader_dot_glyph_maps_to_unicode_dot`
    /// below).
    #[test]
    fn emit_tab_leader_glyphs_dot_count_scales_with_span() {
        let stack = liberation_stack();
        let face = stack.face("liberation").expect("liberation face");
        let px = 18.0_f32;
        let step = face
            .glyph_metrics('.', px)
            .expect("liberation must shape a period")
            .advance_width;
        assert!(step > 0.0);
        let font = LeaderFont {
            face,
            resource: "F0",
            px_size: px,
        };
        let count = |b: &[u8]| b.windows(4).filter(|w| w == b" Tj\n").count();

        let mut narrow = Content::new();
        assert!(emit_tab_leader_glyphs(
            &mut narrow,
            200.0,
            &font,
            '.',
            0.0,
            300.0,
            50.0
        ));
        let narrow_bytes = narrow.finish().to_vec();
        let base = count(&narrow_bytes);
        assert!(base > 0, "a 300 pt tab at 18 px must fit at least one dot");

        let mut wider = Content::new();
        assert!(emit_tab_leader_glyphs(
            &mut wider,
            200.0,
            &font,
            '.',
            0.0,
            300.0 + step,
            50.0
        ));
        let wider_bytes = wider.finish().to_vec();
        assert_eq!(
            count(&wider_bytes),
            base + 1,
            "widening the tab by exactly one glyph's advance must show exactly one more dot"
        );

        /* A tab narrower than the padding shapes nothing (but is not a
        missing-glyph failure — the caller must not fall back to a rule). */
        let mut empty = Content::new();
        assert!(emit_tab_leader_glyphs(
            &mut empty, 200.0, &font, '.', 0.0, 2.0, 50.0
        ));
        assert!(count(&empty.finish()) == 0);
    }

    /// A leadered right tab adds glyph shows to the content stream that a
    /// plain (unleadered) tab at the same stop does not — mirrors
    /// `list_marker_glyphs_reach_the_content_stream`'s technique. A
    /// document with no leader tabs is byte-for-byte unaffected (every
    /// pre-existing test in this module still passes unchanged).
    #[test]
    fn tab_leader_dot_glyphs_reach_the_content_stream() {
        let stack = liberation_stack();
        let leadered = page_with_tab_leader(&stack, Some(TabLeaderKind::Dot));
        let plain = page_with_tab_leader(&stack, None);
        let fo = test_font_objs(&["liberation"]);
        let shows = |c: &[u8]| c.windows(4).filter(|w| w == b" Tj\n").count();
        let with_leader = shows(&build_content(&leadered, &fo, &stack));
        let without = shows(&build_content(&plain, &fo, &stack));
        assert!(
            with_leader > without,
            "a leadered tab must add glyph shows: {with_leader} vs {without}"
        );
    }

    /// The leader glyph's id is synthesized on the fly (it never rides a
    /// layout-produced cluster), so `add_run_mappings` alone would leave
    /// it out of `/ToUnicode` — a viewer would then extract the dots as
    /// nothing. `collect_to_unicode_pages` must map it straight to `'.'`.
    #[test]
    fn tab_leader_dot_glyph_maps_to_unicode_dot() {
        let stack = liberation_stack();
        let page = page_with_tab_leader(&stack, Some(TabLeaderKind::Dot));
        let map = collect_to_unicode_pages(std::slice::from_ref(&page), &["Entry\t1"], &stack);
        let liberation = map.get("liberation").expect("liberation font mapped");
        let dot_gid = stack
            .face("liberation")
            .expect("liberation face")
            .glyph_id('.')
            .expect("liberation must shape a period");
        assert_eq!(
            liberation.get(&dot_gid).map(Vec::as_slice),
            Some(['.'].as_slice()),
            "the leader glyph id must decode to a literal '.' for text extraction"
        );
    }

    /// `Heavy` / `MiddleDot` leaders are Word's plain-rule kinds (see
    /// `leader_fill_char`): they must reach the content stream as fills
    /// (`re`/`f`), never as glyph shows, and must not disturb the
    /// unleadered baseline's show count.
    #[test]
    fn tab_leader_heavy_paints_a_rule_not_glyphs() {
        let stack = liberation_stack();
        let heavy = page_with_tab_leader(&stack, Some(TabLeaderKind::Heavy));
        let dot = page_with_tab_leader(&stack, Some(TabLeaderKind::Dot));
        let plain = page_with_tab_leader(&stack, None);
        let fo = test_font_objs(&["liberation"]);
        let heavy_bytes = build_content(&heavy, &fo, &stack);
        let dot_bytes = build_content(&dot, &fo, &stack);
        let plain_bytes = build_content(&plain, &fo, &stack);
        let shows = |c: &[u8]| c.windows(4).filter(|w| w == b" Tj\n").count();
        let fills = |c: &[u8]| c.windows(4).filter(|w| w == b" re\n").count();
        assert!(
            shows(&heavy_bytes) <= shows(&plain_bytes),
            "a heavy leader must not add glyph shows over the plain tab"
        );
        assert!(
            shows(&dot_bytes) > shows(&heavy_bytes),
            "a dot leader must add glyph shows a heavy leader does not"
        );
        assert!(
            fills(&heavy_bytes) > fills(&plain_bytes),
            "a heavy leader must paint a fill rule"
        );
    }

    /// List markers ("1.", "•") must reach the content stream as glyph
    /// shows — a numbered list exported without them silently loses the
    /// numbering.
    #[test]
    fn list_marker_glyphs_reach_the_content_stream() {
        let stack = liberation_stack();
        let marked = page_with(&stack, "item", &[plain_span(4)], Some("1."));
        let plain = page_with(&stack, "item", &[plain_span(4)], None);
        assert!(
            marked.blocks[0]
                .as_paragraph()
                .expect("paragraph block")
                .marker
                .is_some(),
            "layout must produce a marker box"
        );
        let fo = test_font_objs(&["liberation"]);
        let shows = |c: &[u8]| c.windows(4).filter(|w| w == b" Tj\n").count();
        let with_marker = shows(&build_content(&marked, &fo, &stack));
        let without = shows(&build_content(&plain, &fo, &stack));
        assert!(
            with_marker > without,
            "marker glyphs must add shows: {with_marker} vs {without}"
        );
    }

    /// The used-font collection must see the marker run — a marker-only
    /// paragraph (no line runs) still embeds its font.
    #[test]
    fn marker_font_reaches_the_embedded_set() {
        let stack = liberation_stack();
        let run = VisualRun {
            glyphs: vec![PositionedGlyph {
                id: 20,
                cluster: 0,
                x_advance: 10.0,
                y_advance: 0.0,
                x_offset: 0.0,
                y_offset: 0.0,
                synthetic: false,
                inline_image_rel_id: None,
                inline_footnote_marker: None,
                inline_note_anchor: None,
                inline_object_height: 0.0,
                float: None,
                leader: None,
            }],
            font: "liberation".to_string(),
            direction: ShapingDirection::Ltr,
            source_range: 0..0,
            attrs: TextAttrs {
                px_size: 18.0,
                color: [0, 0, 0, 255],
                faux_bold: false,
                faux_italic: false,
                underline: engine::UnderlineStyle::None,
                strike: false,
                bg_color: None,
                baseline_shift_px: 0.0,
            },
        };
        let para = ParagraphBox {
            origin: Point { x: 36.0, y: 0.0 },
            size: Size {
                width: 100.0,
                height: 26.0,
            },
            lines: vec![],
            direction: ShapingDirection::Ltr,
            marker: Some(MarkerBox {
                origin: Point { x: -20.0, y: 0.0 },
                baseline: 14.0,
                run,
                width: 10.0,
            }),
            source_paragraph_id: ParagraphBox::NO_SOURCE_ID,
            fields: vec![],
            page_break_after_line: vec![],
            borders: None,
            shading: None,
            keep_next: false,
            flow: layout::ParaFlow::default(),
            review_mark: None,
        };
        let page = PageBox {
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
            floats: Vec::new(),
        };
        let mut out = Vec::new();
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &[],
            PdfProfile::Plain,
            &mut out,
        )
        .expect("export marker-only paragraph");
        assert!(
            out.windows(10).any(|w| w == b"/FontFile2"),
            "marker run must contribute its font to the embedded set"
        );
    }

    /// Bold/italic on a regular-only stack resolve to faux synthesis; the
    /// PDF mirrors the canvas via `2 Tr` (fill+stroke) and a sheared Tm.
    #[test]
    fn faux_bold_italic_reach_the_content_stream() {
        let stack = liberation_stack();
        let mut span = plain_span(11);
        span.bold = true;
        span.italic = true;
        let page = page_with(&stack, "Hello world", &[span], None);
        let attrs = page.blocks[0]
            .as_paragraph()
            .expect("paragraph block")
            .lines[0]
            .runs[0]
            .attrs;
        assert!(
            attrs.faux_bold && attrs.faux_italic,
            "regular-only stack must resolve to faux synthesis"
        );
        let content = build_content(&page, &test_font_objs(&["liberation"]), &stack);
        assert!(
            find(&content, b"2 Tr\n").is_some(),
            "faux bold must set fill+stroke text rendering mode"
        );
        assert!(
            find(&content, b"0 Tr\n").is_some(),
            "text rendering mode must reset to fill after the run"
        );
        assert!(
            find(&content, b"1 0 0.22 1 ").is_some(),
            "faux italic must shear the text matrix"
        );
    }

    /// Highlight fills sit behind the `BT`/`ET` block; underline + strike
    /// fills follow it — the same layering as `render/scene.rs`.
    #[test]
    fn decorations_render_highlight_underline_strike() {
        let stack = liberation_stack();
        let mut span = plain_span(11);
        span.underline = engine::UnderlineStyle::Single;
        span.strike = true;
        span.bg_color = Some([0xFF, 0xEB, 0x78, 0xFF]);
        let page = page_with(&stack, "Hello world", &[span], None);
        let content = build_content(&page, &test_font_objs(&["liberation"]), &stack);
        let bt = find(&content, b"BT\n").expect("text block");
        let et = find(&content, b"ET\n").expect("text block end");
        let first_rect = find(&content, b" re\n").expect("highlight rect");
        assert!(
            first_rect < bt,
            "bg highlight must be filled before the text block"
        );
        assert!(
            find(&content[et..], b" re\n").is_some(),
            "underline/strike fills must follow the text block"
        );
        let rects = content.windows(4).filter(|w| w == b" re\n").count();
        assert!(
            rects >= 3,
            "highlight + underline + strike need three fills, got {rects}"
        );
    }

    /// `<w:vertAlign>` — a positive `baseline_shift_px` lifts the glyphs,
    /// i.e. a *larger* y in PDF's bottom-up space.
    #[test]
    fn baseline_shift_lifts_superscript_glyphs() {
        fn first_tm_y(content: &[u8]) -> f32 {
            let s = String::from_utf8_lossy(content);
            let tokens: Vec<&str> = s.split_whitespace().collect();
            let idx = tokens.iter().position(|t| *t == "Tm").expect("Tm op");
            tokens[idx - 1].parse().expect("Tm y operand")
        }
        let stack = liberation_stack();
        let mut shifted_span = plain_span(11);
        shifted_span.baseline_shift_px = 5.0;
        let base = page_with(&stack, "Hello world", &[plain_span(11)], None);
        let shifted = page_with(&stack, "Hello world", &[shifted_span], None);
        let fo = test_font_objs(&["liberation"]);
        let y_base = first_tm_y(&build_content(&base, &fo, &stack));
        let y_shift = first_tm_y(&build_content(&shifted, &fo, &stack));
        assert!(
            (y_shift - y_base - 5.0).abs() < 0.01,
            "positive shift must lift the glyph in PDF space: base {y_base}, shifted {y_shift}"
        );
    }

    /// `<w:pPr><w:shd>` fills behind the text; `<w:pBdr>` strokes after it.
    #[test]
    fn paragraph_shading_and_borders_are_emitted() {
        let stack = liberation_stack();
        let mut page = page_with(&stack, "Hello world", &[plain_span(11)], None);
        let LayoutBlock::Paragraph(para) = &mut page.blocks[0] else {
            unreachable!("fixture emits one paragraph block");
        };
        para.shading = Some([0xE8, 0xF0, 0xFF, 0xFF]);
        para.borders = Some(engine::default_word_borders());
        let content = build_content(&page, &test_font_objs(&["liberation"]), &stack);
        let bt = find(&content, b"BT\n").expect("text block");
        let rect = find(&content, b" re\n").expect("shading rect");
        assert!(rect < bt, "paragraph shading must fill behind the text");
        let et = find(&content, b"ET\n").expect("text block end");
        assert!(
            find(&content[et..], b"\nS").is_some(),
            "paragraph borders must stroke after the text"
        );
    }

    /* ================================================================
    Issue #71 — header/footer band + footnote emission.
    ================================================================ */

    #[test]
    fn header_and_footer_bands_reach_the_content_stream() {
        let stack = liberation_stack();
        let mut page = hello_page(&stack);
        /* Reuse the laid body paragraph as band content — same font,
        same glyph machinery. */
        let band_para = match &page.blocks[0] {
            LayoutBlock::Paragraph(p) => p.clone(),
            LayoutBlock::Table(_) => unreachable!(),
        };
        page.header = Some(layout::HeaderFooterBox {
            blocks: vec![LayoutBlock::Paragraph(band_para.clone())],
            source_rid: None,
        });
        page.footer = Some(layout::HeaderFooterBox {
            blocks: vec![LayoutBlock::Paragraph(band_para)],
            source_rid: None,
        });

        /* Baseline: the SAME page without bands paints fewer glyph
        show ops. Content streams are uncompressed for Plain, so count
        `Tj`-family show operators via the 2-byte glyph strings. */
        let mut with_bands = Vec::new();
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &["Hello world"],
            PdfProfile::Plain,
            &mut with_bands,
        )
        .expect("export with bands");

        let mut without = page.clone();
        without.header = None;
        without.footer = None;
        let mut plain = Vec::new();
        export_pdf(
            std::slice::from_ref(&without),
            &stack,
            &["Hello world"],
            PdfProfile::Plain,
            &mut plain,
        )
        .expect("export without bands");

        assert!(
            with_bands.len() > plain.len(),
            "band glyphs must add content-stream bytes: {} vs {}",
            with_bands.len(),
            plain.len()
        );
    }

    #[test]
    fn band_only_font_is_embedded() {
        /* Design review hazard — `show_run` silently draws nothing for
        an unembedded font. A font referenced ONLY by a band must
        reach the embedded set. */
        let stack = liberation_stack();
        let page_body_less = {
            let mut page = hello_page(&stack);
            /* Move the paragraph into the FOOTER; body goes empty. */
            let para = match page.blocks.remove(0) {
                LayoutBlock::Paragraph(p) => p,
                LayoutBlock::Table(_) => unreachable!(),
            };
            page.footer = Some(layout::HeaderFooterBox {
                blocks: vec![LayoutBlock::Paragraph(para)],
                source_rid: None,
            });
            page
        };
        let mut out = Vec::new();
        export_pdf(
            std::slice::from_ref(&page_body_less),
            &stack,
            &["Hello world"],
            PdfProfile::Plain,
            &mut out,
        )
        .expect("export");
        assert!(
            out.windows(10).any(|w| w == b"/FontFile2"),
            "footer-only font must be embedded"
        );
    }

    #[test]
    fn footnote_band_emits_its_separator_and_text() {
        let stack = liberation_stack();
        let mut page = hello_page(&stack);
        let note_para = match &page.blocks[0] {
            LayoutBlock::Paragraph(p) => p.clone(),
            LayoutBlock::Table(_) => unreachable!(),
        };
        page.footnotes = layout::NoteBand {
            entries: vec![layout::boxes::FootnoteEntry {
                id: 1,
                kind: engine::NoteKind::Footnote,
                marker: "1".into(),
                origin: layout::Point { x: 0.0, y: 0.0 },
                blocks: vec![LayoutBlock::Paragraph(note_para)],
                first_block_index: 0,
                continued_from_previous: false,
                continues_on_next: false,
            }],
            y: page.size.height - page.margins.bottom - 20.0,
            continuation: false,
        };
        let mut with_notes = Vec::new();
        export_pdf(
            std::slice::from_ref(&page),
            &stack,
            &["Hello world"],
            PdfProfile::Plain,
            &mut with_notes,
        )
        .expect("export");
        let mut without = page.clone();
        without.footnotes = layout::NoteBand::default();
        let mut plain = Vec::new();
        export_pdf(
            std::slice::from_ref(&without),
            &stack,
            &["Hello world"],
            PdfProfile::Plain,
            &mut plain,
        )
        .expect("export");
        assert!(
            with_notes.len() > plain.len(),
            "footnote band adds separator rule + glyphs"
        );
    }

    /// Issue #91 — a table taller than a page is split by the paginator
    /// into per-page fragments (header row repeated); the PDF painter
    /// must paint every continuation row exactly like the canvas path:
    /// each page's content stream carries one text matrix per glyph of
    /// every row on that page, header clone included, plus cell borders.
    #[test]
    fn split_table_continuation_rows_paint_on_every_page() {
        let stack = liberation_stack();
        let para = layout_paragraph(ParagraphConfig {
            text: "hi",
            fonts: &stack,
            spans: &[plain_span("hi".len() as u32)],
            base_direction: ShapingDirection::Ltr,
            max_width: 200.0,
            line_height: 22.0,
            line_height_exact: false,
            alignment: Alignment::Start,
            indent_start_px: 0.0,
            indent_end_px: 0.0,
            first_line_indent_px: 0.0,
            hanging_indent_px: 0.0,
            marker_text: None,
            px_size_for_marker: 22.0,
            inline_objects: &[],
            tab_stops_px: &[],
        });
        let glyphs_per_row: usize = para
            .lines
            .iter()
            .flat_map(|l| l.runs.iter())
            .map(|r| r.glyphs.len())
            .sum();
        assert!(glyphs_per_row > 0);
        let n_rows = 40usize;
        let rows: Vec<layout::TableRowBox> = (0..n_rows)
            .map(|i| layout::TableRowBox {
                origin: layout::Point {
                    x: 0.0,
                    y: i as f32 * 30.0,
                },
                size: layout::Size {
                    width: 200.0,
                    height: 30.0,
                },
                cells: vec![layout::TableCellBox {
                    origin: layout::Point { x: 0.0, y: 0.0 },
                    size: layout::Size {
                        width: 200.0,
                        height: 30.0,
                    },
                    grid_span: 1,
                    v_merge: engine::VMergeRole::None,
                    borders: engine::default_word_borders(),
                    shading: None,
                    content: vec![LayoutBlock::Paragraph(para.clone())],
                    padding_left: 0.0,
                    padding_top: 0.0,
                    padding_right: 0.0,
                    padding_bottom: 0.0,
                    content_offset: 0,
                }],
                header: i == 0,
                cant_split: false,
                source_row: i as u32,
                exact_height: false,
            })
            .collect();
        let table = TableBox {
            origin: layout::Point::default(),
            size: layout::Size {
                width: 200.0,
                height: n_rows as f32 * 30.0,
            },
            columns: vec![200.0],
            rows,
            outer_borders: engine::default_word_borders(),
            placement_dx: 0.0,
        };
        let geom = layout::PaginatePageGeometry {
            width: 595.0,
            height: 842.0,
            margins: Margins::uniform(72.0),
            header_offset: 36.0,
            footer_offset: 36.0,
        };
        let mut pag = layout::Paginator::with_default_bands(geom, None, None);
        pag.push_block(LayoutBlock::Table(table), 0.0, 0.0);
        let (pages, notes) = pag.finish_with_notes();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(pages.len(), 2, "1200 pt of rows on a 698 pt body");
        let fo = test_font_objs(&["liberation"]);
        let mut painted_body_rows = 0usize;
        for (i, page) in pages.iter().enumerate() {
            let rows: Vec<&layout::TableRowBox> = page
                .blocks
                .iter()
                .filter_map(LayoutBlock::as_table)
                .flat_map(|t| t.rows.iter())
                .collect();
            assert!(rows[0].header, "page {i} opens with the header row");
            painted_body_rows += rows.iter().filter(|r| !r.header).count();
            let content = build_content(page, &fo, &stack);
            let text = String::from_utf8_lossy(&content);
            let tms = text.split_whitespace().filter(|t| *t == "Tm").count();
            assert_eq!(
                tms,
                rows.len() * glyphs_per_row,
                "page {i}: every row (header clone included) paints its glyphs"
            );
            assert!(
                text.split_whitespace().any(|t| t == "S"),
                "page {i}: cell borders stroke"
            );
        }
        assert_eq!(painted_body_rows, n_rows - 1);
    }

    /// Issue #155 — a row that does not fit the rest of the page breaks
    /// at a line boundary: both parts of the row paint their own lines
    /// (none lost, none doubled) on their own page.
    #[test]
    fn row_split_at_a_line_boundary_paints_both_parts() {
        let stack = liberation_stack();
        let para = layout_paragraph(ParagraphConfig {
            text: "hi",
            fonts: &stack,
            spans: &[plain_span("hi".len() as u32)],
            base_direction: ShapingDirection::Ltr,
            max_width: 200.0,
            line_height: 22.0,
            line_height_exact: false,
            alignment: Alignment::Start,
            indent_start_px: 0.0,
            indent_end_px: 0.0,
            first_line_indent_px: 0.0,
            hanging_indent_px: 0.0,
            marker_text: None,
            px_size_for_marker: 22.0,
            inline_objects: &[],
            tab_stops_px: &[],
        });
        let glyphs: usize = para
            .lines
            .iter()
            .flat_map(|l| l.runs.iter())
            .map(|r| r.glyphs.len())
            .sum();
        assert!(glyphs > 0);
        let line_h = para.size.height;
        let row = |i: u32, n_paras: usize, h: f32| {
            let content: Vec<LayoutBlock> = (0..n_paras)
                .map(|k| {
                    let mut p = para.clone();
                    p.origin.y = k as f32 * line_h;
                    LayoutBlock::Paragraph(p)
                })
                .collect();
            layout::TableRowBox {
                origin: layout::Point::default(),
                size: layout::Size {
                    width: 200.0,
                    height: h,
                },
                cells: vec![layout::TableCellBox {
                    origin: layout::Point::default(),
                    size: layout::Size {
                        width: 200.0,
                        height: h,
                    },
                    grid_span: 1,
                    v_merge: engine::VMergeRole::None,
                    borders: engine::default_word_borders(),
                    shading: None,
                    content,
                    padding_left: 0.0,
                    padding_top: 0.0,
                    padding_right: 0.0,
                    padding_bottom: 0.0,
                    content_offset: 0,
                }],
                header: false,
                cant_split: false,
                source_row: i,
                exact_height: false,
            }
        };
        /* 698 pt body: a 650 pt first row leaves 48 pt — two of the
        second row's five lines stay, three continue. */
        let mut rows = vec![row(0, 1, 650.0), row(1, 5, 5.0 * line_h)];
        rows[1].origin.y = 650.0;
        let table = TableBox {
            origin: layout::Point::default(),
            size: layout::Size {
                width: 200.0,
                height: 650.0 + 5.0 * line_h,
            },
            columns: vec![200.0],
            rows,
            outer_borders: engine::default_word_borders(),
            placement_dx: 0.0,
        };
        let geom = layout::PaginatePageGeometry {
            width: 595.0,
            height: 842.0,
            margins: Margins::uniform(72.0),
            header_offset: 36.0,
            footer_offset: 36.0,
        };
        let mut pag = layout::Paginator::with_default_bands(geom, None, None);
        pag.push_block(LayoutBlock::Table(table), 0.0, 0.0);
        let (pages, notes) = pag.finish_with_notes();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(pages.len(), 2);
        let fo = test_font_objs(&["liberation"]);
        let per_page: Vec<usize> = pages
            .iter()
            .map(|page| {
                let content = build_content(page, &fo, &stack);
                let text = String::from_utf8_lossy(&content).into_owned();
                text.split_whitespace().filter(|t| *t == "Tm").count()
            })
            .collect();
        let lines_on = |p: &layout::PageBox| -> usize {
            p.blocks
                .iter()
                .filter_map(LayoutBlock::as_table)
                .flat_map(|t| t.rows.iter())
                .map(|r| r.cells[0].content.len())
                .sum()
        };
        assert_eq!(lines_on(&pages[0]), 3, "row 0 + two lines of row 1");
        assert_eq!(lines_on(&pages[1]), 3, "the other three lines");
        assert_eq!(per_page, vec![3 * glyphs, 3 * glyphs]);
    }
}
