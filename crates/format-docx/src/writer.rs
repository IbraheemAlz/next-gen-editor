//! `.docx` writer: serialize `DocumentTree` → `word/document.xml`, repack
//! into a ZIP archive. When called via `write_docx(archive, doc)`, the
//! original archive's other entries are written back verbatim; only
//! `word/document.xml` is regenerated.

use crate::error::DocxError;
use crate::opc::archive::{
    COMMENTS_EXTENDED_XML, COMMENTS_XML, DOC_XML, DocxArchive, NUMBERING_XML, RELS_XML,
};
use crate::parts::comments::{
    build_comments_extended_xml, build_comments_extended_xml_with_overrides, build_comments_xml,
};
use crate::parts::numbering::build_numbering_xml;
use engine::{
    Alignment, Block, BorderStroke, BorderStyle, CellBorders, CellWidth, DocumentTree, Field,
    FontFamily, InlineKind, InlineObject, LineHeight, ParaProperties, Paragraph, Revision,
    RevisionKind, RowHeight, SpanStyle, Table, TableCell, TableRow, TextDirection, UnderlineStyle,
    VMergeRole,
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Cursor, Write};
use zip::write::{SimpleFileOptions, ZipWriter};

/// Standard OOXML document namespace boilerplate (matches what Word emits).
const DOC_XML_HEADER: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    "\n",
    r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
    "<w:body>",
);
/// Header used for documents that carry inline drawings (images). Word
/// requires DrawingML, WordprocessingDrawing, picture, and relationships
/// namespaces declared at the document root before any `<w:drawing>`
/// child element references them. The image-free header stays the
/// minimal form so plain text round-trips byte-stable.
const DOC_XML_HEADER_WITH_DRAWING: &str = concat!(
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
    "\n",
    r#"<w:document "#,
    r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" "#,
    r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
    r#"xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" "#,
    r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
    r#"xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">"#,
    "<w:body>",
);
const DOC_XML_FOOTER: &str = "<w:sectPr/></w:body></w:document>";

/// `true` when any paragraph in the doc (including those inside table
/// cells) carries an inline image — the writer picks the expanded
/// namespace header so the emitted `<w:drawing>` resolves.
fn doc_has_inline_images(doc: &DocumentTree) -> bool {
    fn any_image_in_blocks(blocks: &[Block]) -> bool {
        blocks.iter().any(|b| match b {
            Block::Paragraph(p) => paragraph_has_image(p),
            Block::Table(t) => t.rows.iter().any(|row| {
                row.cells
                    .iter()
                    .any(|cell| any_image_in_blocks(&cell.blocks))
            }),
        })
    }
    fn paragraph_has_image(p: &Paragraph) -> bool {
        p.inline_objects
            .iter()
            .any(|o| matches!(o.kind, InlineKind::Image { .. }))
    }
    doc.blocks.iter().any(|b| match b {
        Block::Paragraph(p) => paragraph_has_image(p),
        Block::Table(t) => t.rows.iter().any(|row| {
            row.cells
                .iter()
                .any(|cell| any_image_in_blocks(&cell.blocks))
        }),
    })
}

fn family_docx_name(f: &FontFamily) -> &str {
    f.display_name()
}

/// XML-escape character data (`&`, `<`, `>`) into `out`. Quotes don't matter
/// inside character data.
fn push_escaped(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

/// Emit a `<w:rPr>` block for `style`, or nothing when it is the default.
/// Children follow the CT_RPr schema order (rFonts, b, i, strike, color, u,
/// shd). A background colour is written as `<w:shd w:fill>` — that carries
/// arbitrary hex, where `<w:highlight>` is limited to a named palette.
fn emit_rpr(style: &SpanStyle, out: &mut String) {
    if *style == SpanStyle::default() {
        return;
    }
    out.push_str("<w:rPr>");
    /* Audit gap A.M2 — `<w:rFonts>` round-trips a known FontFamily,
    a verbatim raw name, and/or a theme binding. The reader splits
    the source's `w:ascii` either into `font_family` (recognised) or
    `raw_font_family` (verbatim string); the theme attrs park in
    `font_theme`. Emit whichever slots are populated. */
    let rfonts_name: Option<String> = style
        .font_family
        .as_ref()
        .map(|f| family_docx_name(f).to_string())
        .or_else(|| style.raw_font_family.clone());
    if rfonts_name.is_some() || style.font_theme.is_some() {
        out.push_str("<w:rFonts");
        if let Some(n) = rfonts_name.as_deref() {
            out.push_str(" w:ascii=\"");
            push_escaped_attr(n, out);
            out.push_str("\" w:hAnsi=\"");
            push_escaped_attr(n, out);
            out.push_str("\" w:cs=\"");
            push_escaped_attr(n, out);
            out.push('"');
        }
        if let Some(t) = style.font_theme.as_deref() {
            out.push_str(" w:asciiTheme=\"");
            push_escaped_attr(t, out);
            out.push_str("\" w:hAnsiTheme=\"");
            push_escaped_attr(t, out);
            out.push_str("\" w:cstheme=\"");
            push_escaped_attr(t, out);
            out.push('"');
        }
        out.push_str("/>");
    }
    if style.bold == Some(true) {
        out.push_str("<w:b/>");
    }
    if style.italic == Some(true) {
        out.push_str("<w:i/>");
    }
    /* CT_RPr ordering — caps/smallCaps sit between `<w:iCs/>` and
    `<w:strike/>` (OOXML §17.3.2). When both are on, Word's writer
    emits both; the reader's `apply_rpr` flips both flags and the
    shape-time transform prefers `caps` (full-height) over `smallCaps`. */
    if style.caps == Some(true) {
        out.push_str("<w:caps/>");
    }
    if style.small_caps == Some(true) {
        out.push_str("<w:smallCaps/>");
    }
    if style.strike == Some(true) {
        out.push_str("<w:strike/>");
    }
    if let Some([r, g, b, _]) = style.color {
        out.push_str(&format!("<w:color w:val=\"{r:02X}{g:02X}{b:02X}\"/>"));
    }
    /* `<w:sz>` / `<w:szCs>` — Word's half-point encoding; round to nearest.
    Emit both elements so ASCII + complex-script runs (Arabic, Hebrew,
    Thai) both pick up the size. Word's own writer always pairs them. */
    if let Some(pt) = style.font_size {
        let half_pts = (pt * 2.0).round().max(2.0) as u32;
        out.push_str(&format!("<w:sz w:val=\"{half_pts}\"/>"));
        out.push_str(&format!("<w:szCs w:val=\"{half_pts}\"/>"));
    }
    /* `<w:u w:val="…"/>` — emit only when the variant is visible. The
    explicit `none` round-trips when the engine carries it (overrides an
    inherited underline); a sticky `Some(None)` is rare but legal. */
    if let Some(u) = style.underline {
        let v = match u {
            UnderlineStyle::None => "none",
            UnderlineStyle::Single => "single",
            UnderlineStyle::Double => "double",
            UnderlineStyle::Dotted => "dotted",
            UnderlineStyle::Dashed => "dash",
            UnderlineStyle::Wavy => "wave",
        };
        out.push_str(&format!("<w:u w:val=\"{v}\"/>"));
    }
    if let Some([r, g, b, _]) = style.bg_color {
        out.push_str(&format!(
            "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{r:02X}{g:02X}{b:02X}\"/>"
        ));
    }
    /* Audit gap A.M1 — `<w:vertAlign>` after shading per OOXML §17.3.2.42
    ordering. Skip when the resolved value is the implicit baseline (the
    spec lets us omit it for noise reduction; explicit `Baseline` only
    appears when defeating an inherited super/subscript, in which case
    `<w:vertAlign w:val="baseline"/>` IS emitted). */
    if let Some(va) = style.vert_align {
        let v = match va {
            engine::VertAlign::Baseline => "baseline",
            engine::VertAlign::Superscript => "superscript",
            engine::VertAlign::Subscript => "subscript",
        };
        out.push_str(&format!("<w:vertAlign w:val=\"{v}\"/>"));
    }
    out.push_str("</w:rPr>");
}

/// Serialize one run: `<w:r>[<w:rPr>…]<w:t xml:space="preserve">…</w:t></w:r>`.
/// `xml:space="preserve"` keeps leading/trailing whitespace; matters for
/// Arabic trailing-shadda etc.
fn serialize_run(text: &str, style: &SpanStyle, out: &mut String) {
    serialize_run_kind(text, style, false, out);
}

/// `serialize_run`, with a `delete_kind` flag that swaps `<w:t>` for
/// `<w:delText>` when the run sits inside a `<w:del>` wrapper. OOXML
/// requires the renamed element so the deleted bytes don't display as
/// live content in readers that strip tracked-change markup.
fn serialize_run_kind(text: &str, style: &SpanStyle, delete_kind: bool, out: &mut String) {
    let tag = if delete_kind { "w:delText" } else { "w:t" };
    out.push_str("<w:r>");
    emit_rpr(style, out);
    out.push_str(&format!("<{tag} xml:space=\"preserve\">"));
    push_escaped(text, out);
    out.push_str(&format!("</{tag}></w:r>"));
}

/// Escape `&` `<` `>` `"` for an XML attribute value. Quotes matter here
/// (unlike character data) because attribute values are quote-delimited.
fn push_escaped_attr(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

/// Emit the opening tag of a `<w:ins>` / `<w:del>` wrapper. `id` defaults
/// to the writer-assigned fallback when the engine model carries none —
/// Word requires `w:id` on every wrapper, the value must be unique within
/// the document, but is otherwise opaque.
fn emit_revision_open(rev: &Revision, fallback_id: u32, out: &mut String) {
    let tag = match rev.kind {
        RevisionKind::Insert => "w:ins",
        RevisionKind::Delete => "w:del",
        /* FormatChange should never reach the text-wrap path — caller
        filters it. Defensive default to ins so a stray entry never
        produces malformed XML. */
        RevisionKind::FormatChange => "w:ins",
    };
    let id = rev.id.unwrap_or(fallback_id);
    out.push_str(&format!("<{tag} w:id=\"{id}\""));
    if !rev.author.is_empty() {
        out.push_str(" w:author=\"");
        push_escaped_attr(&rev.author, out);
        out.push('"');
    }
    if !rev.date.is_empty() {
        out.push_str(" w:date=\"");
        push_escaped_attr(&rev.date, out);
        out.push('"');
    }
    out.push('>');
}

fn emit_revision_close(kind: RevisionKind, out: &mut String) {
    let tag = match kind {
        RevisionKind::Insert => "w:ins",
        RevisionKind::Delete => "w:del",
        RevisionKind::FormatChange => "w:ins",
    };
    out.push_str(&format!("</{tag}>"));
}

/// `<w:jc w:val="…"/>` token for an `Alignment`. Word emits writing-direction-
/// relative `start` / `end` in modern docs; we match that.
fn jc_val(a: Alignment) -> &'static str {
    match a {
        Alignment::Start => "start",
        Alignment::End => "end",
        Alignment::Center => "center",
        Alignment::Justify => "both",
    }
}

/// Emit `<w:pPr>` for `props`, or nothing when it is the default — a default
/// `ParaProperties` must produce an empty pPr to keep the Phase 1 plain
/// fixtures byte-stable. Children follow the CT_PPr schema order: keepNext,
/// keepLines, pageBreakBefore, spacing, shd, ind, jc, bidi.
///
/// Sprint 12 (#11) — `style_id` is `Some` when the paragraph references a
/// `<w:style>` entry. Emitted as the FIRST `<w:pPr>` child per OOXML
/// schema order (CT_PPrBase: `<w:pStyle>` before everything else).
fn emit_ppr(
    props: &ParaProperties,
    style_id: Option<&str>,
    list_item: Option<engine::ListItem>,
    out: &mut String,
) {
    if *props == ParaProperties::default() && style_id.is_none() && list_item.is_none() {
        return;
    }
    out.push_str("<w:pPr>");
    if let Some(id) = style_id {
        out.push_str("<w:pStyle w:val=\"");
        /* Style ids are XML names — no `<`/`>`/`&`/`"` per OOXML — but
        defensively escape so a malformed id can't break the file. */
        for ch in id.chars() {
            match ch {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                other => out.push(other),
            }
        }
        out.push_str("\"/>");
    }
    if *props == ParaProperties::default() {
        /* Style-only paragraph: open + style + close. The structured
        emit body below will skip every per-field block (all defaults). */
    }
    if props.keep_next {
        out.push_str("<w:keepNext/>");
    }
    if props.keep_lines {
        out.push_str("<w:keepLines/>");
    }
    if props.page_break_before {
        out.push_str("<w:pageBreakBefore/>");
    }
    /* Audit gap A.M4 — `<w:pBdr>` paragraph borders. CT_PPr ordering
    places pBdr between pageBreakBefore and spacing. Each edge with a
    populated stroke emits as a child; absent edges silently omit. */
    if let Some(b) = props.borders.as_ref() {
        out.push_str("<w:pBdr>");
        emit_border_edge("w:top", &b.top, out);
        emit_border_edge("w:left", &b.left, out);
        emit_border_edge("w:bottom", &b.bottom, out);
        emit_border_edge("w:right", &b.right, out);
        emit_border_edge("w:between", &b.inside_h, out);
        out.push_str("</w:pBdr>");
    }
    /* Audit gap A.M3 — `<w:tabs>` custom stops, child order preserved.
    Empty list ⇒ no element. `Clear` kind round-trips so a user-defined
    clear-of-an-inherited-stop survives a save. */
    if !props.tab_stops.is_empty() {
        out.push_str("<w:tabs>");
        for stop in &props.tab_stops {
            let val = match stop.kind {
                engine::TabKind::Left => "left",
                engine::TabKind::Center => "center",
                engine::TabKind::Right => "right",
                engine::TabKind::Decimal => "decimal",
                engine::TabKind::Clear => "clear",
            };
            let pos_twips = (stop.position_pt * 20.0).round() as i32;
            out.push_str(&format!("<w:tab w:val=\"{val}\" w:pos=\"{pos_twips}\"/>"));
        }
        out.push_str("</w:tabs>");
    }
    /* `<w:spacing>` carries both before/after gaps and the line rule. We
    omit the element entirely when none of its attributes are set. */
    let has_gap = props.spacing.before_twips != 0 || props.spacing.after_twips != 0;
    if has_gap || props.line_height.is_some() {
        out.push_str("<w:spacing");
        if props.spacing.before_twips != 0 {
            out.push_str(&format!(" w:before=\"{}\"", props.spacing.before_twips));
        }
        if props.spacing.after_twips != 0 {
            out.push_str(&format!(" w:after=\"{}\"", props.spacing.after_twips));
        }
        if let Some(lh) = props.line_height {
            let (line, rule) = match lh {
                LineHeight::Auto { twips } => (twips, "auto"),
                LineHeight::Exact { twips } => (twips, "exact"),
                LineHeight::AtLeast { twips } => (twips, "atLeast"),
            };
            out.push_str(&format!(" w:line=\"{line}\" w:lineRule=\"{rule}\""));
        }
        out.push_str("/>");
    }
    /* Paragraph shading mirrors the rPr / tcPr `<w:shd>` emitters: the
    arbitrary RGB rides `w:fill` with a `clear` pattern; alpha has no
    OOXML slot. */
    if let Some([r, g, b, _]) = props.shading {
        out.push_str(&format!(
            "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{r:02X}{g:02X}{b:02X}\"/>"
        ));
    }
    let ind = &props.indent;
    if ind.start_twips != 0
        || ind.end_twips != 0
        || ind.first_line_twips != 0
        || ind.hanging_twips != 0
    {
        out.push_str("<w:ind");
        if ind.start_twips != 0 {
            out.push_str(&format!(" w:start=\"{}\"", ind.start_twips));
        }
        if ind.end_twips != 0 {
            out.push_str(&format!(" w:end=\"{}\"", ind.end_twips));
        }
        if ind.first_line_twips != 0 {
            out.push_str(&format!(" w:firstLine=\"{}\"", ind.first_line_twips));
        }
        if ind.hanging_twips != 0 {
            out.push_str(&format!(" w:hanging=\"{}\"", ind.hanging_twips));
        }
        out.push_str("/>");
    }
    if let Some(a) = props.alignment {
        out.push_str(&format!("<w:jc w:val=\"{}\"/>", jc_val(a)));
    }
    if let Some(d) = props.direction {
        match d {
            TextDirection::Rtl => out.push_str("<w:bidi/>"),
            /* LTR is the default; we still emit `<w:bidi w:val="false"/>` so
            an explicit user override round-trips faithfully. */
            TextDirection::Ltr => out.push_str("<w:bidi w:val=\"false\"/>"),
        }
    }
    /* Sprint 13 (#12) — `<w:numPr>` carries list membership; emitted
    last so it sits at the tail of `<w:pPr>` (Word's canonical
    placement; CT_PPrBase's `numPr` follows the other body fields). */
    if let Some(li) = list_item {
        out.push_str(&format!(
            "<w:numPr><w:ilvl w:val=\"{}\"/><w:numId w:val=\"{}\"/></w:numPr>",
            li.ilvl, li.num_id
        ));
    }
    out.push_str("</w:pPr>");
}

/// Serialize one paragraph. A span-free paragraph with default `props` emits
/// a single default run — byte-identical to the pre-Phase-2 writer, so the
/// round-trip harness's plain fixtures see no drift. A styled paragraph
/// emits one `<w:r>` per maximal (range, style) segment, the default-styled
/// gaps included.
fn serialize_paragraph(para: &Paragraph, out: &mut String) {
    out.push_str("<w:p>");
    emit_ppr(&para.props, para.style_id.as_deref(), para.list_item, out);
    /* `<w:br>` (Phase 2 audit, gap A.12) lives as U+2028 / U+000C in
    `para.text`; the structural `<w:r><w:br/></w:r>` emission needs
    the cut-point walk. The fast path stays open for plain
    paragraphs that carry no overlays AND no break characters. */
    let has_break = para
        .text
        .chars()
        .any(|c| c == '\u{2028}' || c == '\u{000C}');
    if para.spans.is_empty()
        && para.inline_objects.is_empty()
        && para.revisions.is_empty()
        && para.fields.is_empty()
        && !has_break
    {
        serialize_run(&para.text, &SpanStyle::default(), out);
    } else {
        emit_styled_runs_with_objects(para, out);
    }
    out.push_str("</w:p>");
}

/// Walk the paragraph as a single ordered pass weaving three orthogonal
/// overlays:
///
/// 1. **Style spans** — each contiguous byte range carries a `SpanStyle`
///    that becomes the run's `<w:rPr>`.
/// 2. **Inline objects** — anchored at a single U+FFFC byte; emit the
///    object's own run shape (`<w:drawing>`, `<w:footnoteReference>`, …)
///    in place of the anchor character.
/// 3. **Revisions** — `<w:ins>` / `<w:del>` wrappers covering byte
///    ranges that group one-or-more `<w:r>` children. When a run sits
///    inside a `<w:del>`, its `<w:t>` switches to `<w:delText>`.
///
/// **Emission algorithm.** All three overlays contribute *cut points*
/// into a sorted, deduped position set. The walk advances one segment
/// `[lo, hi)` at a time. Before emitting a segment we sync the open
/// revision stack to position `lo`:
///
/// - Pop (close) any revision whose `end <= lo`. Stack discipline
///   guarantees innermost-first close order, so the emitted XML is
///   well-formed for properly-nested input (reader builds the input
///   via its own balanced stack, so this invariant holds in practice).
/// - Push (open) any revision whose `start <= lo && end > lo` that is
///   not yet on the stack. Multiple revisions opening at the same
///   position are sorted `(start ASC, end DESC)` so the outermost
///   opens first — keeps `<w:ins><w:del>…</w:del></w:ins>` nesting
///   visually consistent with the reader's stack order.
///
/// After sync, the segment emits exactly one element: an inline object
/// (when `lo` is anchor-aligned) or a single `<w:r>` for `text[lo..hi)`
/// at the style resolved at `lo`. The `in_del` flag — true when *any*
/// `Delete` revision is currently on the stack — switches the `<w:t>`
/// element to `<w:delText>`.
///
/// At end-of-text every still-open revision flushes its close in
/// stack-LIFO order.
fn emit_styled_runs_with_objects(para: &Paragraph, out: &mut String) {
    let len = para.text.len();

    /* Cut-point set: paragraph bounds, every span edge, every inline
    object anchor + its 3-byte trailing edge, every revision boundary.
    BTreeSet sorts + dedups in one pass. */
    let mut cuts: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    cuts.insert(0);
    cuts.insert(len);
    for s in &para.spans {
        cuts.insert((s.start as usize).min(len));
        cuts.insert((s.end as usize).min(len));
    }
    for o in &para.inline_objects {
        let at = (o.at as usize).min(len);
        cuts.insert(at);
        cuts.insert((at + OBJECT_REPLACE_UTF8.len()).min(len));
    }
    for r in &para.revisions {
        cuts.insert((r.start as usize).min(len));
        cuts.insert((r.end as usize).min(len));
    }
    for f in &para.fields {
        cuts.insert((f.start as usize).min(len));
        cuts.insert((f.end as usize).min(len));
    }
    /* Phase 2 audit (gap A.12) — `<w:br>` round-trip. Reader maps
    `<w:br/>` to U+2028 LINE SEPARATOR and `<w:br w:type="page"/>` to
    U+000C FORM FEED in `para.text`. Both characters are mandatory-
    break per UAX-14, so the layout shaper already line-breaks at
    them. The writer emits a dedicated `<w:r><w:br/></w:r>` /
    `<w:r><w:br w:type="page"/></w:r>` run at each occurrence so the
    re-saved file uses Word's structural form, not the bare Unicode
    character (which Word would render as a "missing-glyph" box). */
    let mut break_at: std::collections::HashMap<usize, BreakKind> =
        std::collections::HashMap::new();
    /* Audit gap A.M5 — `<w:tab/>` round-trip. Reader stashes the literal
    U+0009 byte at every tab anchor; the writer reinjects the structural
    `<w:r><w:tab/></w:r>` run at the same offset. Tabs sit alongside
    `<w:br>` in the cut set because the same boundary mechanic applies. */
    let mut tab_at: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (idx, ch) in para.text.char_indices() {
        let kind = match ch {
            '\u{2028}' => Some(BreakKind::Line),
            '\u{000C}' => Some(BreakKind::Page),
            _ => None,
        };
        if let Some(k) = kind {
            break_at.insert(idx, k);
            cuts.insert(idx);
            cuts.insert(idx + ch.len_utf8());
        }
        if ch == '\u{0009}' {
            tab_at.insert(idx);
            cuts.insert(idx);
            cuts.insert(idx + ch.len_utf8());
        }
    }
    let cuts: Vec<usize> = cuts.into_iter().collect();

    /* Revisions sorted for stable open-order at any shared start.
    Sprint 14 (#14) — only `Insert` / `Delete` wrap text with
    `<w:ins>` / `<w:del>`; `FormatChange` rides on `<w:rPr>` via
    `<w:rPrChange>` (emitted by `serialize_run`'s rPr block, NOT
    here), so the text-wrap stack must filter it out. */
    let mut sorted_revs: Vec<&Revision> = para
        .revisions
        .iter()
        .filter(|r| matches!(r.kind, RevisionKind::Insert | RevisionKind::Delete))
        .collect();
    sorted_revs.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));

    /* Fields sorted the same way. Nesting model: revisions wrap fields
    (i.e. `<w:ins><w:r><w:fldChar/></w:r></w:ins>`), so the field
    open emits *after* the revision open at any shared boundary and
    the field close emits *before* the revision close. The two stacks
    are independent — interleaving by open / close order in the walk
    naturally produces well-formed XML. */
    let mut sorted_fields: Vec<&Field> = para.fields.iter().collect();
    sorted_fields.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));

    /* Inline object lookup by anchor byte. */
    let obj_at: std::collections::HashMap<usize, &InlineObject> = para
        .inline_objects
        .iter()
        .map(|o| (o.at as usize, o))
        .collect();

    let style_at = |off: usize| -> SpanStyle {
        para.spans
            .iter()
            .find(|s| off >= s.start as usize && off < s.end as usize)
            .map(|s| s.style.clone())
            .unwrap_or_default()
    };

    let mut rev_stack: Vec<&Revision> = Vec::new();
    let mut field_stack: Vec<&Field> = Vec::new();
    let mut next_fallback_id: u32 = 1;

    for win in cuts.windows(2) {
        let (lo, hi) = (win[0], win[1]);
        if lo >= hi {
            continue;
        }

        /* Close any fields ending at or before `lo` first — field
        wrappers nest *inside* revision wrappers, so the field's `end`
        fldChar run emits before the surrounding `</w:ins>` close. */
        while let Some(top) = field_stack.last() {
            if (top.end as usize) <= lo {
                emit_field_epilogue(out);
                field_stack.pop();
            } else {
                break;
            }
        }
        /* Close any revisions ending at or before `lo`. Stack LIFO order
        emits inner closes first, preserving well-formed nesting. */
        while let Some(top) = rev_stack.last() {
            if (top.end as usize) <= lo {
                emit_revision_close(top.kind, out);
                rev_stack.pop();
            } else {
                break;
            }
        }
        /* Open any revisions that should be active at `lo` but are not
        on the stack yet. `sorted_revs` is in outermost-first order. */
        for r in &sorted_revs {
            let rs = r.start as usize;
            let re = r.end as usize;
            if rs <= lo
                && re > lo
                && !rev_stack
                    .iter()
                    .any(|x| std::ptr::eq(*x as *const _, *r as *const _))
            {
                let fallback = next_fallback_id;
                next_fallback_id += 1;
                emit_revision_open(r, fallback, out);
                rev_stack.push(r);
            }
        }
        /* Open fields *after* revisions so the field prologue lives
        inside the enclosing `<w:ins>` / `<w:del>` wrapper. */
        let in_del = rev_stack.iter().any(|r| r.kind == RevisionKind::Delete);
        for f in &sorted_fields {
            let fs = f.start as usize;
            let fe = f.end as usize;
            if fs <= lo
                && fe > lo
                && !field_stack
                    .iter()
                    .any(|x| std::ptr::eq(*x as *const _, *f as *const _))
            {
                emit_field_prologue(&f.instruction, in_del, out);
                field_stack.push(f);
            }
        }

        if let Some(obj) = obj_at.get(&lo) {
            emit_inline_object(obj, out);
            /* Skip to the byte after the anchor's UTF-8 length — the
            object consumes the full anchor character. The cut set
            already placed a boundary at `lo + OBJECT_REPLACE_UTF8.len()`,
            so the next window picks up from there naturally. */
        } else if let Some(&kind) = break_at.get(&lo) {
            /* `<w:br/>` / `<w:br w:type="page"/>` runs replace the
            U+2028 / U+000C character — emit the structural element so
            Word doesn't render the bare Unicode char as a tofu box. */
            emit_br_run(kind, out);
        } else if tab_at.contains(&lo) {
            /* Audit gap A.M5 — emit the structural `<w:tab/>` element
            instead of a literal HT byte; the rPr applies to the tab
            run so an inherited bold/italic style still survives. */
            emit_tab_run(&style_at(lo), in_del, out);
        } else {
            serialize_run_kind(&para.text[lo..hi], &style_at(lo), in_del, out);
        }
    }

    /* Drain whatever is still open. Field epilogues fire before
    revision closes (field wrappers nest inside revision wrappers). */
    while let Some(_top) = field_stack.pop() {
        emit_field_epilogue(out);
    }
    while let Some(top) = rev_stack.pop() {
        emit_revision_close(top.kind, out);
    }
}

