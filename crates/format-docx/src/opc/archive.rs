//! `DocxArchive` — the pass-through ZIP container.
//!
//! The reader (`read_docx`) unzips a `.docx`, parses `word/document.xml` into
//! an `engine::DocumentTree`, and stashes **every** other entry verbatim in
//! `other_entries`. The writer (`crate::writer::write_docx`) repacks those
//! entries byte-identical alongside a freshly serialized `word/document.xml`.
//!
//! This is the foundation of the round-trip diff bound: parts we do not yet
//! model can never drift, because we re-emit them as raw bytes.

use crate::error::DocxError;
use crate::numbering_resolver::resolve_markers_blocks;
use crate::parts::comments::{parse_comments_extended_xml, parse_comments_xml};
use crate::parts::document::parse_document_xml;
use crate::parts::footer::parse_footer_xml;
use crate::parts::footnotes::parse_footnotes_xml;
use crate::parts::header::parse_header_xml;
use crate::parts::numbering::{NumberingDefinitions, parse_numbering_xml};
use crate::parts::rels::{parse_rels_xml, resolve_target};
use crate::parts::styles::{StyleTable, parse_styles_xml};
use crate::style_resolver::StyleResolver;
use engine::{DocumentTree, ImageBlob};
use std::collections::HashMap;
use std::io::{Cursor, Read};
use zip::ZipArchive;

pub const DOC_XML: &str = "word/document.xml";
pub const STYLES_XML: &str = "word/styles.xml";
pub const NUMBERING_XML: &str = "word/numbering.xml";
pub const RELS_XML: &str = "word/_rels/document.xml.rels";
pub const FOOTNOTES_XML: &str = "word/footnotes.xml";
pub const COMMENTS_XML: &str = "word/comments.xml";
pub const COMMENTS_EXTENDED_XML: &str = "word/commentsExtended.xml";
pub const SETTINGS_XML: &str = "word/settings.xml";

/// All raw archive entries except `word/document.xml`. Carried through the
/// round-trip so the writer can re-emit them verbatim.
#[derive(Debug, Clone)]
pub struct DocxArchive {
    /// `(entry_name, raw bytes)`. Order preserved from the original archive.
    pub other_entries: Vec<(String, Vec<u8>)>,
    /// Pre-parsed paragraphs from `word/document.xml`.
    pub document: DocumentTree,
}

impl DocxArchive {
    /// Look up a sibling part by exact archive entry name (e.g.
    /// `"word/styles.xml"`). Returns `None` if the part is absent.
    ///
    /// Useful to later phases that need to fetch a specific sibling —
    /// `parts::styles` reads `word/styles.xml`, `parts::numbering` reads
    /// `word/numbering.xml`, etc. — without scanning the full `Vec`.
    pub fn part_by_name(&self, name: &str) -> Option<&[u8]> {
        self.other_entries
            .iter()
            .find_map(|(n, b)| (n == name).then_some(b.as_slice()))
    }
}

