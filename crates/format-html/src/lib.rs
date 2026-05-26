//! `format-html` — strict `DocumentTree` -> standalone HTML serializer.
//!
//! Sprint 9 deliverable. The rich-clipboard HTML in `engine::html` is the
//! clipboard payload (`data-rel-id` pointers, no `dir`, no rowspan). This
//! crate is the **file-export** path: a free-standing HTML5 document with
//! `<p dir="ltr|rtl">` per paragraph, table cells emitted with proper
//! `colspan` / `rowspan` from `grid_span` / `VMergeRole`, and inline
//! images carried inline as `src="data:image/...;base64,..."` from
//! `DocumentTree.media` so the file opens by itself.
//!
//! BiDi is preserved per-paragraph (UAX #9 runs per line — never flatten
//! across line breaks). Plain-text fallback lives in
//! `engine::DocumentTree::to_plain_text` as a sibling deliverable.

use engine::{
    Block, BorderStyle, CellBorders, CellProperties, DocumentTree, FontFamily, InlineKind,
    InlineObject, Paragraph, SpanStyle, StyleRun, Table, TextDirection, UnderlineStyle, VMergeRole,
};

/// Top-level entry point — render the whole `DocumentTree` as a
/// standalone HTML5 document. UTF-8 encoded, with a `<meta>` declaring
/// the encoding so a downstream tool that re-opens the file picks the
/// right codec. Body wraps the blocks in a `dir="auto"` container so a
/// per-paragraph `dir` override always wins.
pub fn to_html(doc: &DocumentTree) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\
         <title>Document</title></head><body dir=\"auto\">",
    );
    emit_blocks(&doc.blocks_slice(), &doc.media, &mut out);
    out.push_str("</body></html>");
    out
}

/// Render just the block sequence (no `<html>` envelope) — useful for
/// embedding a document fragment into an existing host page. Identical
/// rules as `to_html` for paragraphs, tables, and inline images.
pub fn to_html_fragment(doc: &DocumentTree) -> String {
    let mut out = String::with_capacity(512);
    emit_blocks(&doc.blocks_slice(), &doc.media, &mut out);
    out
}

/* --------------------------------------------------------------------- */
/* Block dispatch                                                        */
/* --------------------------------------------------------------------- */

type Media = std::collections::HashMap<String, engine::ImageBlob>;

fn emit_blocks(blocks: &[Block], media: &Media, out: &mut String) {
    for b in blocks {
        match b {
            Block::Paragraph(p) => emit_paragraph(p, media, out),
            Block::Table(t) => emit_table(t, media, out),
        }
    }
}

/* --------------------------------------------------------------------- */
/* Paragraph                                                             */
/* --------------------------------------------------------------------- */

fn dir_attr(dir: Option<TextDirection>) -> &'static str {
    match dir {
        Some(TextDirection::Rtl) => " dir=\"rtl\"",
        Some(TextDirection::Ltr) => " dir=\"ltr\"",
        None => "",
    }
}

fn emit_paragraph(p: &Paragraph, media: &Media, out: &mut String) {
    out.push_str("<p");
    out.push_str(dir_attr(p.props.direction));
    out.push('>');
    /* Walk paragraph spans + inline-object anchors in document order.
    Each inline object owns one OBJECT REPLACEMENT CHARACTER in `text`
    (UTF-8 3 bytes); we skip those bytes and emit the object instead. */
    let text_len = p.text.len();
    let mut cursor: usize = 0;
    let mut obj_idx: usize = 0;
    for run in &p.spans {
        let (rs, re) = (run.start as usize, run.end as usize);
        if rs > cursor {
            emit_text_segment(
                p,
                media,
                &mut obj_idx,
                cursor,
                rs,
                &SpanStyle::default(),
                out,
            );
        }
        if re > rs {
            emit_text_segment(p, media, &mut obj_idx, rs, re, &run.style, out);
        }
        cursor = re;
    }
    if cursor < text_len {
        emit_text_segment(
            p,
            media,
            &mut obj_idx,
            cursor,
            text_len,
            &SpanStyle::default(),
            out,
        );
    }
    out.push_str("</p>");
}