/// Emit the complex-field prologue runs — the begin fldChar, the
/// `<w:instrText>` carrier, and the separate fldChar. `delete_kind`
/// switches the inner element to `<w:delText>` when the field sits
/// inside a `<w:del>` wrapper (rare but legal — a reviewer can mark a
/// field for deletion). Each component lives in its own `<w:r>` because
/// Word emits it that way and round-tripping a file with the prologue
/// fused into one run would visibly drift on byte-stable harnesses.
fn emit_field_prologue(instruction: &str, delete_kind: bool, out: &mut String) {
    out.push_str("<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>");
    let tag = if delete_kind {
        "w:delInstrText"
    } else {
        "w:instrText"
    };
    out.push_str(&format!("<w:r><{tag} xml:space=\"preserve\">"));
    push_escaped(instruction, out);
    out.push_str(&format!("</{tag}></w:r>"));
    out.push_str("<w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>");
}

/// Emit the complex-field epilogue — the end fldChar run.
fn emit_field_epilogue(out: &mut String) {
    out.push_str("<w:r><w:fldChar w:fldCharType=\"end\"/></w:r>");
}

/// Phase 2 audit (gap A.12) — `<w:br>` variants. Reader maps
/// `<w:br/>` (default or `w:type="textWrapping"`) to U+2028 LINE
/// SEPARATOR and `<w:br w:type="page"/>` to U+000C FORM FEED.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BreakKind {
    Line,
    Page,
}

fn emit_br_run(kind: BreakKind, out: &mut String) {
    match kind {
        BreakKind::Line => out.push_str("<w:r><w:br/></w:r>"),
        BreakKind::Page => out.push_str("<w:r><w:br w:type=\"page\"/></w:r>"),
    }
}

/// Audit gap A.M5 — `<w:tab/>` round-trip. Emitted as its own `<w:r>`
/// so the surrounding rPr applies (bold/italic on a tab is legal and
/// Word emits it that way). `delete_kind` flips the wrapping element
/// when the tab sits inside a `<w:del>` block — the tab element itself
/// has no body so it does not switch tag names, only the enclosing
/// run picks up `<w:delText>` semantics for any neighbouring text.
fn emit_tab_run(style: &SpanStyle, _delete_kind: bool, out: &mut String) {
    out.push_str("<w:r>");
    emit_rpr(style, out);
    out.push_str("<w:tab/></w:r>");
}

/// UTF-8 encoding of U+FFFC OBJECT REPLACEMENT CHARACTER (the byte
/// anchor for `InlineObject` in a paragraph's `text`).
const OBJECT_REPLACE_UTF8: &str = "\u{FFFC}";

fn emit_inline_object(obj: &InlineObject, out: &mut String) {
    match &obj.kind {
        InlineKind::Image {
            rel_id,
            width_emu,
            height_emu,
        } => emit_image_drawing(rel_id, *width_emu, *height_emu, out),
        InlineKind::FootnoteRef {
            id,
            display_number: _,
        } => {
            /* Phase 8a footnote reference — round-trip via the engine's
            existing `<w:footnoteReference>` emission. */
            out.push_str(&format!(
                "<w:r><w:rPr><w:vertAlign w:val=\"superscript\"/></w:rPr>\
                 <w:footnoteReference w:id=\"{id}\"/></w:r>"
            ));
        }
    }
}

/// Emit the DrawingML inline-picture run for an image. Element order
/// matches Microsoft Word's own output exactly — the OOXML schema is
/// CT_Inline-strict: `extent` precedes `effectExtent` precedes `docPr`
/// precedes `cNvGraphicFramePr` precedes `graphic`. Re-ordering any
/// pair makes Word reject the file with the "unreadable content"
/// recovery prompt.
fn emit_image_drawing(rel_id: &str, width_emu: i64, height_emu: i64, out: &mut String) {
    let cx = width_emu.max(1);
    let cy = height_emu.max(1);
    out.push_str("<w:r><w:drawing>");
    out.push_str(&format!(
        "<wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\">\
         <wp:extent cx=\"{cx}\" cy=\"{cy}\"/>\
         <wp:effectExtent l=\"0\" t=\"0\" r=\"0\" b=\"0\"/>\
         <wp:docPr id=\"1\" name=\"Picture\"/>\
         <wp:cNvGraphicFramePr/>\
         <a:graphic>\
         <a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\">\
         <pic:pic>\
         <pic:nvPicPr><pic:cNvPr id=\"0\" name=\"Image\"/><pic:cNvPicPr/></pic:nvPicPr>\
         <pic:blipFill>\
         <a:blip r:embed=\"{rel_id}\"/>\
         <a:stretch><a:fillRect/></a:stretch>\
         </pic:blipFill>\
         <pic:spPr>\
         <a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>\
         <a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom>\
         </pic:spPr>\
         </pic:pic>\
         </a:graphicData>\
         </a:graphic>\
         </wp:inline>"
    ));
    out.push_str("</w:drawing></w:r>");
}