/// Read a `.docx` byte blob → parsed document + stashed sibling entries.
pub fn read_docx(bytes: &[u8]) -> Result<DocxArchive, DocxError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut other_entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut document_xml: Option<Vec<u8>> = None;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_owned();
        let mut buf = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut buf)?;
        if name == DOC_XML {
            document_xml = Some(buf);
        } else {
            other_entries.push((name, buf));
        }
    }

    let xml = document_xml.ok_or_else(|| DocxError::MissingEntry(DOC_XML.into()))?;

    /* Phase 3 — `word/styles.xml` rides the pass-through but feeds the
    cascade resolver. Absent or malformed → empty table (all paragraphs
    just see direct formatting; behaviour matches pre-Phase-3). */
    let style_table: StyleTable = match other_entries
        .iter()
        .find(|(n, _)| n == STYLES_XML)
        .map(|(_, b)| parse_styles_xml(b))
    {
        Some(Ok(t)) => t,
        _ => StyleTable::default(),
    };
    let resolver = StyleResolver::new(&style_table);
    let mut document = parse_document_xml(&xml, &resolver)?;

    /* Phase 4 — `word/numbering.xml` rides the pass-through and feeds the
    numbering resolver. Second pass over the parsed paragraphs fills each
    list paragraph's `resolved_marker`. */
    let numbering: NumberingDefinitions = match other_entries
        .iter()
        .find(|(n, _)| n == NUMBERING_XML)
        .map(|(_, b)| parse_numbering_xml(b))
    {
        Some(Ok(t)) => t,
        _ => NumberingDefinitions::default(),
    };
    if !numbering.num_instances.is_empty() {
        /* `im::Vector` clones cheaply; we collect into a Vec to mutate
        in-place, then rebuild the tree. `resolve_markers_blocks` only
        writes `Paragraph::resolved_marker` and skips `Block::Table`,
        so structurally-shared fields cost nothing here and tables
        preserve their identity. Phase 6 — preserve the section table
        the document parser collected from `<w:sectPr>`. */
        let mut blocks: Vec<_> = document.blocks.iter().cloned().collect();
        resolve_markers_blocks(&mut blocks, &numbering);
        let sections = std::mem::take(&mut document.sections);
        document = DocumentTree::from_blocks_with_sections(blocks, sections);
    }

    /* Phase 6b — header / footer wiring. The rels table maps each
    `r:id` in `<w:headerReference>` / `<w:footerReference>` to the
    archive entry holding the part. Resolve every ref the document's
    sections carry and parse the corresponding header / footer XML.
    Unknown refs (target missing or rels missing) silently fall back
    to an empty band so a partial archive still renders. */
    let rels = other_entries
        .iter()
        .find(|(n, _)| n == RELS_XML)
        .and_then(|(_, b)| parse_rels_xml(b).ok())
        .unwrap_or_default();
    let mut headers: HashMap<String, Vec<engine::Paragraph>> = HashMap::new();
    let mut footers: HashMap<String, Vec<engine::Paragraph>> = HashMap::new();
    let fetch_part = |rid: &str| -> Option<&[u8]> {
        let target = rels.get(rid)?;
        let entry = resolve_target(target);
        other_entries
            .iter()
            .find(|(n, _)| n == &entry)
            .map(|(_, b)| b.as_slice())
    };
    /* Phase 2 audit — sections now carry a per-role
    `HeaderFooterRefs` instead of a single `Option<String>`; resolve
    every populated slot so the `default` / `first` / `even` parts
    all land in the headers/footers maps. The map is keyed by `r:id`
    so a single header part shared across roles only parses once. */
    for section in &document.sections {
        for rid in [
            section.header_refs.default.as_deref(),
            section.header_refs.first.as_deref(),
            section.header_refs.even.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if !headers.contains_key(rid)
                && let Some(bytes) = fetch_part(rid)
                && let Ok(part) = parse_header_xml(bytes, &resolver)
            {
                headers.insert(rid.to_string(), part.paragraphs);
            }
        }
        for rid in [
            section.footer_refs.default.as_deref(),
            section.footer_refs.first.as_deref(),
            section.footer_refs.even.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if !footers.contains_key(rid)
                && let Some(bytes) = fetch_part(rid)
                && let Ok(part) = parse_footer_xml(bytes, &resolver)
            {
                footers.insert(rid.to_string(), part.paragraphs);
            }
        }
    }
    document = document.with_header_footer_parts(headers, footers);

    // Phase 7 — pull image blobs out of word/media/ keyed by the
    // relationship id every inline image's <a:blip r:embed="..."/>
    // references. Also rewrite each hyperlink's `target` field from its
    // rId to the actual URL the rels table holds (the document parser
    // leaves the rId in place until rels are available).
    let mut media: std::collections::HashMap<String, ImageBlob> = std::collections::HashMap::new();
    /* Iterate every relationship; for each image media entry, fetch the
    blob bytes via the same resolver header/footer used. The
    relationship `Type` field would be the canonical filter
    (`.../image`), but the parser is lenient — any rel pointing into
    `word/media/` is treated as media. */
    for (rid, target) in &rels {
        let entry = resolve_target(target);
        if !entry.starts_with("word/media/") {
            continue;
        }
        if let Some(bytes) = other_entries
            .iter()
            .find(|(n, _)| n == &entry)
            .map(|(_, b)| b.clone())
        {
            let content_type = guess_image_mime(&entry).to_string();
            media.insert(
                rid.clone(),
                ImageBlob {
                    content_type,
                    data: bytes,
                },
            );
        }
    }
    /* Resolve hyperlink targets across every paragraph. */
    let blocks: Vec<_> = document
        .blocks
        .iter()
        .cloned()
        .map(|b| resolve_hyperlinks_block(b, &rels))
        .collect();
    let sections = std::mem::take(&mut document.sections);
    let headers = std::mem::take(&mut document.headers);
    let footers = std::mem::take(&mut document.footers);
    document = DocumentTree::from_blocks_with_sections(blocks, sections)
        .with_header_footer_parts(headers, footers);
    document.media = media;

    // Phase 8a — parse footnotes.xml + comments.xml if present, attach
    // to the document. Both XML parts still ride other_entries verbatim
    // so the passthrough writer round-trips them byte-identical.
    if let Some(bytes) = other_entries
        .iter()
        .find(|(n, _)| n == FOOTNOTES_XML)
        .map(|(_, b)| b.as_slice())
        && let Ok(table) = parse_footnotes_xml(bytes)
    {
        document.footnotes = table.footnotes;
    }
    if let Some(bytes) = other_entries
        .iter()
        .find(|(n, _)| n == COMMENTS_XML)
        .map(|(_, b)| b.as_slice())
        && let Ok(defs) = parse_comments_xml(bytes)
    {
        document.comment_defs = defs.comments;
    }

    /* Sprint 9 — second pass: `word/commentsExtended.xml` carries the
    `w15:done` resolved bit, keyed by `w15:paraId`. Map each entry's
    paraId back to its owning `CommentDef` (via the `first_para_id`
    we just captured) and flip `resolved`. Failures are silent — Word
    treats a malformed extended part as "no resolved comments". */
    if let Some(bytes) = other_entries
        .iter()
        .find(|(n, _)| n == COMMENTS_EXTENDED_XML)
        .map(|(_, b)| b.as_slice())
        && let Ok(entries) = parse_comments_extended_xml(bytes)
    {
        let lookup: std::collections::HashMap<String, bool> = entries.into_iter().collect();
        for c in document.comment_defs.values_mut() {
            if let Some(pid) = c.first_para_id.as_deref()
                && let Some(done) = lookup.get(pid).copied()
            {
                c.resolved = done;
            }
        }
    }

    /* Phase 2 audit — `word/settings.xml` rides `other_entries`
    verbatim for round-trip; the typed read just lifts the
    `even_and_odd_headers` toggle the paginator needs. */
    if let Some(bytes) = other_entries
        .iter()
        .find(|(n, _)| n == SETTINGS_XML)
        .map(|(_, b)| b.as_slice())
        && let Ok(settings) = crate::parts::settings::parse_settings_xml(bytes)
    {
        document.settings.even_and_odd_headers = settings.even_and_odd_headers;
    }

    Ok(DocxArchive {
        other_entries,
        document,
    })
}