/// Emit `p.text[lo..hi]` styled by `style`, interleaving any inline
/// objects anchored within that range. Each object's anchor is a
/// 3-byte U+FFFC; we split the text emit around it.
fn emit_text_segment(
    p: &Paragraph,
    media: &Media,
    obj_idx: &mut usize,
    lo: usize,
    hi: usize,
    style: &SpanStyle,
    out: &mut String,
) {
    let mut start = lo;
    while *obj_idx < p.inline_objects.len() {
        let obj = &p.inline_objects[*obj_idx];
        let at = obj.at as usize;
        if at < start {
            *obj_idx += 1;
            continue;
        }
        if at >= hi {
            break;
        }
        if at > start {
            emit_styled_run(&p.text[start..at], style, out);
        }
        emit_inline_object(obj, media, out);
        start = at.saturating_add(3).min(hi);
        *obj_idx += 1;
    }
    if start < hi {
        emit_styled_run(&p.text[start..hi], style, out);
    }
}

/* --------------------------------------------------------------------- */
/* Styled run                                                            */
/* --------------------------------------------------------------------- */

fn emit_styled_run(text: &str, style: &SpanStyle, out: &mut String) {
    if text.is_empty() {
        return;
    }
    let css = build_inline_style(style);
    let open_span = !css.is_empty();
    if open_span {
        out.push_str("<span style=\"");
        out.push_str(&css);
        out.push_str("\">");
    }
    /* Emit decoration tags innermost so the visual nesting (b > i > u > s)
    matches the typical browser collapse. */
    let mut closers: Vec<&str> = Vec::new();
    if style.bold == Some(true) {
        out.push_str("<b>");
        closers.push("</b>");
    }
    if style.italic == Some(true) {
        out.push_str("<i>");
        closers.push("</i>");
    }
    if style.underline.is_some_and(UnderlineStyle::is_visible) {
        out.push_str("<u>");
        closers.push("</u>");
    }
    if style.strike == Some(true) {
        out.push_str("<s>");
        closers.push("</s>");
    }
    escape_text_into(text, out);
    for tag in closers.into_iter().rev() {
        out.push_str(tag);
    }
    if open_span {
        out.push_str("</span>");
    }
}

fn build_inline_style(s: &SpanStyle) -> String {
    let mut css = String::new();
    if let Some(c) = s.color {
        css.push_str(&format!("color:{};", color_to_hex(c)));
    }
    if let Some(c) = s.bg_color {
        css.push_str(&format!("background-color:{};", color_to_hex(c)));
    }
    if let Some(fam) = s.font_family {
        css.push_str(&format!("font-family:{};", family_css_name(fam)));
    } else if let Some(name) = s.raw_font_family.as_deref() {
        /* Raw font name: a `<w:rFonts w:ascii>` value the engine could not
        resolve to a loaded face. Quote so spaces/specials are safe. */
        css.push_str("font-family:\"");
        escape_attr_into(name, &mut css);
        css.push_str("\";");
    }
    if let Some(px) = s.font_size {
        css.push_str(&format!("font-size:{px}px;"));
    }
    css
}

fn family_css_name(f: FontFamily) -> &'static str {
    match f {
        FontFamily::Amiri => "Amiri",
        FontFamily::LiberationSans => "Liberation Sans",
        FontFamily::NotoNaskhArabic => "Noto Naskh Arabic",
    }
}

