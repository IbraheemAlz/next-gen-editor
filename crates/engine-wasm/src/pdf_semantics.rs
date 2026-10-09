//! Issue #360 — the document-model side table `format_pdf::
//! export_pdf_document` consumes: what the PDF needs (headings for the
//! outline, hyperlinks and bookmarks for link annotations, core
//! properties for `/Info` + XMP; list membership, enclosing table cells,
//! paragraph languages and picture alt text for the tagged export) that
//! the layout box tree does not carry.
//!
//! The table is indexed like `do_export_pdf`'s `para_texts` — by
//! `ParagraphBox::source_paragraph_id` — so every walk here mirrors the
//! `walk_block_texts` call it sits beside, entry for entry.

use format_pdf::{
    CellSemantics, LinkSpan, LinkTarget, ListSemantics, ObjectSemantics, ParagraphSemantics,
};

/// Which document facts a story contributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Story {
    /// The main body: headings feed the outline.
    Body,
    /// A footnote / endnote story: links and bookmarks only (a heading
    /// style inside a note is not a document heading — the TOC agrees).
    Note,
}

/// One entry per paragraph of `block`, in `walk_block_texts` order (table
/// rows × cells × cell blocks, `VMergeRole::Continue` cells skipped).
pub(crate) fn walk_block_semantics(
    doc: &engine::DocumentTree,
    block: &engine::Block,
    story: Story,
    out: &mut Vec<ParagraphSemantics>,
) {
    walk_in_cells(doc, block, story, &mut Vec::new(), out);
}

/// [`walk_block_semantics`] with the stack of table cells enclosing
/// `block` (outermost first) — every paragraph records it for tagging.
fn walk_in_cells(
    doc: &engine::DocumentTree,
    block: &engine::Block,
    story: Story,
    cells: &mut Vec<CellSemantics>,
    out: &mut Vec<ParagraphSemantics>,
) {
    match block {
        engine::Block::Paragraph(p) => {
            let mut sem = paragraph_semantics(doc, p, story);
            sem.cells = cells.clone();
            out.push(sem);
        }
        engine::Block::Table(t) => {
            /* Any value unique among sibling tables at this depth: the
            side-table index of the table's first paragraph. */
            let table = out.len() as u32;
            let row_spans = row_spans(t);
            for (r, row) in t.rows.iter().enumerate() {
                for (c, cell) in row.cells.iter().enumerate() {
                    if cell.props.v_merge == engine::VMergeRole::Continue {
                        continue;
                    }
                    cells.push(CellSemantics {
                        table,
                        row: r as u32,
                        cell: c as u32,
                        header: row.props.header,
                        col_span: u32::from(cell.props.grid_span.max(1)),
                        row_span: row_spans[r][c],
                    });
                    for b in &cell.blocks {
                        walk_in_cells(doc, b, story, cells, out);
                    }
                    cells.pop();
                }
            }
        }
    }
}

/// Rows each cell spans: a `w:vMerge` restart plus every `Continue` cell
/// directly below it in the same grid column (1 for everything else).
fn row_spans(t: &engine::Table) -> Vec<Vec<u32>> {
    /* Grid column of every cell (the spans of the cells before it). */
    let cols: Vec<Vec<(usize, &engine::TableCell)>> = t
        .rows
        .iter()
        .map(|row| {
            let mut col = 0usize;
            row.cells
                .iter()
                .map(|cell| {
                    let at = col;
                    col += usize::from(cell.props.grid_span.max(1));
                    (at, cell)
                })
                .collect()
        })
        .collect();
    cols.iter()
        .enumerate()
        .map(|(r, row)| {
            row.iter()
                .map(|&(col, cell)| {
                    if cell.props.v_merge != engine::VMergeRole::Restart {
                        return 1;
                    }
                    let below = cols[r + 1..]
                        .iter()
                        .take_while(|next| {
                            next.iter().any(|&(c, x)| {
                                c == col && x.props.v_merge == engine::VMergeRole::Continue
                            })
                        })
                        .count();
                    1 + below as u32
                })
                .collect()
        })
        .collect()
}

