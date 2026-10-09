//! Issue #360 — the document-model side table `format_pdf::
//! export_pdf_document` consumes: what the PDF needs (headings for the
//! outline, hyperlinks and bookmarks for link annotations, core
//! properties for `/Info` + XMP) that the layout box tree does not carry.
//!
//! The table is indexed like `do_export_pdf`'s `para_texts` — by
//! `ParagraphBox::source_paragraph_id` — so every walk here mirrors the
//! `walk_block_texts` call it sits beside, entry for entry.

use format_pdf::{LinkSpan, LinkTarget, ParagraphSemantics};

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
    match block {
        engine::Block::Paragraph(p) => out.push(paragraph_semantics(doc, p, story)),
        engine::Block::Table(t) => {
            for row in &t.rows {
                for cell in &row.cells {
                    if cell.props.v_merge == engine::VMergeRole::Continue {
                        continue;
                    }
                    for b in &cell.blocks {
                        walk_block_semantics(doc, b, story, out);
                    }
                }
            }
        }
    }
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
    }
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
    engine::GrabBag::fragments_of(&doc.style_run_defaults.grab_bag)
        .iter()
        .find_map(|frag| {
            let xml = String::from_utf8_lossy(frag);
            let tag = xml.trim_start().strip_prefix("<w:lang")?;
            if !tag.starts_with(|c: char| c.is_whitespace() || c == '/' || c == '>') {
                return None;
            }
            let end = tag.find('>').unwrap_or(tag.len());
            attr(&tag[..end], key).filter(|v| !v.trim().is_empty())
        })
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