fn color_to_hex(c: [u8; 4]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/* --------------------------------------------------------------------- */
/* Inline objects                                                        */
/* --------------------------------------------------------------------- */

fn emit_inline_object(obj: &InlineObject, media: &Media, out: &mut String) {
    match &obj.kind {
        InlineKind::Image {
            rel_id,
            width_emu,
            height_emu,
        } => emit_image(rel_id, *width_emu, *height_emu, media, out),
        InlineKind::FootnoteRef { display_number, .. } => {
            out.push_str("<sup>");
            out.push_str(&display_number.to_string());
            out.push_str("</sup>");
        }
    }
}

fn emit_image(rel_id: &str, width_emu: i64, height_emu: i64, media: &Media, out: &mut String) {
    let w_px = emu_to_css_px(width_emu);
    let h_px = emu_to_css_px(height_emu);
    out.push_str("<img alt=\"\"");
    if w_px > 0 {
        out.push_str(&format!(" width=\"{w_px}\""));
    }
    if h_px > 0 {
        out.push_str(&format!(" height=\"{h_px}\""));
    }
    out.push_str(" src=\"");
    match media.get(rel_id) {
        Some(blob) if !blob.data.is_empty() => {
            out.push_str("data:");
            escape_attr_into(&blob.content_type, out);
            out.push_str(";base64,");
            base64_encode_into(&blob.data, out);
        }
        _ => {
            /* No blob — emit a placeholder rel ref so the export is not
            silently lossy; a downstream pipeline can swap in real bytes. */
            out.push_str("cid:");
            escape_attr_into(rel_id, out);
        }
    }
    out.push_str("\"/>");
}

fn emu_to_css_px(emu: i64) -> i64 {
    /* 1 EMU = 1/9525 CSS px at 96 DPI; round to nearest. */
    if emu <= 0 {
        return 0;
    }
    (emu + 4762) / 9525
}

/* --------------------------------------------------------------------- */
/* Tables                                                                */
/* --------------------------------------------------------------------- */

fn emit_table(t: &Table, media: &Media, out: &mut String) {
    out.push_str("<table style=\"border-collapse:collapse;\">");
    /* Pre-compute, for every (row, cell) emit-decision, the rowspan that
    a `VMergeRole::Restart` cell should carry. A Restart cell consumes
    every directly-following `Continue` cell occupying the SAME logical
    column position. Engine table model stores Continue cells in-place,
    so they share the same `cells[c]` index as the Restart owner. */
    let row_count = t.rows.len();
    for (r, row) in t.rows.iter().enumerate() {
        out.push_str("<tr>");
        for (c, cell) in row.cells.iter().enumerate() {
            match cell.props.v_merge {
                VMergeRole::Continue => {
                    /* Owned by a Restart above — emit nothing. */
                    continue;
                }
                VMergeRole::Restart => {
                    let span = continue_run_below(t, r, c);
                    emit_table_cell(cell, media, span, out);
                }
                VMergeRole::None => emit_table_cell(cell, media, 1, out),
            }
        }
        out.push_str("</tr>");
        let _ = (row_count, row);
    }
    out.push_str("</table>");
}

/// Count consecutive rows below `r` whose cell at column `c` carries
/// `VMergeRole::Continue`. The returned rowspan is `1 + count`.
fn continue_run_below(t: &Table, r: usize, c: usize) -> u32 {
    let mut span: u32 = 1;
    let mut i = r + 1;
    while i < t.rows.len() {
        match t.rows[i].cells.get(c) {
            Some(cell) if cell.props.v_merge == VMergeRole::Continue => {
                span += 1;
                i += 1;
            }
            _ => break,
        }
    }
    span
}

fn emit_table_cell(cell: &engine::TableCell, media: &Media, rowspan: u32, out: &mut String) {
    out.push_str("<td");
    let colspan = u32::from(cell.props.grid_span.max(1));
    if colspan > 1 {
        out.push_str(&format!(" colspan=\"{colspan}\""));
    }
    if rowspan > 1 {
        out.push_str(&format!(" rowspan=\"{rowspan}\""));
    }
    let style = cell_inline_style(&cell.props);
    if !style.is_empty() {
        out.push_str(" style=\"");
        out.push_str(&style);
        out.push('"');
    }
    out.push('>');
    emit_blocks(&cell.blocks, media, out);
    out.push_str("</td>");
}

fn cell_inline_style(props: &CellProperties) -> String {
    let mut css = String::new();
    if let Some(shade) = props.shading {
        css.push_str(&format!("background-color:{};", color_to_hex(shade)));
    }
    if let Some(borders) = &props.borders {
        emit_border_edges(borders, &mut css);
    }
    css
}

fn emit_border_edges(b: &CellBorders, css: &mut String) {
    for (edge, name) in [
        (&b.top, "top"),
        (&b.left, "left"),
        (&b.bottom, "bottom"),
        (&b.right, "right"),
    ] {
        if let Some(stroke) = edge {
            let style_token = border_css_style(&stroke.style);
            if style_token == "none" {
                css.push_str(&format!("border-{name}:none;"));
                continue;
            }
            let px = (f32::from(stroke.size_eighth_pt)) / 6.0;
            let color = stroke
                .color
                .map(color_to_hex)
                .unwrap_or_else(|| "#000000".to_string());
            css.push_str(&format!("border-{name}:{px}px {style_token} {color};"));
        }
    }
}

fn border_css_style(style: &BorderStyle) -> &'static str {
    match style {
        BorderStyle::Single => "solid",
        BorderStyle::Double => "double",
        BorderStyle::Dotted => "dotted",
        BorderStyle::Dashed => "dashed",
        BorderStyle::None => "none",
        BorderStyle::Other(_) => "solid",
    }
}

