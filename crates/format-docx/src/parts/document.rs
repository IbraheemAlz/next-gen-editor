//! `word/document.xml` — parse paragraphs + runs into a `DocumentTree`.
//!
//! Phase 3 additions:
//!
//! - **Style cascade.** A [`StyleResolver`] (Phase 3) folds doc defaults +
//!   `<w:basedOn>` chain + direct `<w:pPr>` / `<w:rPr>` into the engine's
//!   flat `ParaProperties` / `SpanStyle`. The engine never sees a
//!   `pStyle` / `rStyle` reference.
//! - **Source-byte capture.** Each `<w:p>` records its exact byte range
//!   in the source `document.xml`; the writer emits these bytes verbatim
//!   when the engine has not mutated the paragraph (the passthrough
//!   optimisation; zero document.xml drift on untouched paragraphs).

use crate::error::DocxError;
use crate::schema::ct_ppr::apply_ppr;
use crate::schema::ct_rpr::{apply_rpr, attr_val};
use crate::style_resolver::StyleResolver;
use engine::{DocumentTree, ListItem, ParaProperties, Paragraph, SpanStyle, StyleRun};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// Parse `word/document.xml` into paragraphs.
///
/// `resolver` folds the OOXML cascade so each paragraph / run hits the
/// engine as fully-resolved flat properties.
pub fn parse_document_xml(
    xml: &[u8],
    resolver: &StyleResolver<'_>,
) -> Result<DocumentTree, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);

    let mut paragraphs: Vec<Paragraph> = Vec::new();
    let mut para_text = String::new();
    let mut spans: Vec<StyleRun> = Vec::new();

    /* Per-paragraph parser state. */
    let mut p_style_id: Option<String> = None;
    let mut direct_ppr = ParaProperties::default();
    let mut pmark_rpr = SpanStyle::default();
    /* Phase 4 — `<w:numPr>/<w:numId>` + `<w:ilvl>` accumulators. We don't
    inherit either field from a paragraph style here; that's a separate
    cascade source Phase 4 ships without modelling. */
    let mut list_num_id: Option<u32> = None;
    let mut list_ilvl: Option<u8> = None;
    let mut in_num_pr = false;

    /* Per-run parser state. */
    let mut in_run = false;
    let mut in_rpr = false;
    let mut in_ppr = false;
    let mut in_text_elt = false;
    let mut r_style_id: Option<String> = None;
    let mut direct_rpr = SpanStyle::default();
    let mut run_text = String::new();

    /* Source-byte capture for the passthrough optimisation. `prev_pos` is
    the byte offset of the just-yielded event's end — equivalently the
    start of the next event we are about to read. */
    let mut prev_pos: usize = 0;
    let mut p_start_byte: Option<usize> = None;

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let name = e.name();
                match name.as_ref() {
                    b"w:p" => {
                        /* Capture the byte offset of the `<w:p` opening
                        delimiter — that's `prev_pos`, the position
                        *before* this event was read (= where the `<` of
                        `<w:p>` lives in the source). */
                        p_start_byte = Some(prev_pos);
                        p_style_id = None;
                        direct_ppr = ParaProperties::default();
                        pmark_rpr = SpanStyle::default();
                    }
                    b"w:r" => {
                        in_run = true;
                        r_style_id = None;
                        direct_rpr = SpanStyle::default();
                        run_text.clear();
                    }
                    b"w:rPr" => in_rpr = true,
                    /* A `<w:pPr>` only counts when it's the paragraph's own
                    properties — not a nested element under a `<w:r>`. */
                    b"w:pPr" if !in_run => in_ppr = true,
                    b"w:numPr" if in_ppr => in_num_pr = true,
                    b"w:t" => in_text_elt = true,
                    b"w:pStyle" if in_ppr => {
                        p_style_id = attr_val(&e, b"w:val");
                    }
                    b"w:rStyle" if in_run && in_rpr => {
                        r_style_id = attr_val(&e, b"w:val");
                    }
                    b"w:numId" if in_num_pr => {
                        list_num_id = attr_val(&e, b"w:val").and_then(|v| v.parse().ok());
                    }
                    b"w:ilvl" if in_num_pr => {
                        list_ilvl = attr_val(&e, b"w:val").and_then(|v| v.parse().ok());
                    }
                    n if in_run && in_rpr => apply_rpr(n, &e, &mut direct_rpr),
                    n if in_ppr && in_rpr => {
                        /* Paragraph-mark `<w:pPr>/<w:rPr>` — applies to the
                        ¶ glyph. We keep it as `pmark_rpr` for round-trip
                        and lay it under the run baseline below. */
                        apply_rpr(n, &e, &mut pmark_rpr);
                    }
                    n if in_ppr && !in_rpr && !in_num_pr => {
                        apply_ppr(n, &e, &mut direct_ppr);
                    }
                    _ => {}
                }
            }
            Event::Empty(e) => {
                let name = e.name();
                match name.as_ref() {
                    b"w:pStyle" if in_ppr => {
                        p_style_id = attr_val(&e, b"w:val");
                    }
                    b"w:rStyle" if in_run && in_rpr => {
                        r_style_id = attr_val(&e, b"w:val");
                    }
                    b"w:numId" if in_num_pr => {
                        list_num_id = attr_val(&e, b"w:val").and_then(|v| v.parse().ok());
                    }
                    b"w:ilvl" if in_num_pr => {
                        list_ilvl = attr_val(&e, b"w:val").and_then(|v| v.parse().ok());
                    }
                    n if in_run && in_rpr => apply_rpr(n, &e, &mut direct_rpr),
                    n if in_ppr && in_rpr => apply_rpr(n, &e, &mut pmark_rpr),
                    n if in_ppr && !in_rpr && !in_num_pr => {
                        apply_ppr(n, &e, &mut direct_ppr);
                    }
                    _ => {}
                }
            }
            Event::Text(t) if in_text_elt => {
                run_text.push_str(&t.unescape()?);
            }
            Event::End(e) => match e.name().as_ref() {
                b"w:t" => in_text_elt = false,
                b"w:rPr" => in_rpr = false,
                b"w:pPr" => in_ppr = false,
                b"w:numPr" => in_num_pr = false,
                b"w:r" => {
                    in_run = false;
                    let start = para_text.len() as u32;
                    para_text.push_str(&run_text);
                    let end = para_text.len() as u32;
                    if start == end {
                        continue;
                    }
                    /* Run cascade: paragraph baseline (doc defaults + pStyle
                    chain) + paragraph-mark rPr is already baked into the
                    baseline below per-paragraph; here we only fold the
                    character style chain + direct rPr. */
                    let (_, baseline) = resolver.resolve_paragraph(
                        p_style_id.as_deref(),
                        direct_ppr.clone(),
                        pmark_rpr,
                    );
                    let style = resolver.resolve_run(baseline, r_style_id.as_deref(), direct_rpr);
                    if style != SpanStyle::default() {
                        match spans.last_mut() {
                            Some(last) if last.end == start && last.style == style => {
                                last.end = end;
                            }
                            _ => spans.push(StyleRun { start, end, style }),
                        }
                    }
                }
                b"w:p" => {
                    /* Read position after `</w:p>` — that's where the `>`
                    closes — gives us the end byte. */
                    let p_end_byte = reader.buffer_position() as usize;
                    let source_xml = p_start_byte
                        .take()
                        .filter(|&s| s < p_end_byte && p_end_byte <= xml.len())
                        .map(|s| xml[s..p_end_byte].to_vec());

                    /* Paragraph cascade: bake direct_ppr on top of doc
                    defaults + pStyle chain. The baseline rPr we computed
                    per-run is informational here. */
                    let (props, _) = resolver.resolve_paragraph(
                        p_style_id.take().as_deref(),
                        std::mem::take(&mut direct_ppr),
                        std::mem::take(&mut pmark_rpr),
                    );
                    /* Compose `ListItem` from the per-paragraph numPr
                    accumulators; partial refs (numId without ilvl, or
                    vice versa) default the missing field to 0 — Word
                    treats absent `<w:ilvl>` as level 0. */
                    let list_item = match (list_num_id.take(), list_ilvl.take()) {
                        (Some(num_id), ilvl) => Some(ListItem {
                            num_id,
                            ilvl: ilvl.unwrap_or(0),
                        }),
                        (None, _) => None,
                    };
                    paragraphs.push(Paragraph {
                        text: std::mem::take(&mut para_text),
                        spans: std::mem::take(&mut spans),
                        props,
                        list_item,
                        /* Resolver fills this in a second pass once the full
                        doc order is known (see `opc::archive::read_docx`). */
                        resolved_marker: None,
                        dirty: false,
                        source_xml,
                    });
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        prev_pos = reader.buffer_position() as usize;
        buf.clear();
    }

    Ok(DocumentTree::from_rich_paragraphs(paragraphs))
}