/// The semantics of one paragraph.
fn paragraph_semantics(
    doc: &engine::DocumentTree,
    p: &engine::Paragraph,
    story: Story,
) -> ParagraphSemantics {
    let heading = match story {
        Story::Body => doc.outline_heading_level(p),
        Story::Note => None,
    };
    ParagraphSemantics {
        heading,
        title: heading
            .map(|_| engine::toc::entry_text(p))
            .unwrap_or_default(),
        links: p
            .hyperlinks
            .iter()
            .filter(|h| h.start < h.end)
            .map(|h| LinkSpan {
                start: h.start,
                end: h.end,
                target: match h.target.strip_prefix('#') {
                    Some(name) => LinkTarget::Bookmark(name.to_string()),
                    None => LinkTarget::Uri(h.target.clone()),
                },
            })
            .collect(),
        bookmarks: bookmark_names(p),
        list: p.list_item.as_ref().map(|li| ListSemantics {
            level: li.ilvl,
            marker: p.resolved_marker.clone().unwrap_or_default(),
        }),
        cells: Vec::new(),
        lang: paragraph_lang(doc, p),
        objects: p
            .inline_objects
            .iter()
            .filter_map(|o| {
                let (name, descr) = o.image_label()?;
                Some(ObjectSemantics {
                    at: o.at,
                    alt: descr.or(name),
                    decorative: is_decorative(o),
                })
            })
            .collect(),
    }
}

/// Whether `c` is an Arabic-script letter (Arabic, Arabic Supplement,
/// Arabic Extended-A/B and the presentation forms).
fn is_arabic(c: char) -> bool {
    matches!(c,
        '\u{0600}'..='\u{06FF}'
        | '\u{0750}'..='\u{077F}'
        | '\u{0870}'..='\u{08FF}'
        | '\u{FB50}'..='\u{FDFF}'
        | '\u{FE70}'..='\u{FEFF}')
        && c.is_alphabetic()
}

/// The paragraph's natural language for a tagged export's `/Lang`: the
/// explicit run language (`<w:lang>`, an unmodeled grab-bag child — see
/// issue #344) covering most of the paragraph's dominant script — its
/// `w:bidi` attribute for Arabic-script text, `w:val` otherwise — else,
/// for an Arabic paragraph, the document default's `w:bidi`, else `ar`.
/// `None` (inherit the document's) for anything else; `format_pdf` also
/// drops a value equal to the document's.
pub(crate) fn paragraph_lang(doc: &engine::DocumentTree, p: &engine::Paragraph) -> Option<String> {
    let letters = |range: std::ops::Range<usize>, arabic: bool| {
        p.text
            .get(range)
            .unwrap_or("")
            .chars()
            .filter(|&c| c.is_alphabetic() && is_arabic(c) == arabic)
            .count()
    };
    let arabic_n = letters(0..p.text.len(), true);
    let other_n = letters(0..p.text.len(), false);
    if arabic_n == 0 && other_n == 0 {
        return None;
    }
    let arabic = arabic_n > other_n;
    let key = if arabic { "w:bidi" } else { "w:val" };
    let mut weights: Vec<(String, usize)> = Vec::new();
    for span in &p.spans {
        let Some(lang) = run_lang(&span.style.grab_bag, key) else {
            continue;
        };
        let n = letters(span.start as usize..span.end as usize, arabic);
        match weights.iter_mut().find(|(l, _)| *l == lang) {
            Some((_, w)) => *w += n,
            None => weights.push((lang, n)),
        }
    }
    let explicit = weights
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .max_by_key(|(_, n)| *n)
        .map(|(l, _)| l);
    explicit
        .or_else(|| arabic.then(|| default_lang(doc, "w:bidi").unwrap_or_else(|| "ar".to_string())))
}