/// Phase 3 passthrough optimisation. A paragraph that was loaded from a
/// `.docx` (so `source_xml` is `Some`) and has not been mutated by the
/// engine (`dirty == false`) is re-emitted **verbatim** from its captured
/// source bytes. Every other paragraph regenerates via
/// `serialize_paragraph` — the lossy-vs-stylesheet but engine-state-
/// preserving fallback path.
///
/// This is what keeps stylesheet-heavy real-world `.docx` files
/// byte-stable on round-trip: paragraphs the user didn't touch carry
/// their original `<w:pStyle>` / `<w:rsidR>` / `<w:proofErr>` markup
/// unmodified.
fn emit_paragraph(para: &Paragraph, out: &mut String) {
    if !para.dirty
        && let Some(raw) = &para.source_xml
    {
        /* Source bytes are valid UTF-8 — they came out of a well-formed
        XML document that the reader already round-tripped through
        `quick_xml`'s decoder. Defensively fall back on regenerate if the
        bytes aren't UTF-8. */
        match std::str::from_utf8(raw) {
            Ok(s) => out.push_str(s),
            Err(_) => serialize_paragraph(para, out),
        }
    } else {
        serialize_paragraph(para, out);
    }
}

/// Phase 5 PR 1 block dispatcher. `Block::Paragraph` rides the existing
/// `emit_paragraph` passthrough optimisation; `Block::Table` rides its
/// own opaque passthrough (`source_xml` if clean, else a stub —
/// regenerate-from-rows lands in Phase 5 PR 2).
fn emit_block(block: &Block, out: &mut String) {
    match block {
        Block::Paragraph(p) => emit_paragraph(p, out),
        Block::Table(t) => emit_table(t, out),
    }
}

fn emit_table(t: &Table, out: &mut String) {
    if !t.dirty
        && let Some(raw) = &t.source_xml
    {
        /* Source bytes captured by the Phase 5 PR 1 parser — already
        valid UTF-8 from `quick_xml`. Same defensive fall-back as
        `emit_paragraph`. */
        match std::str::from_utf8(raw) {
            Ok(s) => {
                out.push_str(s);
                return;
            }
            Err(_) => { /* fall through to regenerate */ }
        }
    }
    regenerate_table(t, out);
}

/// Phase 5 PR 3 — full table regeneration. Emits `<w:tbl>` with
/// `<w:tblPr>` (when non-default), `<w:tblGrid>`, then each `<w:tr>`
/// with `<w:trPr>` and `<w:tc>` cells carrying `<w:tcPr>` (gridSpan,
/// vMerge, tcW, shd, tcBorders, vAlign) and nested block content via
/// `emit_block` for true recursion.
fn regenerate_table(t: &Table, out: &mut String) {
    out.push_str("<w:tbl>");
    emit_tbl_pr(&t.props, out);
    /* `<w:tblGrid>` — one `<w:gridCol w:w="…"/>` per template column. */
    if !t.grid.is_empty() {
        out.push_str("<w:tblGrid>");
        for w in &t.grid {
            out.push_str(&format!("<w:gridCol w:w=\"{w}\"/>"));
        }
        out.push_str("</w:tblGrid>");
    }
    for row in &t.rows {
        emit_table_row(row, out);
    }
    out.push_str("</w:tbl>");
}

fn emit_tbl_pr(props: &engine::TableProperties, out: &mut String) {
    let has_margins = props.cell_margins != engine::CellMargins::default();
    let has_layout_override = matches!(props.layout, engine::TableLayout::Fixed);
    let has_content = props.width.is_some()
        || props.alignment.is_some()
        || props.indent_twips != 0
        || props.borders.is_some()
        || props.table_style_id.is_some()
        || has_margins
        || has_layout_override;
    if !has_content {
        return;
    }
    out.push_str("<w:tblPr>");
    if let Some(id) = &props.table_style_id {
        out.push_str(&format!("<w:tblStyle w:val=\"{id}\"/>"));
    }
    if let Some(w) = props.width {
        emit_w_width("w:tblW", w, out);
    }
    if props.indent_twips != 0 {
        out.push_str(&format!(
            "<w:tblInd w:w=\"{}\" w:type=\"dxa\"/>",
            props.indent_twips
        ));
    }
    if let Some(a) = props.alignment {
        out.push_str(&format!("<w:jc w:val=\"{}\"/>", jc_val(a)));
    }
    /* Audit gap A.M8 — emit `<w:tblLayout>` only for the non-default
    `Fixed` form so existing Autofit tables stay byte-identical on
    roundtrip. */
    if has_layout_override {
        out.push_str("<w:tblLayout w:type=\"fixed\"/>");
    }
    if let Some(b) = &props.borders {
        emit_cell_borders("w:tblBorders", b, out);
    }
    if has_margins {
        emit_cell_margins("w:tblCellMar", &props.cell_margins, out);
    }
    out.push_str("</w:tblPr>");
}

fn emit_table_row(row: &TableRow, out: &mut String) {
    out.push_str("<w:tr>");
    emit_tr_pr(&row.props, out);
    for cell in &row.cells {
        emit_table_cell(cell, out);
    }
    out.push_str("</w:tr>");
}

fn emit_tr_pr(props: &engine::RowProperties, out: &mut String) {
    let has = props.height.is_some() || props.cant_split || props.header;
    if !has {
        return;
    }
    out.push_str("<w:trPr>");
    if let Some(h) = props.height {
        let (twips, rule) = match h {
            RowHeight::Auto => (0, "auto"),
            RowHeight::AtLeast { twips } => (twips, "atLeast"),
            RowHeight::Exact { twips } => (twips, "exact"),
        };
        out.push_str(&format!(
            "<w:trHeight w:val=\"{twips}\" w:hRule=\"{rule}\"/>"
        ));
    }
    if props.cant_split {
        out.push_str("<w:cantSplit/>");
    }
    if props.header {
        out.push_str("<w:tblHeader/>");
    }
    out.push_str("</w:trPr>");
}

fn emit_table_cell(cell: &TableCell, out: &mut String) {
    out.push_str("<w:tc>");
    emit_tc_pr(&cell.props, out);
    /* A cell must contain at least one paragraph (Word repair dialog
    fires on empty cells); inject a default `<w:p>` when blocks are
    empty. */
    if cell.blocks.is_empty() {
        out.push_str("<w:p/>");
    } else {
        for block in &cell.blocks {
            emit_block(block, out);
        }
    }
    out.push_str("</w:tc>");
}

fn emit_tc_pr(props: &engine::CellProperties, out: &mut String) {
    let has = props.grid_span > 1
        || !matches!(props.v_merge, VMergeRole::None)
        || props.width.is_some()
        || props.borders.is_some()
        || props.shading.is_some()
        || !matches!(props.v_align, engine::VerticalAlign::Top)
        || props.cell_margins.is_some();
    if !has {
        return;
    }
    out.push_str("<w:tcPr>");
    if let Some(w) = props.width {
        emit_w_width("w:tcW", w, out);
    }
    if props.grid_span > 1 {
        out.push_str(&format!("<w:gridSpan w:val=\"{}\"/>", props.grid_span));
    }
    match props.v_merge {
        VMergeRole::Restart => out.push_str("<w:vMerge w:val=\"restart\"/>"),
        VMergeRole::Continue => out.push_str("<w:vMerge/>"),
        VMergeRole::None => {}
    }
    if let Some(b) = &props.borders {
        emit_cell_borders("w:tcBorders", b, out);
    }
    if let Some([r, g, b, _]) = props.shading {
        out.push_str(&format!(
            "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{r:02X}{g:02X}{b:02X}\"/>"
        ));
    }
    match props.v_align {
        engine::VerticalAlign::Center => out.push_str("<w:vAlign w:val=\"center\"/>"),
        engine::VerticalAlign::Bottom => out.push_str("<w:vAlign w:val=\"bottom\"/>"),
        engine::VerticalAlign::Top => {}
    }
    if let Some(m) = &props.cell_margins {
        emit_cell_margins("w:tcMar", m, out);
    }
    out.push_str("</w:tcPr>");
}

/// Phase 2 audit (gap B.1/B.2) — emit `<w:tblCellMar>` or `<w:tcMar>`.
/// Only edges with a value emit a sub-element; absent edges (the
/// `Option<i32>` is `None`) round-trip as inheritance markers, matching
/// how the reader captured them. A `<w:tcMar>` with only `<w:left>` and
/// `<w:right>` set re-emits the same shape, preserving Word's inherit
/// semantics for top + bottom.
fn emit_cell_margins(elem: &str, m: &engine::CellMargins, out: &mut String) {
    out.push_str(&format!("<{elem}>"));
    if let Some(v) = m.top_twips {
        out.push_str(&format!("<w:top w:w=\"{v}\" w:type=\"dxa\"/>"));
    }
    if let Some(v) = m.left_twips {
        out.push_str(&format!("<w:left w:w=\"{v}\" w:type=\"dxa\"/>"));
    }
    if let Some(v) = m.bottom_twips {
        out.push_str(&format!("<w:bottom w:w=\"{v}\" w:type=\"dxa\"/>"));
    }
    if let Some(v) = m.right_twips {
        out.push_str(&format!("<w:right w:w=\"{v}\" w:type=\"dxa\"/>"));
    }
    out.push_str(&format!("</{elem}>"));
}

fn emit_w_width(elem: &str, w: CellWidth, out: &mut String) {
    let (val, typ) = match w {
        CellWidth::Dxa(v) => (v.to_string(), "dxa"),
        CellWidth::Pct(v) => (v.to_string(), "pct"),
        CellWidth::Auto => ("0".to_string(), "auto"),
        CellWidth::Nil => ("0".to_string(), "nil"),
    };
    out.push_str(&format!("<{elem} w:w=\"{val}\" w:type=\"{typ}\"/>"));
}

fn emit_cell_borders(elem: &str, b: &CellBorders, out: &mut String) {
    out.push_str(&format!("<{elem}>"));
    emit_border_edge("w:top", &b.top, out);
    emit_border_edge("w:left", &b.left, out);
    emit_border_edge("w:bottom", &b.bottom, out);
    emit_border_edge("w:right", &b.right, out);
    emit_border_edge("w:insideH", &b.inside_h, out);
    emit_border_edge("w:insideV", &b.inside_v, out);
    out.push_str(&format!("</{elem}>"));
}

fn emit_border_edge(elem: &str, edge: &Option<BorderStroke>, out: &mut String) {
    let Some(s) = edge else { return };
    let val = match &s.style {
        BorderStyle::Single => "single",
        BorderStyle::Double => "double",
        BorderStyle::Dotted => "dotted",
        BorderStyle::Dashed => "dashed",
        BorderStyle::None => "none",
        BorderStyle::Other(o) => o.as_str(),
    };
    let mut attrs = format!(" w:val=\"{val}\" w:sz=\"{}\"", s.size_eighth_pt);
    if let Some([r, g, b, _]) = s.color {
        attrs.push_str(&format!(" w:color=\"{r:02X}{g:02X}{b:02X}\""));
    }
    out.push_str(&format!("<{elem}{attrs}/>"));
}

fn build_document_xml(doc: &DocumentTree) -> String {
    let mut out = String::with_capacity(2048);
    if doc_has_inline_images(doc) {
        out.push_str(DOC_XML_HEADER_WITH_DRAWING);
    } else {
        out.push_str(DOC_XML_HEADER);
    }
    for block in &doc.blocks {
        emit_block(block, &mut out);
    }
    /* Audit gap A.H2 / A.M11 / A.M12 — trailing body-level `<w:sectPr/>`
    carries the last section's column / page-numbering / section-type
    descriptors. Emit the full element only when one of them is non-
    default; otherwise stay on the bare `<w:sectPr/>` form so existing
    roundtrip fixtures stay byte-identical. */
    let trailing_sect = doc.sections.last().cloned().unwrap_or_default();
    let cols = trailing_sect.columns;
    let pgn = trailing_sect.page_num;
    let sect_type = trailing_sect.section_type;
    let needs_full = cols.is_multi()
        || pgn != engine::PageNumType::default()
        || sect_type != engine::SectionType::default();
    if needs_full {
        out.push_str("<w:sectPr>");
        if cols.is_multi() {
            emit_cols(cols, &mut out);
        }
        emit_pg_num_type(pgn, &mut out);
        emit_sect_type(sect_type, &mut out);
        out.push_str("</w:sectPr></w:body></w:document>");
    } else {
        out.push_str(DOC_XML_FOOTER);
    }
    out
}

/// Audit gap A.M11 — emit `<w:pgNumType w:start w:fmt/>`. Skips entirely
/// when both fields are at their defaults (no `start` override, decimal
/// format) to keep the trailing sectPr lean for plain documents.
fn emit_pg_num_type(p: engine::PageNumType, out: &mut String) {
    if p == engine::PageNumType::default() {
        return;
    }
    out.push_str("<w:pgNumType");
    if let Some(n) = p.start {
        out.push_str(&format!(" w:start=\"{n}\""));
    }
    let fmt = match p.format {
        engine::PageNumFormat::Decimal => None,
        engine::PageNumFormat::LowerRoman => Some("lowerRoman"),
        engine::PageNumFormat::UpperRoman => Some("upperRoman"),
        engine::PageNumFormat::LowerLetter => Some("lowerLetter"),
        engine::PageNumFormat::UpperLetter => Some("upperLetter"),
    };
    if let Some(f) = fmt {
        out.push_str(&format!(" w:fmt=\"{f}\""));
    }
    out.push_str("/>");
}

/// Audit gap A.M12 — emit `<w:type w:val>`. `NextPage` is the implicit
/// default; omit. Other variants round-trip explicitly.
fn emit_sect_type(t: engine::SectionType, out: &mut String) {
    let v = match t {
        engine::SectionType::NextPage => return,
        engine::SectionType::Continuous => "continuous",
        engine::SectionType::EvenPage => "evenPage",
        engine::SectionType::OddPage => "oddPage",
    };
    out.push_str(&format!("<w:type w:val=\"{v}\"/>"));
}

/// Audit gap A.H2 — emit `<w:cols w:num w:space/>`. Caller has already
/// confirmed the section is multi-column. `w:space` is round-tripped in
/// twips to avoid pt → twip rounding drift on a value Word stamps as an
/// exact integer (720, 360, etc.).
fn emit_cols(cols: engine::ColumnSpec, out: &mut String) {
    let space_twips = (cols.gutter_pt * 20.0).round() as i32;
    out.push_str(&format!(
        "<w:cols w:num=\"{}\" w:space=\"{}\"/>",
        cols.count, space_twips
    ));
}