/* --------------------------------------------------------------------- */
/* Text/attribute escaping                                               */
/* --------------------------------------------------------------------- */

fn escape_text_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

fn escape_attr_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

/* --------------------------------------------------------------------- */
/* Base64 (RFC 4648, MIME variant — no line wrapping)                    */
/* --------------------------------------------------------------------- */

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode_into(bytes: &[u8], out: &mut String) {
    out.reserve(bytes.len().div_ceil(3) * 4);
    let mut chunks = bytes.chunks_exact(3);
    for ch in chunks.by_ref() {
        let n = (u32::from(ch[0]) << 16) | (u32::from(ch[1]) << 8) | u32::from(ch[2]);
        out.push(B64[((n >> 18) & 0x3f) as usize] as char);
        out.push(B64[((n >> 12) & 0x3f) as usize] as char);
        out.push(B64[((n >> 6) & 0x3f) as usize] as char);
        out.push(B64[(n & 0x3f) as usize] as char);
    }
    let rem = chunks.remainder();
    match rem.len() {
        1 => {
            let n = u32::from(rem[0]) << 16;
            out.push(B64[((n >> 18) & 0x3f) as usize] as char);
            out.push(B64[((n >> 12) & 0x3f) as usize] as char);
            out.push_str("==");
        }
        2 => {
            let n = (u32::from(rem[0]) << 16) | (u32::from(rem[1]) << 8);
            out.push(B64[((n >> 18) & 0x3f) as usize] as char);
            out.push(B64[((n >> 12) & 0x3f) as usize] as char);
            out.push(B64[((n >> 6) & 0x3f) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
}

/* --------------------------------------------------------------------- */
/* Small accessor extension on `DocumentTree`                            */
/*                                                                       */
/* The engine model stores top-level blocks in an `im::Vector<Block>`.   */
/* Walking that for read-only emit wants a `&[Block]` slice; the trait   */
/* extension keeps that local to this crate without bloating the engine. */
/* --------------------------------------------------------------------- */

trait DocBlocksSlice {
    fn blocks_slice(&self) -> Vec<Block>;
}

impl DocBlocksSlice for DocumentTree {
    fn blocks_slice(&self) -> Vec<Block> {
        /* `im::Vector` is structurally shared; `iter().cloned()` is the
        documented way to materialise a `Vec`. Blocks themselves are
        cheap to clone — paragraphs hold `Vec<StyleRun>` etc. but the
        export path is one-shot, off the editing critical path. */
        self.blocks.iter().cloned().collect()
    }
}

/* Make `StyleRun` reachable for tests through the public re-export
trail; not used by the emit logic above but pulled in via tests. */
#[allow(dead_code)]
fn _ensure_use_of_style_run(_r: &StyleRun) {}

/* ===================================================================== */
/* Tests                                                                 */
/* ===================================================================== */

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{
        Block, BorderStroke, BorderStyle, CellBorders, CellProperties, DocumentTree, FontFamily,
        InlineKind, InlineObject, ParaProperties, Paragraph, SpanStyle, StyleRun, Table, TableCell,
        TableProperties, TableRow, TextDirection, VMergeRole,
    };

    fn doc_with(blocks: Vec<Block>) -> DocumentTree {
        let mut doc = DocumentTree::default();
        for b in blocks {
            doc.blocks.push_back(b);
        }
        doc
    }

    /* ---- empty document ------------------------------------------------ */

    #[test]
    fn empty_document_emits_envelope_only() {
        let html = to_html(&DocumentTree::default());
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<body dir=\"auto\"></body>"));
        assert!(html.trim_end().ends_with("</html>"));
    }

    #[test]
    fn empty_fragment_is_empty_string() {
        assert_eq!(to_html_fragment(&DocumentTree::default()), "");
    }

    /* ---- mixed-style paragraph ----------------------------------------- */

    #[test]
    fn mixed_style_paragraph_emits_decorations_and_inline_style() {
        let p = Paragraph {
            text: "Hello bold!".into(),
            spans: vec![StyleRun {
                start: 6,
                end: 10,
                style: SpanStyle {
                    bold: Some(true),
                    color: Some([0xff, 0x00, 0x00, 0xff]),
                    font_size: Some(14.0),
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let html = to_html_fragment(&doc_with(vec![Block::Paragraph(p)]));
        /* Default-styled prefix "Hello ", then styled "bold", then "!". */
        assert!(html.starts_with("<p>Hello "), "got: {html}");
        assert!(
            html.contains("<span style=\"color:#ff0000;font-size:14px;\"><b>bold</b></span>"),
            "got: {html}"
        );
        assert!(html.ends_with("!</p>"));
    }

    #[test]
    fn paragraph_escapes_html_specials() {
        let p = Paragraph {
            text: "a < b & c > d".into(),
            ..Default::default()
        };
        let html = to_html_fragment(&doc_with(vec![Block::Paragraph(p)]));
        assert_eq!(html, "<p>a &lt; b &amp; c &gt; d</p>");
    }

    /* ---- 2 x 2 table --------------------------------------------------- */

    fn cell_with_text(s: &str) -> TableCell {
        TableCell {
            props: CellProperties::default(),
            blocks: vec![Block::Paragraph(Paragraph {
                text: s.into(),
                ..Default::default()
            })],
        }
    }

    #[test]
    fn two_by_two_table_with_shaded_cell_and_top_border() {
        let mut top_left = cell_with_text("TL");
        top_left.props.shading = Some([0xff, 0xff, 0x99, 0xff]);
        top_left.props.borders = Some(CellBorders {
            top: Some(BorderStroke {
                style: BorderStyle::Single,
                size_eighth_pt: 8, // 1 pt
                color: Some([0, 0, 0, 0xff]),
            }),
            ..Default::default()
        });
        let table = Table {
            grid: vec![6765, 6765],
            props: TableProperties::default(),
            rows: vec![
                TableRow {
                    props: Default::default(),
                    cells: vec![top_left, cell_with_text("TR")],
                },
                TableRow {
                    props: Default::default(),
                    cells: vec![cell_with_text("BL"), cell_with_text("BR")],
                },
            ],
            dirty: true,
            source_xml: None,
        };
        let html = to_html_fragment(&doc_with(vec![Block::Table(table)]));
        assert!(html.starts_with("<table style=\"border-collapse:collapse;\">"));
        assert_eq!(html.matches("<tr>").count(), 2);
        assert_eq!(html.matches("<td").count(), 4);
        assert!(
            html.contains("background-color:#ffff99;"),
            "shading css missing in: {html}"
        );
        assert!(
            html.contains("border-top:") && html.contains("solid #000000"),
            "border css missing in: {html}"
        );
        assert!(html.contains("<p>TL</p>"));
        assert!(html.contains("<p>BR</p>"));
    }

    /* ---- BiDi: Arabic + English paragraph emits dir="rtl" ------------- */

    #[test]
    fn rtl_paragraph_emits_dir_rtl() {
        let p = Paragraph {
            text: "مرحبا hello".into(),
            props: ParaProperties {
                direction: Some(TextDirection::Rtl),
                ..Default::default()
            },
            ..Default::default()
        };
        let html = to_html_fragment(&doc_with(vec![Block::Paragraph(p)]));
        assert!(
            html.starts_with("<p dir=\"rtl\">"),
            "RTL paragraph missing dir attr: {html}"
        );
        assert!(html.contains("مرحبا hello"));
    }

    #[test]
    fn ltr_paragraph_emits_dir_ltr_when_set_explicitly() {
        let p = Paragraph {
            text: "abc".into(),
            props: ParaProperties {
                direction: Some(TextDirection::Ltr),
                ..Default::default()
            },
            ..Default::default()
        };
        let html = to_html_fragment(&doc_with(vec![Block::Paragraph(p)]));
        assert!(html.starts_with("<p dir=\"ltr\">"));
    }

    /* ---- inline image: data: URI from media table ---------------------- */

    #[test]
    fn paragraph_with_inline_image_emits_data_uri() {
        let mut media: std::collections::HashMap<String, engine::ImageBlob> = Default::default();
        /* 4 raw bytes — base64 = "AAECAw==". */
        media.insert(
            "rId7".into(),
            engine::ImageBlob {
                content_type: "image/png".into(),
                data: vec![0, 1, 2, 3],
            },
        );
        let p = Paragraph {
            text: "x\u{FFFC}y".into(),
            inline_objects: vec![InlineObject {
                at: 1, /* byte offset of U+FFFC inside "x\u{FFFC}y" */
                kind: InlineKind::Image {
                    rel_id: "rId7".into(),
                    width_emu: 1_905_000,
                    height_emu: 1_524_000,
                },
            }],
            ..Default::default()
        };
        let mut doc = doc_with(vec![Block::Paragraph(p)]);
        doc.media = media;
        let html = to_html_fragment(&doc);
        assert!(
            html.contains("src=\"data:image/png;base64,AAECAw==\""),
            "expected data URI in: {html}"
        );
        assert!(html.contains("<p>x"), "expected text before image: {html}");
        assert!(html.contains("y</p>"), "expected text after image: {html}");
        assert!(html.contains("width=\"200\""));
        assert!(html.contains("height=\"160\""));
    }

    #[test]
    fn image_without_media_blob_falls_back_to_cid_pointer() {
        let p = Paragraph {
            text: "\u{FFFC}".into(),
            inline_objects: vec![InlineObject {
                at: 0,
                kind: InlineKind::Image {
                    rel_id: "rIdMissing".into(),
                    width_emu: 0,
                    height_emu: 0,
                },
            }],
            ..Default::default()
        };
        let html = to_html_fragment(&doc_with(vec![Block::Paragraph(p)]));
        /* No blob -> the placeholder src ensures we never silently drop
        the image; downstream pipelines can rewrite the cid: pointer. */
        assert!(
            html.contains("src=\"cid:rIdMissing\""),
            "expected cid fallback: {html}"
        );
    }

    /* ---- vMerge -> rowspan; grid_span -> colspan ---------------------- */

    #[test]
    fn vmerge_restart_collects_rowspan_skips_continue() {
        let mut tl = cell_with_text("Owner");
        tl.props.v_merge = VMergeRole::Restart;
        let mut cont = cell_with_text(""); /* ignored at render */
        cont.props.v_merge = VMergeRole::Continue;
        let mut wide = cell_with_text("wide");
        wide.props.grid_span = 2;
        let table = Table {
            grid: vec![3000, 3000],
            props: TableProperties::default(),
            rows: vec![
                TableRow {
                    props: Default::default(),
                    cells: vec![tl, cell_with_text("R0C1")],
                },
                TableRow {
                    props: Default::default(),
                    cells: vec![cont, cell_with_text("R1C1")],
                },
                TableRow {
                    props: Default::default(),
                    cells: vec![wide],
                },
            ],
            dirty: true,
            source_xml: None,
        };
        let html = to_html_fragment(&doc_with(vec![Block::Table(table)]));
        assert!(
            html.contains("rowspan=\"2\""),
            "owner rowspan missing: {html}"
        );
        assert!(
            html.contains("colspan=\"2\""),
            "wide cell colspan missing: {html}"
        );
        /* The Continue cell renders nothing of its own — only the owner
        cell's contents appear. Crude check: "Owner" appears exactly once. */
        assert_eq!(html.matches("Owner").count(), 1);
        assert!(html.contains("R0C1"));
        assert!(html.contains("R1C1"));
        assert!(html.contains("wide"));
    }

    /* ---- font family + raw_font_family ---- */

    #[test]
    fn font_family_emits_resolved_or_raw_name() {
        let resolved = Paragraph {
            text: "a".into(),
            spans: vec![StyleRun {
                start: 0,
                end: 1,
                style: SpanStyle {
                    font_family: Some(FontFamily::Amiri),
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let raw = Paragraph {
            text: "b".into(),
            spans: vec![StyleRun {
                start: 0,
                end: 1,
                style: SpanStyle {
                    raw_font_family: Some("Cambria".into()),
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let html = to_html_fragment(&doc_with(vec![
            Block::Paragraph(resolved),
            Block::Paragraph(raw),
        ]));
        assert!(html.contains("font-family:Amiri;"));
        assert!(html.contains("font-family:\"Cambria\";"));
    }
}