/// Attribute `key` of the `<w:lang>` child in a run-properties grab bag.
fn run_lang(bag: &Option<Box<engine::GrabBag>>, key: &str) -> Option<String> {
    engine::GrabBag::fragments_of(bag).iter().find_map(|frag| {
        let xml = String::from_utf8_lossy(frag);
        let tag = xml.trim_start().strip_prefix("<w:lang")?;
        if !tag.starts_with(|c: char| c.is_whitespace() || c == '/' || c == '>') {
            return None;
        }
        let end = tag.find('>').unwrap_or(tag.len());
        attr(&tag[..end], key).filter(|v| !v.trim().is_empty())
    })
}

/// Whether a picture is marked decorative (`<wp:docPr>`'s
/// `<adec:decorative val="1"/>` extension, Office 2019+): such a picture
/// is an artifact in a tagged export, not a `Figure`.
fn is_decorative(o: &engine::InlineObject) -> bool {
    let from_anchor = o.anchor.as_deref().and_then(|a| a.doc_pr_xml.as_deref());
    let source = o
        .source_xml
        .as_deref()
        .and_then(|b| core::str::from_utf8(b).ok());
    from_anchor
        .into_iter()
        .chain(source)
        .find_map(doc_pr_element)
        .is_some_and(|el| {
            let mut rest = el;
            while let Some(at) = rest.find(":decorative") {
                let tag = &rest[at + ":decorative".len()..];
                let end = tag.find('>').unwrap_or(tag.len());
                if tag.starts_with(|c: char| c.is_whitespace() || c == '/' || c == '>')
                    && attr(&tag[..end], "val").is_some_and(|v| v == "1" || v == "true")
                {
                    return true;
                }
                rest = &tag[end..];
            }
            false
        })
}

/// The whole `<wp:docPr …>…</wp:docPr>` element (or the self-closing tag)
/// in `xml`.
fn doc_pr_element(xml: &str) -> Option<&str> {
    const OPEN: &str = "<wp:docPr";
    let mut from = 0;
    while let Some(rel) = xml[from..].find(OPEN) {
        let start = from + rel;
        let after = start + OPEN.len();
        if xml[after..].starts_with(|c: char| c.is_whitespace() || c == '/' || c == '>') {
            let close = after + xml[after..].find('>')?;
            if xml[..close].ends_with('/') {
                return Some(&xml[start..=close]);
            }
            let end = xml[close..]
                .find("</wp:docPr>")
                .map_or(xml.len(), |e| close + e + "</wp:docPr>".len());
            return Some(&xml[start..end]);
        }
        from = after;
    }
    None
}

/// The document information `format_pdf` writes as `/Info` + XMP: the
/// source package's core-properties part (`docProps/core.xml`, located
/// through the package-root relationship — the part itself rides the OPC
/// passthrough verbatim; this only reads it), with the modeled
/// `settings.author` as the author fallback (a document whose package was
/// detached). The language is `dc:language`, else the `<w:docDefaults>`
/// run properties' `<w:lang w:val>` (an unmodeled grab-bag child — see
/// issue #344).
pub(crate) fn document_metadata(doc: &engine::DocumentTree) -> format_pdf::DocumentMetadata {
    let core = doc
        .source_package
        .as_deref()
        .and_then(|pkg| {
            let get = |name: &str| pkg.entry(name);
            let name = format_docx::opc::part_names::PartNames::core_props_name(&get);
            pkg.entry(&name)
        })
        .and_then(|xml| format_docx::parts::core_props::parse_core_props_xml(xml).ok())
        .unwrap_or_default();
    format_pdf::DocumentMetadata {
        title: core.title,
        author: core.creator.or_else(|| doc.settings.author.clone()),
        subject: core.subject,
        keywords: core.keywords,
        lang: core.language.or_else(|| default_lang(doc, "w:val")),
    }
}