/// Guess a MIME type from a `word/media/*` archive entry name. The OOXML
/// spec routes media discovery through the rels `Type` attribute; this
/// helper is the fallback for archives that drop the `Type` (or for
/// future formats the parser does not catalogue yet). Defaults to
/// `application/octet-stream` so the renderer's `createImageBitmap` can
/// still try.
fn guess_image_mime(entry: &str) -> &'static str {
    let lower = entry.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match lower.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

/// Walk a block, rewriting every paragraph's hyperlink `target` from the
/// parser's rId placeholder to the URL the rels table holds. Hyperlinks
/// whose rId is missing from rels are dropped (defensive — a hyperlink
/// without a target is not useful).
fn resolve_hyperlinks_block(
    block: engine::Block,
    rels: &std::collections::HashMap<String, String>,
) -> engine::Block {
    match block {
        engine::Block::Paragraph(mut p) => {
            p.hyperlinks = p
                .hyperlinks
                .into_iter()
                .filter_map(|h| {
                    rels.get(&h.target).map(|url| engine::Hyperlink {
                        start: h.start,
                        end: h.end,
                        target: url.clone(),
                    })
                })
                .collect();
            engine::Block::Paragraph(p)
        }
        engine::Block::Table(mut t) => {
            for row in t.rows.iter_mut() {
                for cell in row.cells.iter_mut() {
                    cell.blocks = std::mem::take(&mut cell.blocks)
                        .into_iter()
                        .map(|b| resolve_hyperlinks_block(b, rels))
                        .collect();
                }
            }
            engine::Block::Table(t)
        }
    }
}