/// Repack `archive`'s sibling entries verbatim + a freshly serialized
/// `word/document.xml` from `doc`. Returns the assembled `.docx` bytes.
pub fn write_docx(archive: &DocxArchive, doc: &DocumentTree) -> Result<Vec<u8>, DocxError> {
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);

        let comments_already_present = archive.other_entries.iter().any(|(n, _)| n == COMMENTS_XML);
        let extended_already_present = archive
            .other_entries
            .iter()
            .any(|(n, _)| n == COMMENTS_EXTENDED_XML);

        /* L1.2 (#18) — synthesize `word/comments.xml` only when the
        archive carries no `comments.xml` AND the engine holds at least
        one comment that NEEDS the extended part but has no captured
        paraId: a resolved comment, or (issue #27) a threaded reply —
        both round-trip through `<w15:commentEx>` rows that key on
        `w14:paraId`. The synthesis path mints fresh `w14:paraId` /
        `w14:textId` values per `<w:p>` so the matching
        `commentsExtended.xml` row can refer to them. Existing
        Word-authored `comments.xml` is never overwritten — the writer
        continues to ride the OPC passthrough. */
        let any_unminted_resolved = doc
            .comment_defs
            .values()
            .any(|c| (c.resolved || c.parent_id.is_some()) && c.first_para_id.is_none());
        let (comments_bytes, minted_paraids): (Option<Vec<u8>>, HashMap<u32, String>) =
            if any_unminted_resolved && !comments_already_present {
                let (bytes, map) = build_comments_xml(&doc.comment_defs);
                (Some(bytes), map)
            } else {
                (None, HashMap::new())
            };

        /* Sprint 9 — regenerate `word/commentsExtended.xml` only when
        the document carries at least one resolved comment or (issue
        #27) at least one threaded reply. Untouched comments fall
        through the passthrough so unrelated documents stay
        byte-identical. The mint map populates first_para_id for
        engine-minted comments synthesized above. */
        let extended_bytes = if doc
            .comment_defs
            .values()
            .any(|c| c.resolved || c.parent_id.is_some())
        {
            if minted_paraids.is_empty() {
                build_comments_extended_xml(&doc.comment_defs)
            } else {
                build_comments_extended_xml_with_overrides(&doc.comment_defs, &minted_paraids)
            }
        } else {
            None
        };

        /* Sprint 13 (#12) — regenerate `word/numbering.xml` only when
        `doc.numbering.dirty` is `true` (an in-engine synth flipped
        it). Otherwise the OPC passthrough keeps the original bytes
        verbatim — round-trip remains byte-identical for documents
        that never touched a list. */
        let numbering_bytes: Option<Vec<u8>> = if doc.numbering.dirty {
            Some(build_numbering_xml(&doc.numbering))
        } else {
            None
        };
        let numbering_already_present = archive
            .other_entries
            .iter()
            .any(|(n, _)| n == NUMBERING_XML);

        /* L1.2 (#18) — when comments.xml or commentsExtended.xml is
        being synthesized on a fresh document, splice the matching
        `<Override>` / `<Relationship>` rows into the passthrough OPC
        entries additively. Both helpers are no-ops when the row is
        already present, so untouched documents stay byte-identical. */
        let synth_comments = comments_bytes.is_some();
        let synth_extended = !extended_already_present && extended_bytes.is_some();

        /* Write sibling entries verbatim, in original order — except
        for parts we have a regenerated copy for (replace in-place).
        `[Content_Types].xml` and `word/_rels/document.xml.rels` may
        also receive additive splices when L1.2 synthesizes a new
        part on a fresh document. */
        for (name, bytes) in &archive.other_entries {
            zip.start_file(name, opts)?;
            if name == COMMENTS_EXTENDED_XML
                && let Some(new_bytes) = extended_bytes.as_deref()
            {
                zip.write_all(new_bytes)?;
            } else if name == COMMENTS_XML
                && let Some(new_bytes) = comments_bytes.as_deref()
            {
                /* Synthesis path is guarded behind `!comments_already_present`;
                if comments.xml IS in `other_entries`, `comments_bytes` is
                `None` and this branch is unreachable. Kept defensive for
                the case where the gate evolves. */
                zip.write_all(new_bytes)?;
            } else if name == NUMBERING_XML
                && let Some(new_bytes) = numbering_bytes.as_deref()
            {
                zip.write_all(new_bytes)?;
            } else if name == "[Content_Types].xml" && (synth_comments || synth_extended) {
                let raw = std::str::from_utf8(bytes)?;
                let mut patched: Cow<'_, str> = Cow::Borrowed(raw);
                if synth_comments {
                    patched = match inject_content_type_override(
                        &patched,
                        COMMENTS_PART_NAME,
                        COMMENTS_CONTENT_TYPE,
                    ) {
                        Cow::Borrowed(_) => patched,
                        Cow::Owned(s) => Cow::Owned(s),
                    };
                }
                if synth_extended {
                    patched = match inject_content_type_override(
                        &patched,
                        COMMENTS_EXT_PART_NAME,
                        COMMENTS_EXT_CONTENT_TYPE,
                    ) {
                        Cow::Borrowed(_) => patched,
                        Cow::Owned(s) => Cow::Owned(s),
                    };
                }
                zip.write_all(patched.as_bytes())?;
            } else if name == RELS_XML && (synth_comments || synth_extended) {
                let raw = std::str::from_utf8(bytes)?;
                let mut patched: Cow<'_, str> = Cow::Borrowed(raw);
                let mut next = next_rel_id(&patched);
                if synth_comments {
                    let rid = format!("rId{next}");
                    let new =
                        inject_doc_rel(&patched, &rid, COMMENTS_REL_TYPE, COMMENTS_REL_TARGET);
                    if let Cow::Owned(s) = new {
                        patched = Cow::Owned(s);
                        next += 1;
                    }
                }
                if synth_extended {
                    let rid = format!("rId{next}");
                    let new = inject_doc_rel(
                        &patched,
                        &rid,
                        COMMENTS_EXT_REL_TYPE,
                        COMMENTS_EXT_REL_TARGET,
                    );
                    if let Cow::Owned(s) = new {
                        patched = Cow::Owned(s);
                    }
                }
                zip.write_all(patched.as_bytes())?;
            } else {
                zip.write_all(bytes)?;
            }
        }
        /* Sprint 13 (#12) — append numbering.xml if synthesized fresh
        on a document that never had one. Per Sprint 13 v1, no
        Content_Types Override / rels synth; Word tolerates this. */
        if !numbering_already_present && let Some(new_bytes) = numbering_bytes.as_deref() {
            zip.start_file(NUMBERING_XML, opts)?;
            zip.write_all(new_bytes)?;
        }
        /* L1.2 (#18) — append comments.xml when synthesized fresh.
        Content_Types + rels were patched in the write loop above. */
        if let Some(new_bytes) = comments_bytes.as_deref() {
            zip.start_file(COMMENTS_XML, opts)?;
            zip.write_all(new_bytes)?;
        }
        /* Append `commentsExtended.xml` when synthesized fresh.
        Content_Types + rels were patched in the write loop above. */
        if !extended_already_present && let Some(new_bytes) = extended_bytes.as_deref() {
            zip.start_file(COMMENTS_EXTENDED_XML, opts)?;
            zip.write_all(new_bytes)?;
        }

        /* Write the regenerated document.xml. */
        zip.start_file(DOC_XML, opts)?;
        let xml = build_document_xml(doc);
        zip.write_all(xml.as_bytes())?;

        zip.finish()?;
    }
    Ok(buf)
}

/// Build a minimal valid `.docx` from a freshly created document — no
/// existing archive to base on. When the document references media
/// (`doc.media`), the writer pulls the blobs into `word/media/*` and
/// generates one `<Relationship>` per blob in `word/_rels/document.xml.rels`
/// so each inline `<w:drawing>` resolves. `[Content_Types].xml` learns
/// one `<Default>` per distinct image extension; absent that, Word
/// rejects the file with a "no content type" parse error.
pub fn build_minimal_docx(doc: &DocumentTree) -> Result<Vec<u8>, DocxError> {
    let extensions = media_extensions(doc);
    let content_types = build_content_types(&extensions);
    let doc_rels = build_doc_rels(doc);
    let mut other_entries: Vec<(String, Vec<u8>)> = vec![
        ("[Content_Types].xml".into(), content_types.into_bytes()),
        ("_rels/.rels".into(), DOT_RELS_XML.as_bytes().to_vec()),
        ("word/_rels/document.xml.rels".into(), doc_rels.into_bytes()),
    ];
    /* One `word/media/<filename>` entry per image blob the engine
    holds, named after the relationship id so the `<a:blip r:embed>`
    lookups in `word/document.xml` agree with the rels target. */
    for (rel_id, blob) in &doc.media {
        let filename = media_filename(rel_id, &blob.content_type);
        other_entries.push((format!("word/media/{filename}"), blob.data.clone()));
    }
    let archive = DocxArchive {
        other_entries,
        document: doc.clone(),
    };
    write_docx(&archive, doc)
}

/// Derive the file extension a `word/media/*` entry uses from its
/// MIME content type. Falls back to `bin` for unrecognised types so
/// the archive still validates as ZIP even if Word would later
/// reject the file.
fn media_extension(content_type: &str) -> &'static str {
    match content_type.trim().to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpeg",
        "image/gif" => "gif",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/x-emf" | "image/emf" => "emf",
        "image/x-wmf" | "image/wmf" => "wmf",
        "image/svg+xml" => "svg",
        _ => "bin",
    }
}

fn media_filename(rel_id: &str, content_type: &str) -> String {
    format!("{rel_id}.{ext}", ext = media_extension(content_type))
}

/// Distinct media extensions referenced by `doc.media`. Used to emit
/// one `<Default Extension="png" ContentType="image/png"/>` per type
/// in `[Content_Types].xml`.
fn media_extensions(doc: &DocumentTree) -> Vec<(&'static str, &'static str)> {
    let mut out: Vec<(&'static str, &'static str)> = Vec::new();
    for blob in doc.media.values() {
        let ext = media_extension(&blob.content_type);
        let mime: &'static str = match ext {
            "png" => "image/png",
            "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "bmp" => "image/bmp",
            "tiff" => "image/tiff",
            "emf" => "image/x-emf",
            "wmf" => "image/x-wmf",
            "svg" => "image/svg+xml",
            _ => "application/octet-stream",
        };
        if !out.iter().any(|(e, _)| *e == ext) {
            out.push((ext, mime));
        }
    }
    out
}

fn build_content_types(extra_defaults: &[(&'static str, &'static str)]) -> String {
    let mut out = String::with_capacity(512);
    out.push_str(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\n\
         <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\n\
         <Default Extension=\"xml\" ContentType=\"application/xml\"/>\n",
    );
    for (ext, mime) in extra_defaults {
        out.push_str(&format!(
            "<Default Extension=\"{ext}\" ContentType=\"{mime}\"/>\n"
        ));
    }
    out.push_str(
        "<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\n\
         </Types>",
    );
    out
}

fn build_doc_rels(doc: &DocumentTree) -> String {
    let mut out = String::with_capacity(512);
    out.push_str(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\n",
    );
    for (rel_id, blob) in &doc.media {
        let filename = media_filename(rel_id, &blob.content_type);
        out.push_str(&format!(
            "<Relationship Id=\"{rel_id}\" \
             Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/image\" \
             Target=\"media/{filename}\"/>\n"
        ));
    }
    out.push_str("</Relationships>");
    out
}

const DOT_RELS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

/* L1.2 (#18) — OPC plumbing constants for `comments.xml` and
`commentsExtended.xml`. Content-type names + rel-type IRIs match the
OOXML spec; targets are relative to `word/`. */
const COMMENTS_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml";
const COMMENTS_EXT_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.commentsExtended+xml";
const COMMENTS_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
const COMMENTS_EXT_REL_TYPE: &str =
    "http://schemas.microsoft.com/office/2011/relationships/commentsExtended";
const COMMENTS_PART_NAME: &str = "/word/comments.xml";
const COMMENTS_EXT_PART_NAME: &str = "/word/commentsExtended.xml";
const COMMENTS_REL_TARGET: &str = "comments.xml";
const COMMENTS_EXT_REL_TARGET: &str = "commentsExtended.xml";

/// L1.2 (#18) — additive-or-noop splice of a `<Override>` row into an
/// existing `[Content_Types].xml`. Returns `Cow::Borrowed` when the
/// part name is already present, preserving the source bytes
/// byte-identical (no whitespace / attribute-order drift).
fn inject_content_type_override<'a>(
    xml: &'a str,
    part_name: &str,
    content_type: &str,
) -> Cow<'a, str> {
    let needle = format!("PartName=\"{part_name}\"");
    if xml.contains(&needle) {
        return Cow::Borrowed(xml);
    }
    let Some(close_idx) = xml.find("</Types>") else {
        /* Malformed — leave it alone rather than corrupt further. */
        return Cow::Borrowed(xml);
    };
    let mut out = String::with_capacity(xml.len() + 192);
    out.push_str(&xml[..close_idx]);
    out.push_str(&format!(
        "<Override PartName=\"{part_name}\" ContentType=\"{content_type}\"/>\n"
    ));
    out.push_str(&xml[close_idx..]);
    Cow::Owned(out)
}

/// L1.2 (#18) — pick a fresh `rId` that does not collide with any
/// existing `Id="rIdN"` value in `rels_xml`. Returns the next sequence
/// number (so `rId{n}` is unused).
fn next_rel_id(rels_xml: &str) -> u32 {
    let mut max = 0u32;
    let mut rest = rels_xml;
    while let Some(idx) = rest.find("Id=\"rId") {
        rest = &rest[idx + 7..];
        let end = rest.find('"').unwrap_or(rest.len());
        if let Ok(n) = rest[..end].parse::<u32>() {
            max = max.max(n);
        }
        rest = &rest[end..];
    }
    max + 1
}

