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
        in-place. `resolve_markers_blocks` only writes
        `Paragraph::resolved_marker` and skips `Block::Table`, so tables
        preserve their identity. Phase 3 (#40) — section markers ride the
        paragraphs themselves and `body_section` never leaves `document`,
        so replacing the block list IS the whole rebuild (the old
        rebuild-via-`from_blocks_with_sections` zeroed unrelated fields
        and needed take/restore dances for sections + comment_ranges —
        the exact bug class issue #61 patched around). */
        let mut blocks: Vec<_> = document.blocks.iter().cloned().collect();
        resolve_markers_blocks(&mut blocks, &numbering);
        document.blocks = blocks.into_iter().collect();
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
    so a single header part shared across roles only parses once.
    Phase 3 (#40) — sections are derived from the paragraph markers +
    `body_section` on demand. */
    let sections = document.effective_sections();
    for section in &sections {
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
    /* Resolve hyperlink targets across every paragraph. Phase 3 (#40) —
    a straight block-list replacement: section markers ride the
    paragraphs (resolve_hyperlinks_block preserves every non-hyperlink
    field), and body_section / headers / footers / comment_ranges never
    leave `document`, so the old take/rebuild/restore dance (issue #61)
    is gone. */
    document.blocks = document
        .blocks
        .iter()
        .cloned()
        .map(|b| resolve_hyperlinks_block(b, &rels))
        .collect();
    document.media = media;

    /* Sprint 12 (#11) — mirror the `StyleTable` into the engine model
    so the live editor can apply / re-resolve styles without the
    format-docx crate. Paragraph styles only — character styles
    (`<w:rStyle>`) stay out of scope per the sprint plan. Re-stamped
    AFTER `from_blocks_with_sections` rebuilds since those rebuild
    constructors otherwise zero the field. */
    document.style_defaults = style_table.defaults.para.clone();
    document.style_run_defaults = style_table.defaults.run.clone();
    for (id, def) in &style_table.by_id {
        if !matches!(def.kind, crate::parts::styles::StyleKind::Paragraph) {
            continue;
        }
        document.styles.insert(
            id.clone(),
            engine::ParagraphStyle {
                id: id.clone(),
                name: id.clone(),
                based_on: def.based_on.clone(),
                para: def.para.clone(),
                run: def.run.clone(),
            },
        );
    }

    /* Sprint 13 (#12) — mirror the parsed `NumberingDefinitions` into
    the engine model so `Command::ToggleList` synthesis can reuse
    existing templates and the live editor can recompute markers
    without re-traversing `other_entries`. Source of truth for parse
    semantics remains `format-docx`; this is a value-copy. `.dirty`
    stays `false` — read-only ingest never bloats the on-disk part. */
    document.numbering = engine_numbering_from(&numbering);

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
    we just captured) and flip `resolved`. Issue #27 — the same pass
    resolves `w15:paraIdParent` (threaded replies) back to the parent
    comment's `w:id` by inverting the first_para_id map. Failures are
    silent — Word treats a malformed extended part as "no resolved
    comments". */
    if let Some(bytes) = other_entries
        .iter()
        .find(|(n, _)| n == COMMENTS_EXTENDED_XML)
        .map(|(_, b)| b.as_slice())
        && let Ok(entries) = parse_comments_extended_xml(bytes)
    {
        let lookup: std::collections::HashMap<String, (bool, Option<String>)> = entries
            .into_iter()
            .map(|e| (e.para_id, (e.done, e.parent_para_id)))
            .collect();
        let id_by_para: std::collections::HashMap<String, u32> = document
            .comment_defs
            .iter()
            .filter_map(|(id, c)| c.first_para_id.clone().map(|p| (p, *id)))
            .collect();
        for c in document.comment_defs.values_mut() {
            if let Some(pid) = c.first_para_id.as_deref()
                && let Some((done, parent_pid)) = lookup.get(pid)
            {
                c.resolved = *done;
                c.parent_id = parent_pid
                    .as_deref()
                    .and_then(|pp| id_by_para.get(pp).copied());
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

/// Sprint 13 (#12) — value-copy from format-docx's
/// `NumberingDefinitions` (the canonical OOXML parser) into the engine
/// mirror that lives on `DocumentTree.numbering`. Shapes are
/// parallel; the enum / struct mapping is line-by-line. `.dirty`
/// stays `false` so the writer keeps `numbering.xml` byte-identical
/// until an in-engine synth flips it.
fn engine_numbering_from(src: &NumberingDefinitions) -> engine::numbering::NumberingDefinitions {
    use crate::parts::numbering as fd;
    use engine::numbering as eg;
    fn cvt_fmt(f: &fd::NumFmt) -> eg::NumFmt {
        match f {
            fd::NumFmt::Decimal => eg::NumFmt::Decimal,
            fd::NumFmt::DecimalZero => eg::NumFmt::DecimalZero,
            fd::NumFmt::LowerLetter => eg::NumFmt::LowerLetter,
            fd::NumFmt::UpperLetter => eg::NumFmt::UpperLetter,
            fd::NumFmt::LowerRoman => eg::NumFmt::LowerRoman,
            fd::NumFmt::UpperRoman => eg::NumFmt::UpperRoman,
            fd::NumFmt::Bullet => eg::NumFmt::Bullet,
            fd::NumFmt::None => eg::NumFmt::None,
            fd::NumFmt::Other(s) => eg::NumFmt::Other(s.clone()),
        }
    }
    fn cvt_lvl(l: &fd::LvlDef) -> eg::LvlDef {
        eg::LvlDef {
            ilvl: l.ilvl,
            start: l.start,
            num_fmt: cvt_fmt(&l.num_fmt),
            lvl_text: l.lvl_text.clone(),
            lvl_restart: l.lvl_restart,
            indent: l.indent,
        }
    }
    let mut out = engine::numbering::NumberingDefinitions::default();
    for (id, abs) in &src.abstract_nums {
        out.abstract_nums.insert(
            *id,
            eg::AbstractNum {
                id: abs.id,
                levels: abs.levels.iter().map(cvt_lvl).collect(),
            },
        );
    }
    for (id, num) in &src.num_instances {
        out.num_instances.insert(
            *id,
            eg::NumInstance {
                num_id: num.num_id,
                abstract_num_id: num.abstract_num_id,
                overrides: num
                    .overrides
                    .iter()
                    .map(|o| eg::LvlOverride {
                        ilvl: o.ilvl,
                        start_override: o.start_override,
                        lvl: o.lvl.as_ref().map(cvt_lvl),
                    })
                    .collect(),
            },
        );
    }
    out
}