/// An attribute (`w:val` / `w:bidi` / `w:eastAsia`) of the `<w:lang>`
/// element in the document's default run properties, when present.
pub(crate) fn default_lang(doc: &engine::DocumentTree, key: &str) -> Option<String> {
    run_lang(&doc.style_run_defaults.grab_bag, key)
}

/// Every bookmark name anchored in `p`: the modeled `_Toc*` bookmarks
/// (issue #81), the `<w:bookmarkStart>` markers riding the paragraph's
/// source markup (issues #199 / #106 — every other in-paragraph bookmark),
/// and the block-level ones just before it (issue #120). A bookmark that
/// opens mid-paragraph still resolves to the paragraph's top — a PDF
/// `/Dest` is a page position, and the paragraph top is where a reader
/// jumping to it wants to land.
pub(crate) fn bookmark_names(p: &engine::Paragraph) -> Vec<String> {
    let mut out: Vec<String> = p.bookmarks.iter().map(|b| b.name.clone()).collect();
    if let Some(m) = p.source_markup.as_deref() {
        for marker in &m.markers {
            bookmark_names_in(&marker.xml, &mut out);
        }
    }
    if let Some(body) = p.body_xml.as_deref() {
        for frag in &body.before {
            if let engine::BodyFragment::Verbatim { xml } = frag {
                bookmark_names_in(xml, &mut out);
            }
        }
    }
    out.dedup();
    out
}

/// Append the `w:name` of every `<w:bookmarkStart …>` in `xml`.
fn bookmark_names_in(xml: &[u8], out: &mut Vec<String>) {
    const OPEN: &[u8] = b"<w:bookmarkStart";
    let mut from = 0;
    while let Some(rel) = xml[from..].windows(OPEN.len()).position(|w| w == OPEN) {
        let start = from + rel + OPEN.len();
        let Some(end) = xml[start..].iter().position(|&b| b == b'>') else {
            return;
        };
        let tag = String::from_utf8_lossy(&xml[start..start + end]);
        if let Some(name) = attr(&tag, "w:name")
            && !name.is_empty()
            && !out.contains(&name)
        {
            out.push(name);
        }
        from = start + end;
    }
}

/// The unescaped value of attribute `key` in a start tag's attribute text.
fn attr(tag: &str, key: &str) -> Option<String> {
    let mut rest = tag;
    while let Some(at) = rest.find(key) {
        let before_ok = at == 0 || rest[..at].ends_with(char::is_whitespace);
        let after = rest[at + key.len()..].trim_start();
        if before_ok && let Some(after) = after.strip_prefix('=') {
            let after = after.trim_start();
            let quote = after.chars().next()?;
            if quote == '"' || quote == '\'' {
                let body = &after[1..];
                let end = body.find(quote)?;
                return Some(unescape(&body[..end]));
            }
        }
        rest = &rest[at + key.len()..];
    }
    None
}

/// The five predefined XML entities (attribute values in a bookmark name).
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bookmark_names_come_from_model_markers_and_block_fragments() {
        let mut p = engine::Paragraph {
            text: "Body".into(),
            bookmarks: vec![engine::Bookmark {
                name: "_Toc1".into(),
                id: Some(1),
            }],
            ..Default::default()
        };
        p.source_markup = Some(Box::new(engine::SourceMarkup {
            text_len: 4,
            markers: vec![engine::SourceMarker::verbatim(
                2,
                br#"<w:bookmarkStart w:id="7" w:name="Mid&amp;Way"/>"#.to_vec(),
            )],
            ..Default::default()
        }));
        p.body_xml = Some(Box::new(engine::BodyPassthrough {
            before: vec![engine::BodyFragment::Verbatim {
                xml: br#"<w:bookmarkStart w:name='Before' w:id="3"/><w:bookmarkEnd w:id="3"/>"#
                    .to_vec(),
            }],
            after: Vec::new(),
        }));
        assert_eq!(bookmark_names(&p), ["_Toc1", "Mid&Way", "Before"]);
    }
}