/// L1.2 (#18) — additive-or-noop splice of a `<Relationship>` row into
/// `word/_rels/document.xml.rels`. Skips when any existing row already
/// points at `target` (regardless of its rId). Returns the rId actually
/// used (caller-allocated when injected, the existing one otherwise).
fn inject_doc_rel<'a>(
    rels_xml: &'a str,
    rel_id: &str,
    rel_type: &str,
    target: &str,
) -> Cow<'a, str> {
    let target_needle = format!("Target=\"{target}\"");
    if rels_xml.contains(&target_needle) {
        return Cow::Borrowed(rels_xml);
    }
    let Some(close_idx) = rels_xml.find("</Relationships>") else {
        return Cow::Borrowed(rels_xml);
    };
    let mut out = String::with_capacity(rels_xml.len() + 256);
    out.push_str(&rels_xml[..close_idx]);
    out.push_str(&format!(
        "<Relationship Id=\"{rel_id}\" Type=\"{rel_type}\" Target=\"{target}\"/>\n"
    ));
    out.push_str(&rels_xml[close_idx..]);
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opc::archive::read_docx;
    use engine::{ParaProperties, Paragraph, StyleRun};

    #[test]
    fn round_trip_run_properties() {
        /* "hello world": bold + red over "hello", underline over "world". */
        let bold_red = SpanStyle {
            bold: Some(true),
            color: Some([255, 0, 0, 255]),
            ..Default::default()
        };
        let ul = SpanStyle {
            underline: Some(UnderlineStyle::Single),
            ..Default::default()
        };
        let para = Paragraph {
            text: "hello world".into(),
            spans: vec![
                StyleRun {
                    start: 0,
                    end: 5,
                    style: bold_red.clone(),
                },
                StyleRun {
                    start: 6,
                    end: 11,
                    style: ul.clone(),
                },
            ],
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: engine::ParaProperties::default(),
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        let p = &parsed.document.nth_paragraph(0).unwrap();
        assert_eq!(p.text, "hello world");
        assert_eq!(p.style_at(0), bold_red);
        assert_eq!(p.style_at(4), bold_red);
        assert_eq!(p.style_at(5), SpanStyle::default());
        assert_eq!(p.style_at(6), ul);
    }

    #[test]
    fn round_trip_highlight_and_family() {
        let styled = SpanStyle {
            bg_color: Some([255, 235, 120, 255]),
            font_family: Some(FontFamily::LiberationSans),
            ..Default::default()
        };
        let para = Paragraph {
            text: "abc".into(),
            spans: vec![StyleRun {
                start: 0,
                end: 3,
                style: styled.clone(),
            }],
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: engine::ParaProperties::default(),
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        assert_eq!(
            parsed.document.nth_paragraph(0).unwrap().style_at(1),
            styled
        );
    }

    /* Issue #23 — a custom (string-backed) font family survives a full
    write → read cycle, resolves to a `Custom` face on read, and re-saves to a
    byte-identical `word/document.xml` (the verbatim display name is preserved
    rather than dropped into `raw_font_family`). */
    #[test]
    fn round_trip_custom_font_family() {
        let styled = SpanStyle {
            font_family: Some(FontFamily::Custom {
                id: "cairo".into(),
                display: "Cairo".into(),
            }),
            ..Default::default()
        };
        let para = Paragraph {
            text: "abc".into(),
            spans: vec![StyleRun {
                start: 0,
                end: 3,
                style: styled.clone(),
            }],
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: engine::ParaProperties::default(),
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        // Resolves to the Custom face — not parked in `raw_font_family`.
        assert_eq!(
            parsed.document.nth_paragraph(0).unwrap().style_at(1),
            styled
        );
        assert!(
            parsed
                .document
                .nth_paragraph(0)
                .unwrap()
                .style_at(1)
                .raw_font_family
                .is_none()
        );
        // Re-save is byte-identical: the verbatim "Cairo" round-trips.
        let resaved = build_minimal_docx(&parsed.document).expect("re-build");
        assert_eq!(bytes, resaved);
    }

    /* Issue #23 regression — a custom `<w:rFonts>` name carrying surrounding
    whitespace must round-trip BYTE-IDENTICALLY. `FontFamily::from_display_name`
    stores the verbatim display (it trims only to derive the resolution id), so
    a trailing space survives read → re-save. Before the fix the reader trimmed
    the stored display and the re-save dropped the space. */
    #[test]
    fn round_trip_custom_font_family_preserves_surrounding_whitespace() {
        let styled = SpanStyle {
            font_family: Some(FontFamily::Custom {
                id: "calibri".into(),
                display: "Calibri ".into(), // trailing space is data, not noise
            }),
            ..Default::default()
        };
        let para = Paragraph {
            text: "abc".into(),
            spans: vec![StyleRun {
                start: 0,
                end: 3,
                style: styled.clone(),
            }],
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: engine::ParaProperties::default(),
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        // The reader reconstructs the same Custom face, trailing space intact.
        assert_eq!(
            parsed.document.nth_paragraph(0).unwrap().style_at(1),
            styled
        );
        // And a regenerated re-save is byte-identical — no whitespace drift.
        let resaved = build_minimal_docx(&parsed.document).expect("re-build");
        assert_eq!(bytes, resaved);
    }

    #[test]
    fn round_trip_font_size_sz_szcs() {
        /* Reader must lift `<w:sz>` / `<w:szCs>` into `SpanStyle.font_size`;
        writer must emit both elements so ASCII + complex-script runs
        agree on the size. Half-point math: 13.5 pt = w:val="27". */
        let sized = SpanStyle {
            font_size: Some(13.5),
            ..Default::default()
        };
        let para = Paragraph {
            text: "abc".into(),
            spans: vec![StyleRun {
                start: 0,
                end: 3,
                style: sized,
            }],
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: engine::ParaProperties::default(),
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        let bytes = build_minimal_docx(&doc).expect("build");
        /* Inspect the regenerated document.xml directly — both elements
        must be present at the matching half-point value. */
        let saved_doc_xml = {
            let mut z = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
            let mut f = z.by_name("word/document.xml").unwrap();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            s
        };
        assert!(
            saved_doc_xml.contains("<w:sz w:val=\"27\"/>"),
            "ASCII font-size element missing: {saved_doc_xml}"
        );
        assert!(
            saved_doc_xml.contains("<w:szCs w:val=\"27\"/>"),
            "complex-script font-size element missing: {saved_doc_xml}"
        );
        /* Reader round-trip — value must come back as 13.5 pt. */
        let parsed = read_docx(&bytes).expect("read");
        assert_eq!(
            parsed
                .document
                .nth_paragraph(0)
                .unwrap()
                .style_at(1)
                .font_size,
            Some(13.5)
        );
    }

    #[test]
    fn reader_szcs_overrides_sz_when_both_present() {
        /* OOXML lists `<w:szCs>` after `<w:sz>` in CT_RPr; complex-script
        docs depend on it winning. `apply_rpr` folds both into the same
        slot in document order, so the last one written wins. */
        use crate::schema::ct_rpr::apply_rpr;
        use quick_xml::events::Event;
        use quick_xml::reader::Reader;
        let xml = br#"<w:rPr><w:sz w:val="20"/><w:szCs w:val="28"/></w:rPr>"#;
        let mut r = Reader::from_reader(&xml[..]);
        r.config_mut().trim_text(true);
        let mut style = SpanStyle::default();
        let mut buf = Vec::new();
        loop {
            match r.read_event_into(&mut buf).unwrap() {
                Event::Empty(e) | Event::Start(e) if e.name().as_ref() != b"w:rPr" => {
                    apply_rpr(e.name().as_ref(), &e, &mut style);
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        assert_eq!(style.font_size, Some(14.0));
    }

    #[test]
    fn revision_writer_wraps_insert_and_delete_with_attrs() {
        /* Paragraph: "hello bold world". `<w:ins>` covers "hello "
        (live insertion); `<w:del>` covers "bold " (tracked deletion).
        The writer must emit both wrappers around the matching `<w:r>`
        runs, switch the deleted text's element to `<w:delText>`, and
        propagate the `w:author` / `w:date` / `w:id` attributes. */
        let para = Paragraph {
            text: "hello bold world".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: true,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: vec![
                Revision {
                    start: 0,
                    end: 6,
                    kind: RevisionKind::Insert,
                    author: "Alice".into(),
                    date: "2026-01-01T00:00:00Z".into(),
                    id: Some(7),
                    prev_attrs: None,
                },
                Revision {
                    start: 6,
                    end: 11,
                    kind: RevisionKind::Delete,
                    author: "Bob".into(),
                    date: "2026-01-02T00:00:00Z".into(),
                    id: Some(8),
                    prev_attrs: None,
                },
            ],
            fields: Vec::new(),
            style_id: None,
            direct_overrides: engine::ParaProperties::default(),
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        let xml = build_document_xml(&doc);
        /* Insertion: `<w:ins>` with attrs wraps a `<w:r>` carrying
        "hello " inside a `<w:t>` (live text). */
        assert!(
            xml.contains(
                "<w:ins w:id=\"7\" w:author=\"Alice\" w:date=\"2026-01-01T00:00:00Z\">\
                 <w:r><w:t xml:space=\"preserve\">hello </w:t></w:r>\
                 </w:ins>"
            ),
            "insert wrapper missing or malformed: {xml}"
        );
        /* Deletion: `<w:del>` with attrs wraps a `<w:r>` carrying
        "bold " inside `<w:delText>` (deleted text). */
        assert!(
            xml.contains(
                "<w:del w:id=\"8\" w:author=\"Bob\" w:date=\"2026-01-02T00:00:00Z\">\
                 <w:r><w:delText xml:space=\"preserve\">bold </w:delText></w:r>\
                 </w:del>"
            ),
            "delete wrapper missing or malformed: {xml}"
        );
        /* Trailing "world" lives outside both wrappers as a plain run. */
        assert!(xml.contains("<w:r><w:t xml:space=\"preserve\">world</w:t></w:r>"));
    }

    #[test]
    fn revision_reader_captures_id_attribute() {
        /* Read a document carrying a `<w:ins w:id="42">` wrapper and
        verify the engine model preserves the id. */
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:ins w:id="42" w:author="A" w:date="2026-01-01T00:00:00Z"><w:r><w:t xml:space="preserve">inserted</w:t></w:r></w:ins></w:p><w:sectPr/></w:body></w:document>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o644);
            for (name, body) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", DOT_RELS_XML),
                ("word/document.xml", document_xml),
            ] {
                zip.start_file(name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        let parsed = read_docx(&buf).expect("read");
        let p = parsed.document.nth_paragraph(0).unwrap();
        assert_eq!(p.revisions.len(), 1);
        assert_eq!(p.revisions[0].id, Some(42));
        assert_eq!(p.revisions[0].kind, RevisionKind::Insert);
        assert_eq!(p.revisions[0].author, "A");
    }

    #[test]
    fn section_captures_header_footer_roles_and_title_pg() {
        /* `<w:sectPr>` carries three `<w:headerReference>` (default /
        first / even), one `<w:footerReference>` (default), and
        `<w:titlePg/>`. The reader must split the headers across the
        per-role `HeaderFooterRefs` slots and capture `title_pg`. */
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<w:body>
<w:p><w:r><w:t xml:space="preserve">body</w:t></w:r></w:p>
<w:sectPr>
<w:headerReference w:type="default" r:id="rIdH1"/>
<w:headerReference w:type="first" r:id="rIdH2"/>
<w:headerReference w:type="even" r:id="rIdH3"/>
<w:footerReference w:type="default" r:id="rIdF1"/>
<w:titlePg/>
</w:sectPr>
</w:body>
</w:document>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o644);
            for (name, body) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", DOT_RELS_XML),
                ("word/document.xml", document_xml),
            ] {
                zip.start_file(name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        let parsed = read_docx(&buf).expect("read");
        let sections = &parsed.document.sections;
        assert_eq!(sections.len(), 1, "expected one section");
        let s = &sections[0];
        assert_eq!(s.header_refs.default.as_deref(), Some("rIdH1"));
        assert_eq!(s.header_refs.first.as_deref(), Some("rIdH2"));
        assert_eq!(s.header_refs.even.as_deref(), Some("rIdH3"));
        assert_eq!(s.footer_refs.default.as_deref(), Some("rIdF1"));
        assert!(s.footer_refs.first.is_none());
        assert!(s.footer_refs.even.is_none());
        assert!(s.title_pg, "<w:titlePg/> toggle must capture as on");
    }

    /// Audit gap A.H2 — `<w:cols w:num w:space/>` round-trip. Reader
    /// produces a multi-column `ColumnSpec`; writer re-emits the
    /// element inside the trailing body `<w:sectPr>` for a multi-column
    /// document. Single-column sections (no `<w:cols>` at all) stay on
    /// the legacy empty `<w:sectPr/>` path so existing fixtures don't
    /// drift.
    #[test]
    fn section_round_trips_multi_column_descriptor() {
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>
<w:p><w:r><w:t xml:space="preserve">body</w:t></w:r></w:p>
<w:sectPr><w:cols w:num="2" w:space="360"/></w:sectPr>
</w:body>
</w:document>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o644);
            for (name, body) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", DOT_RELS_XML),
                ("word/document.xml", document_xml),
            ] {
                zip.start_file(name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        let parsed = read_docx(&buf).expect("read");
        let sect = parsed.document.sections.last().expect("section");
        assert_eq!(sect.columns.count, 2);
        assert!(
            (sect.columns.gutter_pt - 18.0).abs() < 0.001,
            "360 twips = 18 pt, got {}",
            sect.columns.gutter_pt
        );
        /* Writer round-trip: the regenerated document.xml must contain
        the structural `<w:cols ...>` element inside `<w:sectPr>`. */
        let resaved = write_docx(&parsed, &parsed.document).expect("write");
        let archive = read_docx(&resaved).expect("re-read");
        let xml = archive
            .other_entries
            .iter()
            .find(|(n, _)| n == "word/document.xml")
            .map(|(_, b)| std::str::from_utf8(b).unwrap_or("").to_string())
            .unwrap_or_else(|| {
                /* Writer rebuilds `word/document.xml` instead of stashing
                it in `other_entries`; pull the bytes from the resaved
                zip directly. */
                let mut zip = zip::ZipArchive::new(Cursor::new(&resaved)).expect("zip");
                let mut file = zip.by_name("word/document.xml").expect("doc");
                let mut s = String::new();
                std::io::Read::read_to_string(&mut file, &mut s).expect("read");
                s
            });
        assert!(
            xml.contains("<w:cols w:num=\"2\" w:space=\"360\"/>"),
            "writer dropped `<w:cols>` on round-trip; got: {xml}"
        );
        let sect2 = archive.document.sections.last().expect("section after");
        assert_eq!(sect2.columns.count, 2);
    }

    #[test]
    fn settings_captures_even_and_odd_headers() {
        /* `<w:evenAndOddHeaders/>` in settings.xml must lift into
        `DocumentSettings.even_and_odd_headers`. */
        let settings_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:evenAndOddHeaders/>
</w:settings>"#;
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">x</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/settings.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml"/>
</Types>"#;
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o644);
            for (name, body) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", DOT_RELS_XML),
                ("word/document.xml", document_xml),
                ("word/settings.xml", settings_xml),
            ] {
                zip.start_file(name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        let parsed = read_docx(&buf).expect("read");
        assert!(
            parsed.document.settings.even_and_odd_headers,
            "even/odd toggle must lift from settings.xml"
        );
    }

    #[test]
    fn round_trip_br_line_and_page_break() {
        /* Reader maps `<w:br/>` to U+2028 and `<w:br w:type="page"/>`
        to U+000C; writer regenerates the structural runs around
        those chars when the paragraph is dirty. Verify both:
        text bytes survive the round-trip, and the regenerated XML
        emits a `<w:r><w:br…/></w:r>` rather than a bare `<w:t>` with
        the Unicode char. */
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>
<w:p>
<w:r><w:t xml:space="preserve">before</w:t></w:r>
<w:r><w:br/></w:r>
<w:r><w:t xml:space="preserve">soft</w:t></w:r>
<w:r><w:br w:type="page"/></w:r>
<w:r><w:t xml:space="preserve">after</w:t></w:r>
</w:p>
<w:sectPr/>
</w:body>
</w:document>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o644);
            for (name, body) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", DOT_RELS_XML),
                ("word/document.xml", document_xml),
            ] {
                zip.start_file(name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        let parsed = read_docx(&buf).expect("read");
        let p = parsed.document.nth_paragraph(0).unwrap();
        /* U+2028 and U+000C land in `para.text` at the break positions. */
        assert!(
            p.text.contains('\u{2028}'),
            "line break char missing: {:?}",
            p.text
        );
        assert!(
            p.text.contains('\u{000C}'),
            "page break char missing: {:?}",
            p.text
        );
        let line_pos = p.text.find('\u{2028}').unwrap();
        let page_pos = p.text.find('\u{000C}').unwrap();
        assert_eq!(line_pos, "before".len());
        assert_eq!(
            page_pos,
            "before".len() + '\u{2028}'.len_utf8() + "soft".len()
        );

        /* Force regenerate + reparse — writer emits structural runs. */
        let mut owned = parsed.document.clone();
        if let Some(engine::Block::Paragraph(p)) = owned.blocks.iter_mut().next() {
            p.dirty = true;
            p.source_xml = None;
        }
        let xml = build_document_xml(&owned);
        assert!(
            xml.contains("<w:r><w:br/></w:r>"),
            "line break run missing: {xml}"
        );
        assert!(
            xml.contains("<w:r><w:br w:type=\"page\"/></w:r>"),
            "page break run missing: {xml}"
        );
        /* Reparse — chars come back at the same offsets. */
        let saved = write_docx(&parsed, &owned).expect("write");
        let reparsed = read_docx(&saved).expect("re-read");
        let q = reparsed.document.nth_paragraph(0).unwrap();
        assert_eq!(q.text, p.text);
    }

    #[test]
    fn round_trip_cell_margins_per_cell_and_table_default() {
        /* `<w:tblCellMar>` on the table provides defaults (200 twips
        on every edge); `<w:tcMar>` on the second cell overrides only
        `left` and `right`. Reader fills `TableProperties.cell_margins`
        + the cell's `Option<CellMargins>`; writer round-trips both. */
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>
<w:tbl>
<w:tblPr>
<w:tblCellMar>
<w:top w:w="200" w:type="dxa"/>
<w:left w:w="200" w:type="dxa"/>
<w:bottom w:w="200" w:type="dxa"/>
<w:right w:w="200" w:type="dxa"/>
</w:tblCellMar>
</w:tblPr>
<w:tblGrid><w:gridCol w:w="2880"/><w:gridCol w:w="2880"/></w:tblGrid>
<w:tr>
<w:tc><w:p><w:r><w:t xml:space="preserve">A</w:t></w:r></w:p></w:tc>
<w:tc>
<w:tcPr>
<w:tcMar>
<w:left w:w="500" w:type="dxa"/>
<w:right w:w="500" w:type="dxa"/>
</w:tcMar>
</w:tcPr>
<w:p><w:r><w:t xml:space="preserve">B</w:t></w:r></w:p>
</w:tc>
</w:tr>
</w:tbl>
<w:p><w:r><w:t xml:space="preserve">after</w:t></w:r></w:p>
<w:sectPr/>
</w:body>
</w:document>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o644);
            for (name, body) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", DOT_RELS_XML),
                ("word/document.xml", document_xml),
            ] {
                zip.start_file(name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        let parsed = read_docx(&buf).expect("read");
        let table = parsed
            .document
            .blocks
            .iter()
            .find_map(|b| b.as_table())
            .expect("table block");
        assert_eq!(table.props.cell_margins.top_twips, Some(200));
        assert_eq!(table.props.cell_margins.left_twips, Some(200));
        assert_eq!(table.props.cell_margins.bottom_twips, Some(200));
        assert_eq!(table.props.cell_margins.right_twips, Some(200));
        let row0 = &table.rows[0];
        /* Cell 0 — no override; `cell_margins` stays None. */
        assert!(row0.cells[0].props.cell_margins.is_none());
        /* Cell 1 — `<w:tcMar>` override on left + right only; top +
        bottom remain None and inherit the table default at resolve. */
        let cm = row0.cells[1]
            .props
            .cell_margins
            .as_ref()
            .expect("cell 1 override");
        assert_eq!(cm.left_twips, Some(500));
        assert_eq!(cm.right_twips, Some(500));
        assert_eq!(cm.top_twips, None);
        assert_eq!(cm.bottom_twips, None);
        /* Resolver — top/bottom inherit from the table default, left/
        right pick the cell override. */
        let resolved = engine::CellMargins::resolve_edges(Some(cm), &table.props.cell_margins);
        assert_eq!(resolved.left_twips, 500);
        assert_eq!(resolved.right_twips, 500);
        assert_eq!(resolved.top_twips, 200);
        assert_eq!(resolved.bottom_twips, 200);

        /* Force-regenerate the table and check the writer emits both
        margin elements. */
        let mut owned = parsed.document.clone();
        for b in owned.blocks.iter_mut() {
            if let engine::Block::Table(t) = b {
                t.dirty = true;
                t.source_xml = None;
            }
        }
        let xml = build_document_xml(&owned);
        assert!(
            xml.contains("<w:tblCellMar>"),
            "tblCellMar missing on regenerate: {xml}"
        );
        assert!(
            xml.contains("<w:tcMar>"),
            "tcMar missing on regenerate: {xml}"
        );
        assert!(
            xml.contains("<w:left w:w=\"500\" w:type=\"dxa\"/>"),
            "cell override left missing: {xml}"
        );
    }

    #[test]
    fn header_part_parses_rich_paragraphs_with_fields() {
        /* Phase 2 audit (gap D.1 follow-up). A `word/header1.xml` part
        carrying a PAGE field must parse into the full `Paragraph`
        model, not the legacy `Vec<String>` text-only shape. Once on
        `doc.headers`, downstream consumers (engine-wasm + paginator)
        see the field overlay and can re-evaluate it per page. */
        let header_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:p>
<w:r><w:t xml:space="preserve">Page </w:t></w:r>
<w:r><w:fldChar w:fldCharType="begin"/></w:r>
<w:r><w:instrText xml:space="preserve"> PAGE </w:instrText></w:r>
<w:r><w:fldChar w:fldCharType="separate"/></w:r>
<w:r><w:t xml:space="preserve">1</w:t></w:r>
<w:r><w:fldChar w:fldCharType="end"/></w:r>
</w:p>
</w:hdr>"#;
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<w:body>
<w:p><w:r><w:t xml:space="preserve">body</w:t></w:r></w:p>
<w:sectPr><w:headerReference w:type="default" r:id="rIdH1"/></w:sectPr>
</w:body>
</w:document>"#;
        let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rIdH1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
</Relationships>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>
</Types>"#;
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o644);
            for (name, body) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", DOT_RELS_XML),
                ("word/_rels/document.xml.rels", doc_rels),
                ("word/document.xml", document_xml),
                ("word/header1.xml", header_xml),
            ] {
                zip.start_file(name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        let parsed = read_docx(&buf).expect("read");
        let header_paragraphs = parsed
            .document
            .headers
            .get("rIdH1")
            .expect("rIdH1 header part parsed");
        assert_eq!(header_paragraphs.len(), 1);
        let hp = &header_paragraphs[0];
        /* Header text contains the "Page " prefix + cached "1". */
        assert_eq!(hp.text, "Page 1");
        /* The PAGE field overlay survived the reroute through
        `parse_document_xml` — proves the header reader is now using
        the same full-fidelity pipeline document.xml does. */
        assert_eq!(hp.fields.len(), 1);
        assert_eq!(hp.fields[0].keyword(), "PAGE");
        assert_eq!(hp.fields[0].start, 5);
        assert_eq!(hp.fields[0].end, 6);
    }

    #[test]
    fn complex_field_round_trip_page_number() {
        /* Reader: walk a `PAGE \\* MERGEFORMAT` complex field split
        across the canonical begin / instrText / separate / cached /
        end run sequence. Must produce one `engine::Field` overlay
        anchoring the cached text `"7"` (Word's last-rendered value)
        with the instruction stripped of surrounding whitespace. */
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:body>
<w:p>
<w:r><w:t xml:space="preserve">Page </w:t></w:r>
<w:r><w:fldChar w:fldCharType="begin"/></w:r>
<w:r><w:instrText xml:space="preserve"> PAGE \* MERGEFORMAT </w:instrText></w:r>
<w:r><w:fldChar w:fldCharType="separate"/></w:r>
<w:r><w:t xml:space="preserve">7</w:t></w:r>
<w:r><w:fldChar w:fldCharType="end"/></w:r>
<w:r><w:t xml:space="preserve"> of doc</w:t></w:r>
</w:p>
<w:sectPr/>
</w:body>
</w:document>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o644);
            for (name, body) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", DOT_RELS_XML),
                ("word/document.xml", document_xml),
            ] {
                zip.start_file(name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        let parsed = read_docx(&buf).expect("read");
        let p = parsed.document.nth_paragraph(0).unwrap();
        /* Reader: text contains the live "Page " prefix + cached "7" +
        " of doc" suffix; a single Field overlay anchors the "7". */
        assert_eq!(p.text, "Page 7 of doc");
        assert_eq!(p.fields.len(), 1, "one PAGE field expected");
        let f = &p.fields[0];
        assert_eq!(f.start, 5);
        assert_eq!(f.end, 6);
        assert_eq!(f.instruction, "PAGE \\* MERGEFORMAT");
        assert_eq!(f.keyword(), "PAGE");

        /* Force regeneration through the writer and verify the
        wrappers re-emit as the canonical run sequence. */
        let mut owned_doc = parsed.document.clone();
        if let Some(engine::Block::Paragraph(p)) = owned_doc.blocks.iter_mut().next() {
            p.dirty = true;
            p.source_xml = None;
        }
        let xml = build_document_xml(&owned_doc);
        assert!(
            xml.contains("<w:r><w:fldChar w:fldCharType=\"begin\"/></w:r>"),
            "writer must emit begin fldChar: {xml}"
        );
        assert!(
            xml.contains(
                "<w:r><w:instrText xml:space=\"preserve\">PAGE \\* MERGEFORMAT</w:instrText></w:r>"
            ),
            "writer must emit instrText: {xml}"
        );
        assert!(
            xml.contains("<w:r><w:fldChar w:fldCharType=\"separate\"/></w:r>"),
            "writer must emit separate fldChar: {xml}"
        );
        assert!(
            xml.contains("<w:r><w:fldChar w:fldCharType=\"end\"/></w:r>"),
            "writer must emit end fldChar: {xml}"
        );
        /* Re-read after regeneration: field must still parse back. */
        let saved_bytes = write_docx(&parsed, &owned_doc).expect("write");
        let reparsed = read_docx(&saved_bytes).expect("re-read");
        let q = reparsed.document.nth_paragraph(0).unwrap();
        assert_eq!(q.text, "Page 7 of doc");
        assert_eq!(q.fields.len(), 1);
        assert_eq!(q.fields[0].keyword(), "PAGE");
    }

    #[test]
    fn round_trip_underline_variants() {
        /* Each underline variant must regenerate to its OOXML `w:val`
        token and parse back to the same enum on the way home.
        Variants stay fully qualified — a glob import would shadow
        `Option::None` and break the literal `None` fields below. */
        use engine::UnderlineStyle;
        let cases = [
            (UnderlineStyle::Single, "single"),
            (UnderlineStyle::Double, "double"),
            (UnderlineStyle::Dotted, "dotted"),
            (UnderlineStyle::Dashed, "dash"),
            (UnderlineStyle::Wavy, "wave"),
        ];
        for (variant, token) in cases {
            let style = SpanStyle {
                underline: Some(variant),
                ..Default::default()
            };
            let para = Paragraph {
                text: "u".into(),
                spans: vec![StyleRun {
                    start: 0,
                    end: 1,
                    style,
                }],
                props: ParaProperties::default(),
                list_item: None,
                resolved_marker: None,
                dirty: false,
                source_xml: None,
                inline_objects: Vec::new(),
                hyperlinks: Vec::new(),
                revisions: Vec::new(),
                fields: Vec::new(),
                style_id: None,
                direct_overrides: engine::ParaProperties::default(),
            };
            let doc = DocumentTree::from_rich_paragraphs([para]);
            let bytes = build_minimal_docx(&doc).expect("build");
            let saved_doc_xml = {
                let mut z = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
                let mut f = z.by_name("word/document.xml").unwrap();
                let mut s = String::new();
                std::io::Read::read_to_string(&mut f, &mut s).unwrap();
                s
            };
            assert!(
                saved_doc_xml.contains(&format!("<w:u w:val=\"{token}\"/>")),
                "{variant:?} must emit `w:val=\"{token}\"`; got {saved_doc_xml}"
            );
            let parsed = read_docx(&bytes).expect("read");
            assert_eq!(
                parsed
                    .document
                    .nth_paragraph(0)
                    .unwrap()
                    .style_at(0)
                    .underline,
                Some(variant)
            );
        }
    }

    #[test]
    fn revision_writer_assigns_fallback_id_when_missing() {
        /* A `Revision` synthesised in-engine has `id == None`; the writer
        must assign a fresh sequential id so the emitted XML stays
        well-formed (Word rejects `<w:ins>` without `w:id`). */
        let para = Paragraph {
            text: "x".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: true,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: vec![Revision {
                start: 0,
                end: 1,
                kind: RevisionKind::Insert,
                author: String::new(),
                date: String::new(),
                id: None,
                prev_attrs: None,
            }],
            fields: Vec::new(),
            style_id: None,
            direct_overrides: engine::ParaProperties::default(),
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        let xml = build_document_xml(&doc);
        /* Fallback ids start at 1; an attribute-less revision still gets
        the bare `w:id="1"` attribute. */
        assert!(
            xml.contains("<w:ins w:id=\"1\">"),
            "fallback id missing: {xml}"
        );
    }

    #[test]
    fn plain_paragraph_emits_no_run_properties() {
        /* A span-free paragraph must serialize without a <w:rPr> so the
        round-trip harness's plain fixtures stay byte-stable. */
        let doc = DocumentTree::from_text("plain text");
        let xml = build_document_xml(&doc);
        assert!(!xml.contains("<w:rPr>"));
        assert!(!xml.contains("<w:pPr>"));
        assert!(xml.contains("<w:p><w:r><w:t xml:space=\"preserve\">plain text</w:t></w:r></w:p>"));
    }

    #[test]
    fn round_trip_para_properties() {
        use engine::{Indent, LineHeight, Spacing, TextDirection};
        let props = ParaProperties {
            alignment: Some(engine::Alignment::Center),
            indent: Indent {
                start_twips: 720,
                first_line_twips: 360,
                ..Default::default()
            },
            spacing: Spacing {
                before_twips: 120,
                after_twips: 240,
            },
            line_height: Some(LineHeight::Auto { twips: 360 }),
            direction: Some(TextDirection::Rtl),
            keep_next: true,
            keep_lines: false,
            page_break_before: false,
            borders: None,
            tab_stops: Vec::new(),
            list_item: None,
            shading: Some([0x33, 0x66, 0x99, 0xFF]),
        };
        let para = Paragraph {
            text: "hello world".into(),
            spans: Vec::new(),
            props: props.clone(),
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: engine::ParaProperties::default(),
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        assert_eq!(parsed.document.nth_paragraph(0).unwrap().props, props);
    }

    #[test]
    fn round_trip_paragraph_shading() {
        let para = Paragraph {
            text: "shaded".into(),
            spans: Vec::new(),
            props: ParaProperties {
                shading: Some([0xA5, 0xD6, 0xA7, 0xFF]),
                ..Default::default()
            },
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: engine::ParaProperties::default(),
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        let xml = build_document_xml(&doc);
        assert!(
            xml.contains("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"A5D6A7\"/>"),
            "pPr must carry the shading fill; xml={xml}"
        );
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        assert_eq!(
            parsed.document.nth_paragraph(0).unwrap().props.shading,
            Some([0xA5, 0xD6, 0xA7, 0xFF])
        );
    }

    #[test]
    fn ppr_child_order_matches_schema() {
        /* Schema mandates keepNext, keepLines, pageBreakBefore, spacing, ind,
        jc, bidi in that order. Word rejects out-of-order children with a
        repair dialog, so this is load-bearing. */
        use engine::{Indent, Spacing, TextDirection};
        let para = Paragraph {
            text: "x".into(),
            spans: Vec::new(),
            props: ParaProperties {
                alignment: Some(engine::Alignment::End),
                indent: Indent {
                    start_twips: 720,
                    ..Default::default()
                },
                spacing: Spacing {
                    before_twips: 120,
                    ..Default::default()
                },
                direction: Some(TextDirection::Rtl),
                keep_next: true,
                keep_lines: true,
                page_break_before: true,
                shading: Some([0xFF, 0xEE, 0xDD, 0xFF]),
                ..Default::default()
            },
            list_item: None,
            resolved_marker: None,
            dirty: false,
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: engine::ParaProperties::default(),
        };
        let xml = build_document_xml(&DocumentTree::from_rich_paragraphs([para]));
        let p = xml.find("<w:pPr>").unwrap();
        let order = [
            "<w:keepNext/>",
            "<w:keepLines/>",
            "<w:pageBreakBefore/>",
            "<w:spacing",
            "<w:shd",
            "<w:ind",
            "<w:jc",
            "<w:bidi",
        ];
        let mut cursor = p;
        for tag in order {
            let off = xml[cursor..]
                .find(tag)
                .unwrap_or_else(|| panic!("expected {tag} after offset {cursor}; xml={xml}"));
            cursor += off + tag.len();
        }
    }

    #[test]
    fn round_trip_minimal() {
        let doc = DocumentTree::from_text("hello world");
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        assert_eq!(parsed.document.paragraph_count(), 1);
        assert_eq!(parsed.document.paragraph_text(0), Some("hello world"));
    }

    /* ---- Phase 3: cascade + passthrough ------------------------------ */

    /// A minimal `.docx` whose `word/styles.xml` defines `BaseStyle` (bold)
    /// and `ChildStyle` (italic, basedOn BaseStyle), with `document.xml`
    /// referencing `ChildStyle`. Used by the cascade + passthrough tests.
    fn build_style_cascade_docx() -> Vec<u8> {
        let styles_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:style w:type="paragraph" w:styleId="BaseStyle"><w:name w:val="Base"/><w:rPr><w:b/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="ChildStyle"><w:name w:val="Child"/><w:basedOn w:val="BaseStyle"/><w:rPr><w:i/></w:rPr></w:style>
</w:styles>"#;
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="ChildStyle"/></w:pPr><w:r><w:t xml:space="preserve">hello cascade</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
</Types>"#;
        let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
        let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#;

        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o644);
            for (name, body) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", dot_rels),
                ("word/_rels/document.xml.rels", doc_rels),
                ("word/styles.xml", styles_xml),
                ("word/document.xml", document_xml),
            ] {
                zip.start_file(name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn cascade_resolves_basedon_chain_into_flat_span() {
        let bytes = build_style_cascade_docx();
        let parsed = read_docx(&bytes).expect("read");
        let p = &parsed.document.nth_paragraph(0).unwrap();
        assert_eq!(p.text, "hello cascade");
        /* Single run, covering the whole text, with the cascaded style:
        bold (from BaseStyle) + italic (from ChildStyle). */
        assert_eq!(p.spans.len(), 1, "expected one resolved run span");
        assert_eq!((p.spans[0].start, p.spans[0].end), (0, 13));
        assert_eq!(p.spans[0].style.bold, Some(true), "BaseStyle bold");
        assert_eq!(p.spans[0].style.italic, Some(true), "ChildStyle italic");
    }

    #[test]
    fn passthrough_keeps_unedited_paragraph_byte_identical() {
        let bytes = build_style_cascade_docx();
        let archive = read_docx(&bytes).expect("read");
        /* Loaded paragraph must be clean, with captured source bytes. */
        let p = &archive.document.nth_paragraph(0).unwrap();
        assert!(!p.dirty, "loaded paragraph must not be dirty");
        let raw = p.source_xml.as_deref().expect("source_xml captured");
        let raw_str = std::str::from_utf8(raw).unwrap();
        assert!(raw_str.starts_with("<w:p>"));
        assert!(raw_str.ends_with("</w:p>"));
        assert!(raw_str.contains("<w:pStyle w:val=\"ChildStyle\"/>"));

        /* Save unedited → document.xml must be byte-identical inside the
        <w:p>...</w:p> region. */
        let saved = write_docx(&archive, &archive.document).expect("write");
        let saved_doc_xml = {
            let mut z = zip::ZipArchive::new(Cursor::new(&saved)).unwrap();
            let mut f = z.by_name("word/document.xml").unwrap();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            s
        };
        assert!(
            saved_doc_xml.contains(raw_str),
            "passthrough must re-emit raw <w:p> bytes verbatim"
        );
    }

    #[test]
    fn edit_falls_back_to_serialized_model() {
        let bytes = build_style_cascade_docx();
        let archive = read_docx(&bytes).expect("read");
        /* Mutate the paragraph — engine flips dirty=true, drops source_xml. */
        let edited = archive.document.insert_text(
            engine::LogicalPos {
                path: engine::BlockPath::top(0),
                offset: 5,
            },
            " EDITED",
        );
        let p = &edited.nth_paragraph(0).unwrap();
        assert!(p.dirty, "edit must flip dirty=true");
        assert!(p.source_xml.is_none(), "edit must drop source_xml");

        /* Save → document.xml regenerates (no pStyle ref, but the cascaded
        style spans are emitted as direct rPr). */
        let saved = write_docx(&archive, &edited).expect("write");
        let saved_doc_xml = {
            let mut z = zip::ZipArchive::new(Cursor::new(&saved)).unwrap();
            let mut f = z.by_name("word/document.xml").unwrap();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            s
        };
        assert!(saved_doc_xml.contains("hello EDITED cascade"));
        /* Re-read: cascade is gone (regenerated doc.xml carries direct
        rPr instead of `<w:pStyle>`); but styles.xml still rides
        passthrough, so the StyleTable is still populated — the bold+italic
        span survives as direct formatting on the regenerated run. */
        let reparsed = read_docx(&saved).expect("re-read");
        let q = &reparsed.document.nth_paragraph(0).unwrap();
        assert_eq!(q.text, "hello EDITED cascade");
        assert!(
            q.spans
                .iter()
                .any(|s| s.style.bold == Some(true) && s.style.italic == Some(true))
        );
    }

    /* ---- Phase 4: numbering + list markers ------------------------- */

    /// Minimal `.docx` with `word/numbering.xml` defining one bullet
    /// abstractNum + one two-level decimal/lowerLetter abstractNum, and
    /// `document.xml` interleaving the two list types. Used by the marker
    /// resolver + list-passthrough tests.
    fn build_list_docx() -> Vec<u8> {
        let numbering_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="*"/></w:lvl></w:abstractNum>
<w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="lowerLetter"/><w:lvlText w:val="%1.%2)"/></w:lvl></w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
<w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>
</w:numbering>"#;
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">bullet alpha</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">first ordered</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">nested item</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">second ordered</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>
</Types>"#;
        let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
        let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>
</Relationships>"#;
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o644);
            for (name, body) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", dot_rels),
                ("word/_rels/document.xml.rels", doc_rels),
                ("word/numbering.xml", numbering_xml),
                ("word/document.xml", document_xml),
            ] {
                zip.start_file(name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn list_paragraphs_get_resolved_markers() {
        let bytes = build_list_docx();
        let parsed = read_docx(&bytes).expect("read");
        let paras: Vec<_> = parsed
            .document
            .blocks
            .iter()
            .filter_map(engine::Block::as_paragraph)
            .collect();
        assert_eq!(paras.len(), 4);
        /* Bullet — literal lvlText. */
        assert_eq!(paras[0].resolved_marker.as_deref(), Some("*"));
        assert_eq!(
            paras[0].list_item,
            Some(engine::ListItem { num_id: 1, ilvl: 0 })
        );
        /* Decimal level 0 — `%1.` ⇒ "1." */
        assert_eq!(paras[1].resolved_marker.as_deref(), Some("1."));
        /* Nested level 1 — `%1.%2)` ⇒ "1.a)" */
        assert_eq!(paras[2].resolved_marker.as_deref(), Some("1.a)"));
        /* Back to level 0 — counter increments to 2; level-1 counter
        resets on next nested visit. */
        assert_eq!(paras[3].resolved_marker.as_deref(), Some("2."));
    }

    #[test]
    fn list_passthrough_keeps_num_pr_byte_identical() {
        let bytes = build_list_docx();
        let archive = read_docx(&bytes).expect("read");
        /* All loaded paragraphs must be clean with captured source bytes. */
        for (i, p) in archive
            .document
            .blocks
            .iter()
            .filter_map(engine::Block::as_paragraph)
            .enumerate()
        {
            assert!(!p.dirty, "para {i} loaded dirty");
            let raw = p.source_xml.as_deref().expect("source_xml captured");
            assert!(std::str::from_utf8(raw).unwrap().contains("<w:numPr>"));
        }
        /* Save unedited — every paragraph's source bytes ride through
        verbatim, so document.xml inside the bytes is byte-stable. */
        let saved = write_docx(&archive, &archive.document).expect("write");
        let saved_doc_xml = {
            let mut z = zip::ZipArchive::new(Cursor::new(&saved)).unwrap();
            let mut f = z.by_name("word/document.xml").unwrap();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            s
        };
        let original_doc_xml = {
            let mut z = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
            let mut f = z.by_name("word/document.xml").unwrap();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            s
        };
        assert_eq!(
            saved_doc_xml, original_doc_xml,
            "passthrough must keep list document.xml byte-identical"
        );
    }

    /* ---- Phase 5 PR 1: opaque table parse + passthrough ----------- */

    fn build_table_docx() -> Vec<u8> {
        let document_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t xml:space="preserve">before</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t xml:space="preserve">A1</w:t></w:r></w:p></w:tc></w:tr></w:tbl><w:p><w:r><w:t xml:space="preserve">after</w:t></w:r></w:p><w:sectPr/></w:body></w:document>"#;
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;
        let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
        let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
</Relationships>"#;
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            let opts = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .unix_permissions(0o644);
            for (name, body) in [
                ("[Content_Types].xml", content_types),
                ("_rels/.rels", dot_rels),
                ("word/_rels/document.xml.rels", doc_rels),
                ("word/document.xml", document_xml),
            ] {
                zip.start_file(name, opts).unwrap();
                zip.write_all(body.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn table_block_carries_parsed_rows_and_source_bytes() {
        let bytes = build_table_docx();
        let parsed = read_docx(&bytes).expect("read");
        let blocks: Vec<_> = parsed.document.blocks.iter().collect();
        assert_eq!(blocks.len(), 3, "two paragraphs + one table block");
        assert!(matches!(blocks[0], engine::Block::Paragraph(_)));
        match &blocks[1] {
            engine::Block::Table(t) => {
                /* Phase 5 PR 2 — rows are now parsed; PR 1's "opaque
                rows" invariant is gone. Source bytes still rides the
                passthrough writer. */
                assert_eq!(t.rows.len(), 1, "1 row in the build_table_docx fixture");
                assert_eq!(t.rows[0].cells.len(), 1);
                assert!(!t.dirty);
                let raw = t.source_xml.as_deref().expect("source_xml captured");
                let raw_str = std::str::from_utf8(raw).unwrap();
                assert!(raw_str.starts_with("<w:tbl"));
                assert!(raw_str.ends_with("</w:tbl>"));
                assert!(raw_str.contains("A1"));
            }
            other => panic!("expected Table, got {other:?}"),
        }
        assert!(matches!(blocks[2], engine::Block::Paragraph(_)));
        /* Paragraph-flat view skips the table. */
        assert_eq!(parsed.document.paragraph_count(), 2);
        assert_eq!(parsed.document.paragraph_text(0), Some("before"));
        assert_eq!(parsed.document.paragraph_text(1), Some("after"));
    }

    #[test]
    fn table_passthrough_keeps_doc_xml_byte_identical() {
        let bytes = build_table_docx();
        let archive = read_docx(&bytes).expect("read");
        let saved = write_docx(&archive, &archive.document).expect("write");
        let original_doc_xml = {
            let mut z = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
            let mut f = z.by_name("word/document.xml").unwrap();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            s
        };
        let saved_doc_xml = {
            let mut z = zip::ZipArchive::new(Cursor::new(&saved)).unwrap();
            let mut f = z.by_name("word/document.xml").unwrap();
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            s
        };
        assert_eq!(
            saved_doc_xml, original_doc_xml,
            "opaque table passthrough must keep document.xml byte-identical"
        );
    }

    #[test]
    fn regenerated_table_round_trips_through_reader() {
        /* Phase 5 PR 3 — synthesise a table via `insert_table`, write,
        re-read, assert structure. `Table.dirty=true + source_xml=None`
        forces the regenerate path. */
        let doc =
            engine::DocumentTree::from_text("hello").insert_table(engine::BlockPath::top(1), 2, 3);
        /* Apply a shading edit to exercise the cell-properties emitter. */
        let doc = doc.set_cell_shading(
            engine::BlockPath::top(1),
            0,
            0,
            Some([0xFF, 0xEB, 0x78, 0xFF]),
        );
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("re-read");
        /* 3 blocks: leading "hello" paragraph, the synthesised table,
        and the trailing empty paragraph `insert_table` now appends to
        give the caret an escape destination + match the OOXML mandate
        of a `<w:p>` after every `<w:tbl>` boundary. */
        assert_eq!(parsed.document.blocks.len(), 3);
        let t = parsed.document.blocks[1].as_table().expect("table");
        assert!(
            matches!(parsed.document.blocks[2], engine::Block::Paragraph(ref p) if p.text.is_empty()),
            "trailing escape paragraph"
        );
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].cells.len(), 3);
        assert_eq!(t.grid.len(), 3);
        assert_eq!(
            t.rows[0].cells[0].props.shading,
            Some([0xFF, 0xEB, 0x78, 0xFF])
        );
    }

    /// Tables epic — a rectangular merge survives the regenerate path:
    /// the writer emits `<w:gridSpan>`/`<w:vMerge>` in Word's collapsed
    /// shape and the reader reproduces the same model.
    #[test]
    fn regenerated_merged_table_round_trips_spans() {
        let doc =
            engine::DocumentTree::from_text("hello").insert_table(engine::BlockPath::top(1), 3, 3);
        let doc = doc.merge_cells(engine::BlockPath::top(1), 0, 0, 1, 1);
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("re-read");
        let t = parsed.document.blocks[1].as_table().expect("table");
        assert_eq!(t.grid.len(), 3, "grid template survives");
        assert_eq!(t.rows[0].cells.len(), 2, "top row collapsed");
        assert_eq!(t.rows[0].cells[0].props.grid_span, 2);
        assert_eq!(
            t.rows[0].cells[0].props.v_merge,
            engine::VMergeRole::Restart
        );
        assert_eq!(t.rows[1].cells.len(), 2, "continuation row collapsed");
        assert_eq!(t.rows[1].cells[0].props.grid_span, 2);
        assert_eq!(
            t.rows[1].cells[0].props.v_merge,
            engine::VMergeRole::Continue
        );
        assert_eq!(t.rows[2].cells.len(), 3, "row below the merge intact");
    }

    #[test]
    fn round_trip_arabic() {
        let doc = DocumentTree::from_text("السلام عليكم ورحمة الله");
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        assert_eq!(
            parsed.document.paragraph_text(0),
            Some("السلام عليكم ورحمة الله")
        );
    }

    #[test]
    fn round_trip_xml_escapes() {
        let doc = DocumentTree::from_text("<a> & </a>");
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        assert_eq!(parsed.document.paragraph_text(0), Some("<a> & </a>"));
    }

    /* ---- Phase 9: image .docx round-trip ----------------------------- */

    /// A paragraph with one inline image survives a save → reload cycle:
    /// the EMU extents stay exact, the rel-id resolves through the
    /// regenerated `word/_rels/document.xml.rels`, and the image blob
    /// lands in `word/media/<rel>.png`.
    #[test]
    fn round_trip_inline_image_drawing_and_media() {
        use engine::{ImageBlob, InlineKind, InlineObject};
        let mut doc = DocumentTree::from_text("\u{FFFC}");
        /* Park a fake PNG blob under the relationship id the paragraph's
        InlineObject references. The bytes don't have to BE a valid PNG
        for this round-trip — the writer just stashes them in
        `word/media/rId7.png` and the reader pulls them back. */
        doc.media.insert(
            "rId7".into(),
            ImageBlob {
                content_type: "image/png".into(),
                data: vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a],
            },
        );
        /* Replace the synth paragraph with one that anchors the image
        at offset 0. */
        let para = Paragraph {
            text: "\u{FFFC}".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: true,
            source_xml: None,
            inline_objects: vec![InlineObject {
                at: 0,
                kind: InlineKind::Image {
                    rel_id: "rId7".into(),
                    width_emu: 1_905_000,
                    height_emu: 1_524_000,
                },
            }],
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: None,
            direct_overrides: engine::ParaProperties::default(),
        };
        let mut blocks = doc.blocks.clone();
        blocks.set(0, Block::Paragraph(para));
        let doc = DocumentTree { blocks, ..doc };
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("read");
        /* Paragraph survives. */
        let p = parsed.document.nth_paragraph(0).expect("paragraph at 0");
        assert_eq!(p.text, "\u{FFFC}");
        assert_eq!(p.inline_objects.len(), 1, "image anchored");
        let obj = &p.inline_objects[0];
        match &obj.kind {
            InlineKind::Image {
                rel_id,
                width_emu,
                height_emu,
            } => {
                assert_eq!(rel_id, "rId7");
                assert_eq!(*width_emu, 1_905_000);
                assert_eq!(*height_emu, 1_524_000);
            }
            other => panic!("expected Image kind, got {other:?}"),
        }
        /* Image bytes land at `word/media/rId7.png` and the reader's
        media cache holds them. */
        let media = parsed
            .document
            .media
            .get("rId7")
            .expect("media blob round-trips");
        assert_eq!(media.content_type, "image/png");
        assert_eq!(
            media.data,
            vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
        );
    }

    /// Sprint 9 — full round-trip of the resolved-comment bit through
    /// `word/commentsExtended.xml`. Hand-build a minimal docx that
    /// already carries a comment-with-paraId in `comments.xml` (so the
    /// reader can map `<w15:commentEx paraId>` back to the right
    /// `CommentDef`), open it, flip `resolved`, save, reopen, and
    /// confirm the bit survived. Also verifies that an *un-resolved*
    /// re-save leaves the original `commentsExtended.xml` byte-
    /// identical (the passthrough invariant).
    #[test]
    fn resolved_comment_round_trips_via_extended_part() {
        let para_id = "DEADBEEF";
        let comments_xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <w:comments xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
                         xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\">\
               <w:comment w:id=\"1\" w:author=\"Test\" w:date=\"2026-01-01T00:00:00Z\">\
                 <w:p w14:paraId=\"{para_id}\"><w:r><w:t>body</w:t></w:r></w:p>\
               </w:comment>\
             </w:comments>"
        );
        /* Initial extended part marks the comment NOT resolved. After
        we flip it and re-save the writer should regenerate this part. */
        let extended_xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <w15:commentsEx xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\">\
               <w15:commentEx w15:paraId=\"{para_id}\" w15:done=\"0\"/>\
             </w15:commentsEx>"
        );

        let doc = DocumentTree::from_text("body");
        let extensions = media_extensions(&doc);
        let content_types = build_content_types(&extensions);
        let doc_rels = build_doc_rels(&doc);
        let other = vec![
            ("[Content_Types].xml".into(), content_types.into_bytes()),
            ("_rels/.rels".into(), DOT_RELS_XML.as_bytes().to_vec()),
            ("word/_rels/document.xml.rels".into(), doc_rels.into_bytes()),
            ("word/comments.xml".into(), comments_xml.into_bytes()),
            (
                "word/commentsExtended.xml".into(),
                extended_xml.clone().into_bytes(),
            ),
        ];
        let archive = DocxArchive {
            other_entries: other,
            document: doc.clone(),
        };
        let bytes = write_docx(&archive, &doc).expect("initial write");

        /* Re-open: the reader should attach the paraId + populate
        resolved = false from the extended part. */
        let opened = read_docx(&bytes).expect("read");
        let cdef = opened.document.comment_defs.get(&1).expect("comment def 1");
        assert_eq!(cdef.first_para_id.as_deref(), Some(para_id));
        assert!(
            !cdef.resolved,
            "comment starts un-resolved (extended part says done=0)"
        );

        /* Flip resolved -> true, save, reopen -> resolved should
        survive. */
        let mut flipped = opened.document.clone();
        if let Some(c) = flipped.comment_defs.get_mut(&1) {
            c.resolved = true;
        }
        let resaved = write_docx(&opened, &flipped).expect("resave");
        let reopened = read_docx(&resaved).expect("re-read");
        let cdef2 = reopened
            .document
            .comment_defs
            .get(&1)
            .expect("comment def 1");
        assert!(cdef2.resolved, "resolved bit survived the .docx round-trip");

        /* Without any resolved comment, the extended part is passed
        through byte-identical (no regeneration). */
        let unflipped = opened.document.clone();
        let untouched = write_docx(&opened, &unflipped).expect("resave without flip");
        let archive_back = read_docx(&untouched).expect("re-read");
        let raw_back = archive_back
            .other_entries
            .iter()
            .find(|(n, _)| n == "word/commentsExtended.xml")
            .map(|(_, b)| b.as_slice())
            .expect("extended part still present");
        assert_eq!(
            raw_back,
            extended_xml.as_bytes(),
            "no resolved → passthrough byte-identical"
        );
    }

    /// L1.2 (#18) — synthesize a fresh `.docx` that holds an engine-
    /// minted comment (`first_para_id = None`), mark it resolved, save,
    /// reopen. The writer must mint a `w14:paraId`, emit
    /// `commentsExtended.xml`, and splice the matching `<Override>` +
    /// `<Relationship>` rows into `[Content_Types].xml` and
    /// `word/_rels/document.xml.rels` so the round-trip carries the
    /// resolved bit back.
    #[test]
    fn engine_minted_comment_synthesizes_paraid_and_opc() {
        use engine::{BlockPath, LogicalPos};
        let pos = || LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        };
        let doc = DocumentTree::from_text("hello");
        let (mut doc, comment_id) = doc.insert_comment(
            pos(),
            pos(),
            "needs review".into(),
            "tester".into(),
            "2026-05-26T12:00:00Z".into(),
        );
        /* Sanity: engine-minted comment has no paraId yet. */
        assert!(
            doc.comment_defs
                .get(&comment_id)
                .expect("inserted comment")
                .first_para_id
                .is_none()
        );
        /* Mark it resolved. */
        if let Some(c) = doc.comment_defs.get_mut(&comment_id) {
            c.resolved = true;
        }

        let bytes = build_minimal_docx(&doc).expect("build");

        /* The archive carries comments.xml + commentsExtended.xml. */
        let archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).expect("zip");
        let names: Vec<String> = archive.file_names().map(|n| n.to_string()).collect();
        assert!(
            names.contains(&"word/comments.xml".to_string()),
            "synthesized comments.xml: {names:?}"
        );
        assert!(
            names.contains(&"word/commentsExtended.xml".to_string()),
            "synthesized commentsExtended.xml: {names:?}"
        );

        /* [Content_Types].xml carries both Overrides. */
        let mut a = archive;
        let ct_raw = {
            let mut f = a.by_name("[Content_Types].xml").expect("content types");
            let mut s = String::new();
            use std::io::Read;
            f.read_to_string(&mut s).expect("read ct");
            s
        };
        assert!(
            ct_raw.contains("PartName=\"/word/comments.xml\""),
            "Content_Types Override for comments.xml missing: {ct_raw}"
        );
        assert!(
            ct_raw.contains("PartName=\"/word/commentsExtended.xml\""),
            "Content_Types Override for commentsExtended.xml missing: {ct_raw}"
        );

        /* word/_rels/document.xml.rels carries both relationships. */
        let rels_raw = {
            let mut f = a.by_name("word/_rels/document.xml.rels").expect("doc rels");
            let mut s = String::new();
            use std::io::Read;
            f.read_to_string(&mut s).expect("read rels");
            s
        };
        assert!(
            rels_raw.contains("Target=\"comments.xml\""),
            "rels missing comments.xml: {rels_raw}"
        );
        assert!(
            rels_raw.contains("Target=\"commentsExtended.xml\""),
            "rels missing commentsExtended.xml: {rels_raw}"
        );

        /* Reopen — the resolved bit survives and first_para_id is now set. */
        let reopened = read_docx(&bytes).expect("re-read");
        let cdef = reopened
            .document
            .comment_defs
            .get(&comment_id)
            .expect("comment def round-tripped");
        assert!(
            cdef.resolved,
            "resolved bit survived round-trip on engine-minted comment"
        );
        assert!(
            cdef.first_para_id.is_some(),
            "first_para_id was minted: {cdef:?}"
        );
    }

    /// Issue #27 — a fresh document holding an engine-minted comment
    /// plus a threaded reply must synthesize `comments.xml` +
    /// `commentsExtended.xml` (even though NO comment is resolved —
    /// the thread alone fires the gate) and carry the parent link
    /// across a save → reopen round-trip via `w15:paraIdParent`.
    #[test]
    fn engine_minted_reply_round_trips_thread() {
        use engine::{BlockPath, LogicalPos};
        let pos = |o: u32| LogicalPos {
            path: BlockPath::top(0),
            offset: o,
        };
        let doc = DocumentTree::from_text("hello world");
        let (doc, parent_id) = doc.insert_comment(
            pos(0),
            pos(5),
            "root comment".into(),
            "Alice".into(),
            "2026-07-01T00:00:00Z".into(),
        );
        let (doc, reply_id) = doc
            .reply_to_comment(
                parent_id,
                "reply body".into(),
                "Bob".into(),
                "2026-07-02T00:00:00Z".into(),
            )
            .expect("parent exists");
        assert_ne!(parent_id, reply_id);

        let bytes = build_minimal_docx(&doc).expect("build");

        /* Both comment parts synthesized despite zero resolved bits. */
        let archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).expect("zip");
        let names: Vec<String> = archive.file_names().map(|n| n.to_string()).collect();
        assert!(
            names.contains(&"word/comments.xml".to_string()),
            "thread fires the comments.xml synth gate: {names:?}"
        );
        assert!(
            names.contains(&"word/commentsExtended.xml".to_string()),
            "thread fires the commentsExtended.xml synth gate: {names:?}"
        );

        /* Reopen — the thread survives: same ids, parent link intact,
        reply body + author preserved, paraIds minted for both. */
        let reopened = read_docx(&bytes).expect("re-read");
        let defs = &reopened.document.comment_defs;
        assert_eq!(defs.len(), 2, "both defs round-tripped: {defs:?}");
        let parent = defs.get(&parent_id).expect("parent def");
        let reply = defs.get(&reply_id).expect("reply def");
        assert_eq!(parent.parent_id, None, "root stays top-level");
        assert_eq!(
            reply.parent_id,
            Some(parent_id),
            "parent link survived the round-trip: {reply:?}"
        );
        assert_eq!(reply.paragraphs, vec!["reply body".to_string()]);
        assert_eq!(reply.author, "Bob");
        assert!(!reply.resolved);
        assert!(
            parent.first_para_id.is_some() && reply.first_para_id.is_some(),
            "paraIds minted for both thread members"
        );
    }

    /// L1.2 (#18) — invariant guard: when no comment carries the
    /// resolved bit, an existing archive with `comments.xml` already
    /// present must round-trip byte-identical (no Content_Types /
    /// rels mutation). Protects unrelated Word-authored documents
    /// from accidental rewriting on every save.
    #[test]
    fn untouched_doc_with_comments_stays_byte_identical() {
        let comments_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
            <w:comments xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" \
                        xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\">\
              <w:comment w:id=\"1\" w:author=\"Alice\" w:date=\"2026-01-01T00:00:00Z\">\
                <w:p w14:paraId=\"AAAA0001\"><w:r><w:t>old</w:t></w:r></w:p>\
              </w:comment>\
            </w:comments>";

        let doc = DocumentTree::from_text("body");
        let extensions = media_extensions(&doc);
        let content_types = build_content_types(&extensions);
        let doc_rels = build_doc_rels(&doc);
        let original_ct = content_types.clone();
        let original_rels = doc_rels.clone();
        let other = vec![
            ("[Content_Types].xml".into(), content_types.into_bytes()),
            ("_rels/.rels".into(), DOT_RELS_XML.as_bytes().to_vec()),
            ("word/_rels/document.xml.rels".into(), doc_rels.into_bytes()),
            ("word/comments.xml".into(), comments_xml.as_bytes().to_vec()),
        ];
        let archive = DocxArchive {
            other_entries: other,
            document: doc.clone(),
        };

        let bytes = write_docx(&archive, &doc).expect("resave");
        let archive_back = read_docx(&bytes).expect("re-read");

        let raw_comments = archive_back
            .other_entries
            .iter()
            .find(|(n, _)| n == "word/comments.xml")
            .map(|(_, b)| b.as_slice())
            .expect("comments.xml still present");
        assert_eq!(
            raw_comments,
            comments_xml.as_bytes(),
            "comments.xml passthrough byte-identical"
        );

        let raw_ct = archive_back
            .other_entries
            .iter()
            .find(|(n, _)| n == "[Content_Types].xml")
            .map(|(_, b)| b.clone())
            .expect("content types present");
        assert_eq!(
            String::from_utf8(raw_ct).expect("utf-8"),
            original_ct,
            "Content_Types passthrough byte-identical"
        );

        let raw_rels = archive_back
            .other_entries
            .iter()
            .find(|(n, _)| n == "word/_rels/document.xml.rels")
            .map(|(_, b)| b.clone())
            .expect("rels present");
        assert_eq!(
            String::from_utf8(raw_rels).expect("utf-8"),
            original_rels,
            "rels passthrough byte-identical"
        );
    }

    /// L1.2 (#18) — `inject_content_type_override` and `inject_doc_rel`
    /// must be no-ops when the target row is already present, even if
    /// the rId or attribute order differs. Guards against duplicate
    /// emission on docs that hand-roll their own rels.
    #[test]
    fn opc_inject_helpers_are_idempotent_when_target_present() {
        let ct = "<?xml version=\"1.0\"?>\n\
            <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
            <Override PartName=\"/word/comments.xml\" ContentType=\"foo\"/>\
            </Types>";
        let after = inject_content_type_override(ct, COMMENTS_PART_NAME, COMMENTS_CONTENT_TYPE);
        assert!(matches!(after, Cow::Borrowed(_)), "no-op when present");

        let rels = "<?xml version=\"1.0\"?>\n\
            <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
            <Relationship Id=\"rId99\" Type=\"foo\" Target=\"comments.xml\"/>\
            </Relationships>";
        let after = inject_doc_rel(rels, "rId500", COMMENTS_REL_TYPE, COMMENTS_REL_TARGET);
        assert!(
            matches!(after, Cow::Borrowed(_)),
            "no-op when target present"
        );
    }

    /// L1.2 (#18) — `next_rel_id` must skip over the highest existing
    /// `rIdN` so the new row never collides.
    #[test]
    fn next_rel_id_picks_unused_value() {
        let rels = "<Relationships>\
            <Relationship Id=\"rId1\" .../>\
            <Relationship Id=\"rId7\" .../>\
            <Relationship Id=\"rId42\" .../>\
            </Relationships>";
        assert_eq!(next_rel_id(rels), 43);

        let no_rids = "<Relationships></Relationships>";
        assert_eq!(next_rel_id(no_rids), 1);
    }

    /// Sprint 13 (#12) — repeated `Bullet ↔ Number ↔ Bullet`
    /// toggles on the same paragraph must NOT inflate
    /// `numbering.xml`. The synth path reuses the matching template
    /// it minted on the first toggle; the writer's regenerated XML
    /// stays at exactly one bullet AbstractNum + one number
    /// AbstractNum + two Num instances regardless of toggle count.
    #[test]
    fn list_toggle_idempotency_via_round_trip() {
        use engine::numbering::ListSynthesisKind;
        use engine::{BlockPath, LogicalPos};
        let pos = || LogicalPos {
            path: BlockPath::top(0),
            offset: 0,
        };
        let mut doc = DocumentTree::from_text("toggle me");
        /* 20 toggles, alternating Bullet ↔ Number. */
        for i in 0..20 {
            let kind = if i % 2 == 0 {
                ListSynthesisKind::Bullet
            } else {
                ListSynthesisKind::Number
            };
            doc = doc.toggle_list_on_range(pos(), pos(), kind);
        }
        /* Exactly two AbstractNums + two Num instances — one of each
        kind, reused across every toggle. */
        assert_eq!(
            doc.numbering.abstract_nums.len(),
            2,
            "abstract_nums must stay at 2 (one Bullet + one Decimal) after repeated toggles"
        );
        assert_eq!(
            doc.numbering.num_instances.len(),
            2,
            "num_instances must stay at 2 after repeated toggles"
        );
        assert!(doc.numbering.dirty, "synth must flip dirty");
        /* Round-trip: write + read back; counts hold. */
        let bytes = build_minimal_docx(&doc).expect("build");
        let parsed = read_docx(&bytes).expect("re-read");
        assert_eq!(parsed.document.numbering.abstract_nums.len(), 2);
        assert_eq!(parsed.document.numbering.num_instances.len(), 2);
        /* The last toggle was Number (i=19 odd); paragraph 0's
        list_item should point at the Number-kind num_id. */
        let p = parsed.document.nth_paragraph(0).expect("paragraph 0");
        let li = p.list_item.expect("list_item on toggled paragraph");
        let abs_id = parsed
            .document
            .numbering
            .num_instances
            .get(&li.num_id)
            .map(|n| n.abstract_num_id)
            .expect("num_id resolves");
        let abs = parsed
            .document
            .numbering
            .abstract_nums
            .get(&abs_id)
            .expect("abstract resolves");
        assert!(matches!(
            abs.level(0).map(|l| &l.num_fmt),
            Some(engine::numbering::NumFmt::Decimal)
        ));
    }

    /// Sprint 12 (#11) — `Paragraph.style_id` survives a full `.docx`
    /// round-trip: writer emits `<w:pStyle w:val="...">` as the first
    /// `<w:pPr>` child; reader recovers it from `<w:pStyle>` back
    /// onto the new `Paragraph.style_id` field.
    #[test]
    fn paragraph_style_id_round_trips_via_p_style() {
        let para = Paragraph {
            text: "styled paragraph".into(),
            spans: Vec::new(),
            props: ParaProperties::default(),
            list_item: None,
            resolved_marker: None,
            dirty: true, // force regenerate (skip source_xml passthrough)
            source_xml: None,
            inline_objects: Vec::new(),
            hyperlinks: Vec::new(),
            revisions: Vec::new(),
            fields: Vec::new(),
            style_id: Some("Heading1".into()),
            direct_overrides: ParaProperties::default(),
        };
        let doc = DocumentTree::from_rich_paragraphs([para]);
        let bytes = build_minimal_docx(&doc).expect("build");
        /* Inspect the regenerated `document.xml` so the assertion is
        wire-shape-aware. */
        let archive = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).expect("zip");
        let raw = {
            let mut a = archive;
            let mut f = a.by_name("word/document.xml").expect("doc.xml");
            let mut s = String::new();
            use std::io::Read;
            f.read_to_string(&mut s).expect("read doc.xml");
            s
        };
        assert!(
            raw.contains("<w:pStyle w:val=\"Heading1\"/>"),
            "writer must emit <w:pStyle>: {raw}"
        );
        let reread = read_docx(&bytes).expect("re-read");
        let p = reread.document.nth_paragraph(0).expect("paragraph 0");
        assert_eq!(p.style_id.as_deref(), Some("Heading1"));
        assert_eq!(p.text, "styled paragraph");
    }
}
