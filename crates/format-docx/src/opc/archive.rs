//! `DocxArchive` — the pass-through ZIP container.
//!
//! The reader (`read_docx`) unzips a `.docx`, parses `word/document.xml` into
//! an `engine::DocumentTree`, and stashes **every** other entry verbatim in
//! `other_entries`. The writer (`crate::writer::write_docx`) repacks those
//! entries byte-identical alongside a freshly serialized `word/document.xml`.
//!
//! This is the foundation of the round-trip diff bound: parts we do not yet
//! model can never drift, because we re-emit them as raw bytes.

use crate::error::{DocxError, DocxWarning};
use crate::numbering_resolver::resolve_markers_blocks;
use crate::opc::limits::{
    PackageLimits, check_xml_part, is_walked_xml_part, read_entry_bounded, read_package_entries,
};
use crate::opc::part_names::{PartNames, rels_entry_name, target_candidates};
use crate::parts::comments::{parse_comments_extended_xml, parse_comments_xml};
use crate::parts::document::parse_document_xml_with_warnings;
use crate::parts::endnotes::parse_endnotes_xml;
use crate::parts::footer::parse_footer_xml;
use crate::parts::footnotes::parse_footnotes_xml;
use crate::parts::header::parse_header_xml;
use crate::parts::numbering::{NumberingDefinitions, parse_numbering_xml};
use crate::parts::rels::{parse_rels_xml, resolve_target};
use crate::parts::styles::{StyleTable, parse_styles_xml};
use crate::style_resolver::StyleResolver;
use engine::{DocumentTree, ImageBlob};
use std::collections::HashMap;
use std::io::Cursor;
use zip::ZipArchive;

pub const DOC_XML: &str = "word/document.xml";
pub const STYLES_XML: &str = "word/styles.xml";
pub const NUMBERING_XML: &str = "word/numbering.xml";
pub const RELS_XML: &str = "word/_rels/document.xml.rels";
pub const FOOTNOTES_XML: &str = "word/footnotes.xml";
pub const ENDNOTES_XML: &str = "word/endnotes.xml";
pub const COMMENTS_XML: &str = "word/comments.xml";
pub const COMMENTS_EXTENDED_XML: &str = "word/commentsExtended.xml";
/// Issue #282 — Word 2019+ comment side parts keyed by paraId / durable id.
pub const COMMENTS_IDS_XML: &str = "word/commentsIds.xml";
pub const COMMENTS_EXTENSIBLE_XML: &str = "word/commentsExtensible.xml";
pub const SETTINGS_XML: &str = "word/settings.xml";
/// Issue #355 — where Word puts the theme (`a:theme`); read-only (font +
/// colour schemes), always passthrough.
pub const THEME_XML: &str = "word/theme/theme1.xml";
/// Issue #77 — OPC core properties; `<dc:creator>` feeds the `AUTHOR`
/// field. Passthrough-only (never regenerated).
pub const CORE_PROPS_XML: &str = "docProps/core.xml";

/// All raw archive entries except `word/document.xml`. Carried through the
/// round-trip so the writer can re-emit them verbatim.
#[derive(Debug, Clone)]
pub struct DocxArchive {
    /// `(entry_name, raw bytes)`. Order preserved from the original archive.
    pub other_entries: Vec<(String, Vec<u8>)>,
    /// Pre-parsed paragraphs from `word/document.xml`.
    pub document: DocumentTree,
    /// Issue #84 — every attribute of the source `<w:document>` start tag,
    /// `(name, escaped value)` in document order: the `xmlns:*` bindings
    /// (Word declares ~30, `w14` / `w15` / `mc` / … among them) plus
    /// `mc:Ignorable`. The writer synthesizes its own root element and
    /// re-declares these on it, so passthrough paragraphs (`w14:paraId`
    /// on every Word-authored `<w:p>`) and preserved grab-bag fragments
    /// stay namespace-well-formed. Empty for engine-authored archives.
    pub document_root_attrs: Vec<(String, String)>,
    /// Non-fatal reader diagnostics raised while parsing the package
    /// (issue #111 — a table nested past
    /// `parts::table::MAX_TABLE_NESTING_DEPTH` kept as an opaque block;
    /// issue #349 — an unusable or clamped measure; issue #350 — an
    /// unbalanced or too deeply nested field). Empty when every part landed
    /// in the typed model as written.
    pub warnings: Vec<DocxWarning>,
    /// Issue #353 — where the main part and its special siblings live,
    /// discovered from the package's relationships (fixed `word/…` names
    /// are the fallback). The writer uses these, never the constants.
    pub part_names: PartNames,
}

/// Issue #110 — strict well-formedness check of a saved package's
/// `word/document.xml`, for the round-trip harnesses: re-parse the part
/// with quick-xml (end-tag names checked, comments checked, unmatched end
/// tags rejected) and require exactly one root element closed at EOF.
/// Any violation is an error — a writer that splices a misaligned
/// passthrough range produces a part this rejects, and our own reader
/// then cannot reopen what we just saved.
///
/// Issue #100 — the check is namespace-aware too: every element and
/// attribute prefix must resolve to an in-scope `xmlns:` binding. A
/// passthrough `<w:p w14:paraId=…>` spliced under a synthesized root
/// that forgot to re-declare `xmlns:w14` is well-formed XML 1.0 but NOT
/// namespace-well-formed, and Word refuses (or "repairs") the file.
pub fn check_document_xml_well_formed(docx: &[u8]) -> Result<(), DocxError> {
    /* Issue #353 — the main part is whatever `_rels/.rels` says. */
    let main = main_part_name(docx).unwrap_or_else(|| DOC_XML.to_string());
    check_part_xml_well_formed(docx, &main)
}

/// Issue #353 — the main part's entry name in a packaged `.docx`
/// (`_rels/.rels` → `officeDocument`), `None` when it cannot be resolved
/// to an entry the archive holds.
pub fn main_part_name(docx: &[u8]) -> Option<String> {
    let mut archive = ZipArchive::new(Cursor::new(docx)).ok()?;
    /* Issue #348 — bounded, never sized from the declared length. */
    let rels = read_entry_bounded(
        archive.by_name("_rels/.rels").ok()?,
        "_rels/.rels",
        &PackageLimits::DEFAULT,
        &mut 0,
    )
    .ok()?;
    let entries = vec![("_rels/.rels".to_string(), rels)];
    let names: Vec<String> = archive.file_names().map(str::to_owned).collect();
    PartNames::discover(&entries, &|n| names.iter().any(|e| e == n), &mut Vec::new())
        .ok()
        .map(|p| p.main)
}

/// [`check_document_xml_well_formed`] for any XML part of the package
/// (`word/header1.xml`, `word/footer2.xml`, …) — issue #100: regenerated
/// header/footer roots must re-declare the source root's bindings exactly
/// like `word/document.xml`.
pub fn check_part_xml_well_formed(docx: &[u8], part_name: &str) -> Result<(), DocxError> {
    use quick_xml::events::Event;
    use quick_xml::name::ResolveResult;
    use quick_xml::reader::NsReader;

    let mut archive = ZipArchive::new(Cursor::new(docx))?;
    let part = archive
        .by_name(part_name)
        .map_err(|_| DocxError::MissingEntry(part_name.into()))?;
    /* Issue #348 — never allocate from the declared size. */
    let xml = read_entry_bounded(part, part_name, &PackageLimits::DEFAULT, &mut 0)?;

    let mut reader = NsReader::from_reader(xml.as_slice());
    let config = reader.config_mut();
    config.trim_text(false);
    config.check_end_names = true;
    config.check_comments = true;
    config.allow_unmatched_ends = false;

    let unbound = |what: &str, qname: &[u8], pos: u64| {
        DocxError::MalformedXml(format!(
            "{part_name}: unbound namespace prefix on {what} `{}` at byte {pos}",
            String::from_utf8_lossy(qname)
        ))
    };
    let mut depth: usize = 0;
    let mut roots: usize = 0;
    let mut buf = Vec::new();
    loop {
        let (elem_ns, event) = reader.read_resolved_event_into(&mut buf)?;
        let elem_unbound = matches!(elem_ns, ResolveResult::Unknown(_));
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                if elem_unbound {
                    return Err(unbound(
                        "element",
                        e.name().as_ref(),
                        reader.buffer_position(),
                    ));
                }
                for a in e.attributes() {
                    let a = a?;
                    if let (ResolveResult::Unknown(_), _) = reader.resolve_attribute(a.key) {
                        return Err(unbound(
                            "attribute",
                            a.key.as_ref(),
                            reader.buffer_position(),
                        ));
                    }
                }
                if matches!(event, Event::Start(_)) {
                    if depth == 0 {
                        roots += 1;
                    }
                    depth += 1;
                } else if depth == 0 {
                    roots += 1;
                }
            }
            Event::End(_) => {
                depth = depth.checked_sub(1).ok_or_else(|| {
                    DocxError::MalformedXml(format!(
                        "unmatched end tag at byte {}",
                        reader.buffer_position()
                    ))
                })?;
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    if depth != 0 {
        return Err(DocxError::MalformedXml(format!(
            "{depth} element(s) still open at end of {part_name}"
        )));
    }
    if roots != 1 {
        return Err(DocxError::MalformedXml(format!(
            "{part_name} has {roots} root elements, expected exactly 1"
        )));
    }
    Ok(())
}

/// Attributes of the first element in `xml` (the part's root), raw escaped
/// values. Empty when the part has no element.
pub(crate) fn root_attributes(xml: &[u8]) -> Vec<(String, String)> {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                return e
                    .attributes()
                    .flatten()
                    .map(|a| {
                        (
                            String::from_utf8_lossy(a.key.as_ref()).into_owned(),
                            String::from_utf8_lossy(&a.value).into_owned(),
                        )
                    })
                    .collect();
            }
            Ok(Event::Eof) | Err(_) => return Vec::new(),
            _ => {}
        }
        buf.clear();
    }
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
/// Fallback page geometry for a `<w:sectPr>` missing `<w:pgSz>` is A4, and
/// a paragraph with no resolved `<w:widowControl>` anywhere in the cascade
/// reads as widow/orphan control ON (Word's application default) — see
/// [`read_docx_with_settings`] to override either.
pub fn read_docx(bytes: &[u8]) -> Result<DocxArchive, DocxError> {
    let defaults = engine::DocumentSettings::default();
    read_docx_with_settings(
        bytes,
        defaults.default_page_size,
        defaults.widow_control_default,
    )
}

/// [`read_docx`], with host-chosen fallbacks for two settings OOXML never
/// carries an element for — the values are stamped onto
/// `DocumentTree.settings` for inspection, never read FROM the archive:
///
/// - `default_page_size` ([`engine::DefaultPageSize`], issue #109) — the
///   fallback for any `<w:sectPr>` that omits `<w:pgSz>` (ECMA-376 requires
///   `pgSz`, but the Apache POI / docx4j "wild document" corpus ships files
///   that skip it). An embedding host that defaults new/underspecified
///   documents to US Letter calls this directly with
///   `engine::DefaultPageSize::Letter`.
/// - `widow_control_default` (issue #179) — the effective
///   `<w:widowControl>` for a paragraph whose resolved
///   `engine::ParaProperties::widow_control` is `None`. #95 chose `true`
///   (Word's application default); ECMA-376 itself reads an absent element
///   as "not applied" (`false`). A host that wants the strict spec reading
///   passes `false` here instead of stamping every paragraph.
///
/// `read_docx` itself always resolves both to their `#[default]`s (`A4`,
/// `true`), unchanged, so every pinned `layout::geometry_fingerprint`
/// fixture keeps its geometry.
pub fn read_docx_with_settings(
    bytes: &[u8],
    default_page_size: engine::DefaultPageSize,
    widow_control_default: bool,
) -> Result<DocxArchive, DocxError> {
    read_docx_with_limits(
        bytes,
        default_page_size,
        widow_control_default,
        &PackageLimits::DEFAULT,
    )
}

/// [`read_docx_with_settings`] under explicit resource bounds (issue
/// #348): the package is refused with [`DocxError::PackageTooLarge`] when
/// it holds more than `limits.max_entries` entries, when any entry (or the
/// package as a whole) inflates past its byte budget — every entry is read
/// through `take(limit + 1)`, the declared size is never trusted — or when
/// an XML part the reader walks nests deeper than `limits.max_xml_depth`
/// or holds more than `limits.max_xml_elements` elements. The other
/// entry points use [`PackageLimits::DEFAULT`].
pub fn read_docx_with_limits(
    bytes: &[u8],
    default_page_size: engine::DefaultPageSize,
    widow_control_default: bool,
    limits: &PackageLimits,
) -> Result<DocxArchive, DocxError> {
    /* Issue #349 — every part's non-fatal diagnostics (styles,
    headers, notes included) land on `DocxArchive::warnings`. */
    read_docx_with_password(
        bytes,
        None,
        default_page_size,
        widow_control_default,
        limits,
    )
}

/// Issue #345 — [`read_docx_with_limits`] that also opens an ENCRYPTED
/// package (MS-OFFCRYPTO agile or standard encryption, see
/// [`crate::opc::offcrypto`]) when `password` is given: the decrypted ZIP
/// is read exactly like a plain `.docx` (its declared size bounded by
/// `limits.max_total_bytes`). Without a password an encrypted package is
/// [`DocxError::Encrypted`]; a wrong one [`DocxError::WrongPassword`].
/// A `password` given for a plain package is ignored. The result carries
/// no trace of the encryption: saving it writes an unencrypted package.
pub fn read_docx_with_password(
    bytes: &[u8],
    password: Option<&str>,
    default_page_size: engine::DefaultPageSize,
    widow_control_default: bool,
    limits: &PackageLimits,
) -> Result<DocxArchive, DocxError> {
    let decrypted;
    let bytes = match (crate::opc::cfb::sniff_compound_file(bytes), password) {
        (Some(crate::opc::cfb::CompoundFileKind::EncryptedPackage), Some(pw)) => {
            decrypted = crate::opc::offcrypto::decrypt_package(bytes, pw, limits.max_total_bytes)?;
            decrypted.as_slice()
        }
        _ => bytes,
    };
    let mut part_warnings = Vec::new();
    let mut archive = crate::error::collect_read_warnings(&mut part_warnings, |_| {
        read_docx_scoped(bytes, default_page_size, widow_control_default, limits)
    })?;
    archive.warnings.extend(part_warnings);
    Ok(archive)
}

/// [`read_docx_with_limits`]'s body, run inside the warnings scope.
fn read_docx_scoped(
    bytes: &[u8],
    default_page_size: engine::DefaultPageSize,
    widow_control_default: bool,
    limits: &PackageLimits,
) -> Result<DocxArchive, DocxError> {
    /* Issue #345 — an OLE compound file is never a ZIP: an encrypted
    package (or a legacy binary document) gets its own typed refusal
    instead of "invalid Zip archive". */
    if let Some(kind) = crate::opc::cfb::sniff_compound_file(bytes) {
        return Err(match kind {
            crate::opc::cfb::CompoundFileKind::EncryptedPackage => DocxError::Encrypted,
            crate::opc::cfb::CompoundFileKind::Other => DocxError::CompoundFile,
        });
    }
    /* Issue #348 — every entry through the bounded reader (never an
    allocation from the declared size), then the XML shape bounds of every
    part the reader walks, before any typed walk. */
    let mut all_entries = read_package_entries(bytes, limits)?;
    for (name, buf) in &all_entries {
        if is_walked_xml_part(name) {
            check_xml_part(name, buf, limits)?;
        }
    }

    let mut warnings: Vec<DocxWarning> = Vec::new();
    /* Issue #353 — the main part and its siblings come from the package's
    relationships (fixed `word/…` names are the fallback). */
    let part_names = PartNames::discover(
        &all_entries,
        &|n| all_entries.iter().any(|(e, _)| e == n),
        &mut warnings,
    )?;
    let main_at = all_entries
        .iter()
        .position(|(n, _)| *n == part_names.main)
        .ok_or_else(|| DocxError::MissingEntry(part_names.main.clone()))?;
    let (_, xml) = all_entries.remove(main_at);
    let mut other_entries = all_entries;
    other_entries.shrink_to_fit();
    /* Issue #325 — validate the root's namespace bindings; a non-canonical
    spelling is normalised (regenerate-only) instead of reading empty. */
    let xml = canonical_main_part(&part_names.main, xml, &mut warnings);
    /* Issue #394 — the same for every WordprocessingML sibling the reader
    walks, before any of them is parsed. */
    canonical_sibling_parts(&mut other_entries, &part_names, &mut warnings);

    /* Phase 3 — `word/styles.xml` rides the pass-through but feeds the
    cascade resolver. Absent or malformed → empty table (all paragraphs
    just see direct formatting; behaviour matches pre-Phase-3). */
    let style_table: StyleTable = match other_entries
        .iter()
        .find(|(n, _)| *n == part_names.styles)
        .map(|(_, b)| parse_styles_xml(b))
    {
        Some(Ok(t)) => t,
        _ => StyleTable::default(),
    };
    let resolver = StyleResolver::new(&style_table);
    let mut document = parse_document_xml_with_warnings(
        &xml,
        &resolver,
        &mut warnings,
        default_page_size.geometry(),
    )?;
    document.settings.default_page_size = default_page_size;
    document.settings.widow_control_default = widow_control_default;

    /* Phase 4 — `word/numbering.xml` rides the pass-through and feeds the
    numbering resolver. Second pass over the parsed paragraphs fills each
    list paragraph's `resolved_marker`. */
    let numbering: NumberingDefinitions = match other_entries
        .iter()
        .find(|(n, _)| *n == part_names.numbering)
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
        .find(|(n, _)| *n == part_names.main_rels)
        .and_then(|(_, b)| parse_rels_xml(b).ok())
        .unwrap_or_default();
    let mut headers: HashMap<String, Vec<engine::Block>> = HashMap::new();
    let mut footers: HashMap<String, Vec<engine::Block>> = HashMap::new();
    let fetch_part = |rid: &str| -> Option<&[u8]> {
        let target = rels.get(rid)?;
        let entry = resolve_in_package(&other_entries, &part_names.main, target);
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
                headers.insert(rid.to_string(), part.blocks);
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
                footers.insert(rid.to_string(), part.blocks);
            }
        }
    }
    /* Issue #72 — post-process each header/footer part the way the
    body already is:
    - list markers: `resolve_markers_blocks` used to run only over
      `document.blocks` (and BEFORE the parts were even parsed), so a
      bulleted header rendered with `resolved_marker: None` forever;
    - hyperlinks: rel ids inside a part are scoped to the PART's OWN
      rels file (`word/_rels/headerN.xml.rels`), never to
      document.xml.rels — the body-level resolve pass could not reach
      or resolve them, leaving raw `rId` strings as targets. Resolve
      against the part-local table; unresolved links drop, exactly the
      body semantics. */
    /* Phase 7 / issue #188 — image blobs out of `word/media/`, keyed by
    the resolved TARGET entry name (`word/media/image2.png`), never by
    the bare relationship id: rel ids are scoped per part, so a header's
    `rId5` and the body's `rId5` may name different pictures. Each part
    registers its own rels (body here, header / footer below, notes
    further down) and every picture is stamped with the media key its
    part-local `r:embed` resolves to (`InlineKind::Image::media_key`). The
    relationship `Type` would be the canonical filter (`.../image`), but
    the pass is lenient — any rel resolving into `word/media/` counts. */
    let mut media: HashMap<String, ImageBlob> = HashMap::new();
    let body_media_keys = register_part_media(&other_entries, &part_names.main, &rels, &mut media);
    for (rid, blocks) in headers.iter_mut().chain(footers.iter_mut()) {
        if !numbering.num_instances.is_empty() {
            resolve_markers_blocks(blocks, &numbering);
        }
        if let Some(target) = rels.get(rid.as_str()) {
            let entry = resolve_in_package(&other_entries, &part_names.main, target);
            let rels_name = part_rels_entry_name(&entry);
            let part_rels = other_entries
                .iter()
                .find(|(n, _)| n == &rels_name)
                .and_then(|(_, b)| parse_rels_xml(b).ok())
                .unwrap_or_default();
            *blocks = blocks
                .iter()
                .cloned()
                .map(|b| resolve_hyperlinks_block(b, &part_rels))
                .collect();
            /* Issue #188 / #78 — part-local picture rels. */
            let keys = register_part_media(&other_entries, &entry, &part_rels, &mut media);
            stamp_media_keys(blocks, &keys);
        }
    }
    document = document.with_header_footer_parts(headers, footers);

    /* Resolve hyperlink targets across every paragraph (and stamp each
    body picture's media key). Phase 3 (#40) — a straight block-list
    replacement: section markers ride the paragraphs
    (resolve_hyperlinks_block preserves every non-hyperlink field), and
    body_section / headers / footers / comment_ranges never leave
    `document`, so the old take/rebuild/restore dance (issue #61) is
    gone. */
    document.blocks = document
        .blocks
        .iter()
        .cloned()
        .map(|b| {
            let mut b = resolve_hyperlinks_block(b, &rels);
            stamp_media_keys(std::slice::from_mut(&mut b), &body_media_keys);
            b
        })
        .collect();

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
                /* Issue #297 — the display `<w:name>`, never the id: a
                regenerated styles.xml writes this back, and Word's
                gallery shows it (`heading 1`, not `Heading1`). */
                name: def.name.clone().unwrap_or_default(),
                based_on: def.based_on.clone(),
                para: def.para.clone(),
                run: def.run.clone(),
                next: def.next.clone(),
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

    /* Issue #80 — note stories. `word/footnotes.xml` / `word/endnotes.xml`
    parse through the body pipeline (style spans, lists, hyperlinks,
    grab bags); both parts still ride `other_entries` verbatim so the
    writer passes them through byte-identical until a story is edited.
    Post-processing mirrors the header/footer parts: list markers
    resolve against `numbering.xml`; hyperlink rel ids resolve against
    the PART's own rels file (`word/_rels/footnotes.xml.rels`). */
    for (entry, kind) in [
        (part_names.footnotes.as_str(), engine::NoteKind::Footnote),
        (part_names.endnotes.as_str(), engine::NoteKind::Endnote),
    ] {
        let Some(bytes) = other_entries
            .iter()
            .find(|(n, _)| n == entry)
            .map(|(_, b)| b.as_slice())
        else {
            continue;
        };
        let parsed = match kind {
            engine::NoteKind::Footnote => parse_footnotes_xml(bytes, &resolver),
            engine::NoteKind::Endnote => parse_endnotes_xml(bytes, &resolver),
        };
        let Ok(part) = parsed else {
            continue;
        };
        /* Issue #100 — the note part's own root bindings (they can differ
        from the document root's) for the UI save path, which regenerates
        this part from the tree without the archive. */
        document
            .part_root_attrs
            .insert(entry.to_string(), part.root_attrs.clone());
        let rels_name = part_rels_entry_name(entry);
        let part_rels = other_entries
            .iter()
            .find(|(n, _)| n == &rels_name)
            .and_then(|(_, b)| parse_rels_xml(b).ok())
            .unwrap_or_default();
        /* Issue #188 — note pictures resolve against the note part's rels. */
        let note_media_keys = register_part_media(&other_entries, entry, &part_rels, &mut media);
        let mut stories: HashMap<i32, engine::NoteStory> = HashMap::with_capacity(part.notes.len());
        for mut story in part.notes {
            if !numbering.num_instances.is_empty() {
                resolve_markers_blocks(&mut story.body, &numbering);
            }
            story.body = story
                .body
                .into_iter()
                .map(|b| resolve_hyperlinks_block(b, &part_rels))
                .collect();
            stamp_media_keys(&mut story.body, &note_media_keys);
            stories.insert(story.id, story);
        }
        match kind {
            engine::NoteKind::Footnote => document.footnote_stories = stories,
            engine::NoteKind::Endnote => document.endnote_stories = stories,
        }
    }
    document.media = media;
    // Phase 8a — parse comments.xml if present, attach to the document.
    // The XML part still rides other_entries verbatim so the passthrough
    // writer round-trips it byte-identical.
    if let Some(bytes) = other_entries
        .iter()
        .find(|(n, _)| *n == part_names.comments)
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
        .find(|(n, _)| *n == part_names.comments_extended)
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
    let settings_part = other_entries
        .iter()
        .find(|(n, _)| *n == part_names.settings)
        .and_then(|(_, b)| crate::parts::settings::parse_settings_xml(b).ok());
    if let Some(settings) = &settings_part {
        document.settings.even_and_odd_headers = settings.even_and_odd_headers;
        /* Issue #345 — the editing restriction the engine enforces (the
        part's bytes still pass through verbatim). */
        document.settings.protection = settings.protection.clone();
        /* Issue #80 — document-level note properties. */
        document.footnote_props = settings.footnote_props;
        document.endnote_props = settings.endnote_props;
    }

    /* Issue #355 — the theme part rides `other_entries` verbatim; the
    typed read (+ the settings that select into it) feeds theme-font and
    theme-colour resolution at layout time. */
    document.theme = crate::parts::theme::read_document_theme(
        &other_entries,
        &part_names.theme,
        settings_part.as_ref(),
    )
    .map(std::sync::Arc::new);

    /* Issue #77 — `docProps/core.xml` rides `other_entries` verbatim;
    the typed read lifts `<dc:creator>` so `AUTHOR` fields resolve. */
    if let Some(bytes) = other_entries
        .iter()
        .find(|(n, _)| *n == part_names.core_props)
        .map(|(_, b)| b.as_slice())
        && let Ok(props) = crate::parts::core_props::parse_core_props_xml(bytes)
    {
        document.settings.author = props.creator;
    }

    let document_root_attrs = root_attributes(&xml);
    /* Issue #100 — the live editor keeps only the tree (the engine-wasm
    save path is `build_minimal_docx(&DocumentTree)`), so the tree must
    carry the root bindings too, or every Word paragraph's `w14:paraId`
    is written unbound. */
    document.document_root_attrs = document_root_attrs.clone();
    /* Issue #134 — the live editor keeps only the tree, so the tree also
    carries the source package: the UI save path (`writer::save_docx`)
    re-emits every sibling part through `write_docx` instead of
    synthesizing a minimal package that drops them. */
    document.source_package = Some(std::sync::Arc::new(engine::SourcePackage::from_entries(
        other_entries.iter().cloned(),
    )));

    Ok(DocxArchive {
        other_entries,
        document,
        document_root_attrs,
        warnings,
        part_names,
    })
}

/// Issue #325 — gate the main part on its root's namespace bindings
/// ([`crate::schema::grab_bag::NamespaceScope::classify_root`]). Canonical
/// roots (either family) are returned untouched — the byte-identical fast
/// path. A non-canonical root is re-prefixed
/// ([`crate::schema::ns_normalize::canonicalize_prefixes`]) and reported;
/// when even that fails, or the root is no WordprocessingML at all, the
/// part is returned as-is with a typed warning — never a silent empty
/// document.
fn canonical_main_part(name: &str, xml: Vec<u8>, warnings: &mut Vec<DocxWarning>) -> Vec<u8> {
    use crate::schema::family::RootBinding;
    use crate::schema::ns_normalize::inspect_root;
    match inspect_root(&xml) {
        RootBinding::Canonical(_) => xml,
        RootBinding::NotWordprocessingMl => {
            warnings.push(DocxWarning::NotWordprocessingMl);
            xml
        }
        RootBinding::NonCanonical { detail } => normalise_part(name, xml, detail, warnings),
    }
}

/// Issue #394 — [`canonical_main_part`] for the WordprocessingML siblings
/// the reader walks: `styles.xml`, `numbering.xml`, `settings.xml`,
/// `footnotes.xml`, `endnotes.xml`, `comments.xml` and every header /
/// footer part the main part's relationships name. A non-canonical part
/// is replaced IN `entries` by its normalised bytes, so every consumer —
/// the typed parsers, the writer's verbatim passthrough, the in-place
/// patches (`comments.xml`), a regenerated header's root bindings, the
/// tree's retained source package — sees one consistent canonical
/// spelling: the part is regenerate-only (its source bytes are never
/// spliced into), reported as [`DocxWarning::NonCanonicalNamespaces`].
/// Canonical parts stay byte-identical, as do parts the reader never
/// walks (custom XML, `fontTable.xml`, the theme) and a sibling whose
/// root is no WordprocessingML at all (it reads as before; only the main
/// part reports [`DocxWarning::NotWordprocessingMl`]).
fn canonical_sibling_parts(
    entries: &mut [(String, Vec<u8>)],
    names: &PartNames,
    warnings: &mut Vec<DocxWarning>,
) {
    use crate::schema::family::RootBinding;
    use crate::schema::ns_normalize::inspect_root;
    let mut parts: Vec<String> = [
        &names.styles,
        &names.numbering,
        &names.settings,
        &names.footnotes,
        &names.endnotes,
        &names.comments,
    ]
    .into_iter()
    .cloned()
    .collect();
    parts.extend(header_footer_parts(entries, names));
    let mut seen = std::collections::HashSet::new();
    for part in parts {
        if !seen.insert(part.clone()) {
            continue;
        }
        let Some(slot) = entries.iter_mut().find(|(n, _)| *n == part) else {
            continue;
        };
        if let RootBinding::NonCanonical { detail } = inspect_root(&slot.1) {
            let xml = std::mem::take(&mut slot.1);
            slot.1 = normalise_part(&part, xml, detail, warnings);
        }
    }
}

/// Issue #394 — the archive entries of every header / footer part the
/// main part's relationships target (both namespace families' rel types),
/// in relationship order.
fn header_footer_parts(entries: &[(String, Vec<u8>)], names: &PartNames) -> Vec<String> {
    const REL_BASE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
    let Some(rels) = entries
        .iter()
        .find(|(n, _)| *n == names.main_rels)
        .and_then(|(_, b)| crate::opc::relationships::parse_relationships(b).ok())
    else {
        return Vec::new();
    };
    let header = format!("{REL_BASE}header");
    let footer = format!("{REL_BASE}footer");
    rels.by_type(&header)
        .chain(rels.by_type(&footer))
        .filter(|r| r.target_mode == crate::opc::relationships::TargetMode::Internal)
        .map(|r| resolve_in_package(entries, &names.main, &r.target))
        .collect()
}

/// Issues #325 / #394 — re-prefix one non-canonical part (`detail` names
/// the first offending binding) and report it. A part the normaliser
/// cannot rewrite is returned as-is (it will likely read empty), still
/// reported.
fn normalise_part(
    name: &str,
    xml: Vec<u8>,
    detail: String,
    warnings: &mut Vec<DocxWarning>,
) -> Vec<u8> {
    use crate::schema::ns_normalize::canonicalize_prefixes;
    match canonicalize_prefixes(&xml) {
        Ok(normalised) => {
            warnings.push(DocxWarning::NonCanonicalNamespaces {
                part: name.to_string(),
                detail,
                normalized: true,
            });
            normalised
        }
        Err(e) => {
            warnings.push(DocxWarning::NonCanonicalNamespaces {
                part: name.to_string(),
                detail: format!("{detail}; normalisation failed: {e}"),
                normalized: false,
            });
            xml
        }
    }
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

/// Issue #188 — resolve a relationship `Target` against its SOURCE part
/// (OPC, ECMA-376 Part 2 §9.3: a relative reference is resolved against
/// the source part's name). `media/image1.png` from `word/document.xml`
/// or `word/header1.xml` → `word/media/image1.png`; `../media/x.png`
/// collapses; a leading `/` is package-absolute. Returns the archive
/// entry name (no leading slash).
pub(crate) fn resolve_part_relative_target(source_part: &str, target: &str) -> String {
    let mut segs: Vec<&str> = Vec::new();
    if !target.starts_with('/')
        && let Some((dir, _)) = source_part.rsplit_once('/')
    {
        segs.extend(dir.split('/').filter(|s| !s.is_empty()));
    }
    for s in target.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    segs.join("/")
}

/// Issue #188 — register every picture relationship of ONE part (`rels`
/// is that part's own rels table, `source_part` its entry name) into
/// `media`, keyed by the resolved target entry name, and return the
/// part-local `rel id → media key` map. Identical targets reached from
/// different parts (or under different ids) share one blob. Lenient
/// like the pre-#188 pass: any relationship whose target resolves into
/// `word/media/` and exists in the archive counts as a picture.
fn register_part_media(
    entries: &[(String, Vec<u8>)],
    source_part: &str,
    rels: &HashMap<String, String>,
    media: &mut HashMap<String, ImageBlob>,
) -> HashMap<String, String> {
    let mut keys: HashMap<String, String> = HashMap::new();
    let exists = |name: &str| entries.iter().any(|(n, _)| n == name);
    for (rid, target) in rels {
        let mut entry = resolve_part_relative_target(source_part, target);
        /* Issue #353 — a percent-encoded target (`my%20pic.png`) names the
        decoded entry. */
        if !exists(&entry)
            && let Ok(candidates) = target_candidates(source_part, target)
            && let Some(found) = candidates.into_iter().find(|c| exists(c))
        {
            entry = found;
        }
        /* Pre-#188 leniency: a malformed `word/media/…` target written
        package-relative without its leading slash still resolves. */
        if !exists(&entry) {
            let legacy = resolve_target(target);
            if exists(&legacy) {
                entry = legacy;
            }
        }
        /* Issue #353 — media live in a `media/` directory next to the
        main part, which is `word/media/` unless the main part moved. */
        if !(entry.starts_with("media/") || entry.contains("/media/")) {
            continue;
        }
        if !media.contains_key(&entry) {
            let Some(bytes) = entries
                .iter()
                .find(|(n, _)| n == &entry)
                .map(|(_, b)| b.clone())
            else {
                continue;
            };
            media.insert(
                entry.clone(),
                ImageBlob {
                    content_type: guess_image_mime(&entry).to_string(),
                    data: bytes,
                },
            );
        }
        keys.insert(rid.clone(), entry);
    }
    keys
}

/// Issue #188 — stamp each picture in `blocks` (tables and text-box
/// stories included) with the media key its part-local `rel_id` resolves
/// to. A picture whose rel is unknown keeps `media_key: None`.
fn stamp_media_keys(blocks: &mut [engine::Block], keys: &HashMap<String, String>) {
    if keys.is_empty() {
        return;
    }
    engine::for_each_image_mut(blocks, &mut |kind| {
        if let engine::InlineKind::Image {
            rel_id, media_key, ..
        } = kind
            && let Some(key) = keys.get(rel_id.as_str())
        {
            *media_key = Some(key.clone());
        }
    });
}

/// Issue #72 — the OPC rels entry name for a part: relationships of
/// `word/header1.xml` live at `word/_rels/header1.xml.rels` (the part's
/// directory + `_rels/` + basename + `.rels`).
fn part_rels_entry_name(part_entry: &str) -> String {
    rels_entry_name(part_entry)
}

/// Issue #353 — the archive entry a relationship `target` of `source_part`
/// denotes: the OPC resolution (`..`, `/`-absolute, `%20`) when the
/// archive holds it, else the pre-#353 `word/`-anchored guess. An escaping
/// target resolves to the legacy guess too (a missing entry — nothing
/// outside the archive is ever opened).
pub(crate) fn resolve_in_package(
    entries: &[(String, Vec<u8>)],
    source_part: &str,
    target: &str,
) -> String {
    if let Ok(candidates) = target_candidates(source_part, target)
        && let Some(found) = candidates
            .into_iter()
            .find(|c| entries.iter().any(|(n, _)| n == c))
    {
        return found;
    }
    resolve_target(target)
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
                    /* Issue #81 — internal anchors carry no relationship. */
                    if h.target.starts_with('#') {
                        return Some(h);
                    }
                    rels.get(&h.target).map(|url| engine::Hyperlink {
                        start: h.start,
                        end: h.end,
                        target: url.clone(),
                        attrs: h.attrs,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};

    /// Issue #188 — rel targets resolve against the SOURCE part's name.
    #[test]
    fn part_relative_targets_resolve_against_the_source_part() {
        let r = resolve_part_relative_target;
        assert_eq!(
            r("word/document.xml", "media/image1.png"),
            "word/media/image1.png"
        );
        assert_eq!(
            r("word/header1.xml", "media/image2.png"),
            "word/media/image2.png"
        );
        assert_eq!(
            r("word/header1.xml", "/word/media/image1.png"),
            "word/media/image1.png"
        );
        assert_eq!(r("word/sub/part.xml", "../media/a.png"), "word/media/a.png");
        assert_eq!(r("word/document.xml", "./media/b.png"), "word/media/b.png");
    }

    /// Issue #188 — a body picture and a header picture declared under
    /// the SAME part-local id land as two blobs keyed by target path, a
    /// footer re-using the body's target dedupes onto it, and every
    /// picture carries its part-resolved media key.
    #[test]
    fn media_is_keyed_by_part_resolved_target_not_rel_id() {
        let docx = crate::test_fixtures::part_scoped_media_docx(b"BODY", b"HEADER");
        let doc = read_docx(&docx).expect("read").document;
        let mut keys: Vec<&str> = doc.media.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["word/media/image1.jpeg", "word/media/image2.jpeg"]);
        assert_eq!(doc.media["word/media/image1.jpeg"].data, b"BODY");
        assert_eq!(doc.media["word/media/image2.jpeg"].data, b"HEADER");
        let key_of = |blocks: &[engine::Block]| {
            blocks[0].as_paragraph().unwrap().inline_objects[0]
                .kind
                .image_media_key()
                .map(str::to_string)
        };
        let body: Vec<engine::Block> = doc.blocks.iter().cloned().collect();
        assert_eq!(key_of(&body).as_deref(), Some("word/media/image1.jpeg"));
        assert_eq!(
            key_of(&doc.headers["rId7"]).as_deref(),
            Some("word/media/image2.jpeg")
        );
        assert_eq!(
            key_of(&doc.footers["rId8"]).as_deref(),
            Some("word/media/image1.jpeg")
        );
    }

    /// Minimal package: just `word/document.xml` with the given bytes.
    fn package(document_xml: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            let opts =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            zip.start_file(DOC_XML, opts).unwrap();
            zip.write_all(document_xml).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    const ROOT: &str =
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#;

    #[test]
    fn well_formed_guard_accepts_a_clean_part() {
        let xml = format!(
            "{ROOT}<w:body><w:p><w:r><w:t>x</w:t></w:r></w:p><!-- c --><w:sectPr/></w:body></w:document>"
        );
        check_document_xml_well_formed(&package(xml.as_bytes())).expect("clean part");
        /* A BOM + declaration are fine too. */
        let bom = format!("\u{FEFF}<?xml version=\"1.0\"?>{ROOT}<w:body/></w:document>");
        check_document_xml_well_formed(&package(bom.as_bytes())).expect("bom part");
    }

    /// Issue #110 — the exact corruption the corpus produced: a
    /// passthrough paragraph truncated to `</w` before the next sibling.
    #[test]
    fn well_formed_guard_rejects_the_issue_110_splice() {
        for body in [
            "<w:body><w:p><w:r><w:t>x</w:t></w:r></w<w:sectPr/></w:body>",
            "<w:body><w:p><w:r><w:t>x</w:t></w:r></w<w:p><w:r><w:t>y</w:t></w:r></w</w:body>",
            "<w:body><w:p><w:r><w:t>x</w:t></w:r></wdy></w:document>",
        ] {
            let xml = format!("{ROOT}{body}</w:document>");
            let err = check_document_xml_well_formed(&package(xml.as_bytes()))
                .expect_err("splice must be rejected");
            assert!(
                matches!(err, DocxError::Xml(_) | DocxError::MalformedXml(_)),
                "{err}"
            );
        }
    }

    /// Issue #100 — a prefix used without an in-scope binding is not
    /// namespace-well-formed: the exact shape a Word paragraph
    /// (`w14:paraId`) takes under a synthesized root that forgot to
    /// re-declare `xmlns:w14`. Declared prefixes (root or local) pass.
    #[test]
    fn well_formed_guard_rejects_unbound_prefixes() {
        let attr = format!(
            r#"{ROOT}<w:body><w:p w14:paraId="1A2B3C4D"><w:r><w:t>x</w:t></w:r></w:p></w:body></w:document>"#
        );
        let err = check_document_xml_well_formed(&package(attr.as_bytes()))
            .expect_err("unbound attribute prefix must be rejected");
        assert!(err.to_string().contains("w14:paraId"), "{err}");
        let elem = format!(
            r#"{ROOT}<w:body><w:p><w:r><w:rPr><w14:glow/></w:rPr></w:r></w:p></w:body></w:document>"#
        );
        let err = check_document_xml_well_formed(&package(elem.as_bytes()))
            .expect_err("unbound element prefix must be rejected");
        assert!(err.to_string().contains("w14:glow"), "{err}");
        /* Bound on the root, or locally — both fine; `xml:` is implicit. */
        let ok_root = r#"<w:document xmlns:w="urn:w" xmlns:w14="urn:w14"><w:body><w:p w14:paraId="1"><w:r><w:t xml:space="preserve">x</w:t></w:r></w:p></w:body></w:document>"#;
        check_document_xml_well_formed(&package(ok_root.as_bytes())).expect("root-bound");
        let ok_local = format!(
            r#"{ROOT}<w:body><w:p><w:r><w:rPr><w14:glow xmlns:w14="urn:w14"/></w:rPr></w:r></w:p></w:body></w:document>"#
        );
        check_document_xml_well_formed(&package(ok_local.as_bytes())).expect("locally bound");
    }

    #[test]
    fn well_formed_guard_rejects_structural_problems() {
        /* Unclosed root at EOF. */
        let open = format!("{ROOT}<w:body><w:p/>");
        assert!(matches!(
            check_document_xml_well_formed(&package(open.as_bytes())),
            Err(DocxError::MalformedXml(_))
        ));
        /* Two root elements. */
        let two = format!("{ROOT}</w:document>{ROOT}</w:document>");
        assert!(matches!(
            check_document_xml_well_formed(&package(two.as_bytes())),
            Err(DocxError::MalformedXml(_))
        ));
        /* Stray end tag. */
        let stray = format!("{ROOT}<w:body/></w:document></w:p>");
        assert!(check_document_xml_well_formed(&package(stray.as_bytes())).is_err());
        /* No document.xml at all. */
        let mut buf = Vec::new();
        {
            let mut zip = ZipWriter::new(Cursor::new(&mut buf));
            zip.start_file("word/other.xml", SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"<a/>").unwrap();
            zip.finish().unwrap();
        }
        assert!(matches!(
            check_document_xml_well_formed(&buf),
            Err(DocxError::MissingEntry(_))
        ));
    }

    /// Issue #221 — `read_docx_with_settings`'s `DefaultPageSize` fallback
    /// reaches a `<w:sectPr/>` that omits `<w:pgSz>` entirely: `read_docx`
    /// (unchanged) still falls back to A4, and `Letter` produces the
    /// Letter preset byte-for-byte (`engine::PageGeometry::letter()`).
    #[test]
    fn read_docx_with_settings_page_size_fallback_reaches_a_pgsz_less_section() {
        let bytes = crate::test_fixtures::no_pgsz_docx("hello");

        let a4 = read_docx(&bytes).expect("plain read_docx stays A4");
        assert_eq!(
            a4.document.body_section.geometry,
            engine::PageGeometry::a4()
        );

        let letter = read_docx_with_settings(&bytes, engine::DefaultPageSize::Letter, true)
            .expect("read with Letter default");
        assert_eq!(
            letter.document.body_section.geometry,
            engine::PageGeometry::letter()
        );
        assert_eq!(
            letter.document.settings.default_page_size,
            engine::DefaultPageSize::Letter
        );
    }

    /// Issue #221 — `ts/e2e/fixtures/no_pgsz.docx` (generated by
    /// `.agent-scratch/make_no_pgsz_fixture.py`, same shape as
    /// [`crate::test_fixtures::no_pgsz_docx`] above) is the file the
    /// `open-document-defaults.spec.ts` e2e test loads through the real
    /// Solid shell + worker; this pins that it stays a valid, genuinely
    /// `<w:pgSz>`-less package from the Rust side too, so a change to
    /// either side surfaces here instead of only as an opaque e2e failure.
    #[test]
    fn ts_e2e_no_pgsz_fixture_is_a_valid_pgsz_less_package() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ts/e2e/fixtures/no_pgsz.docx"
        );
        let bytes = std::fs::read(path).expect("read ts/e2e/fixtures/no_pgsz.docx");
        let a4 = read_docx(&bytes).expect("plain read_docx stays A4");
        assert_eq!(
            a4.document.body_section.geometry,
            engine::PageGeometry::a4()
        );
        let letter = read_docx_with_settings(&bytes, engine::DefaultPageSize::Letter, true)
            .expect("read with Letter default");
        assert_eq!(
            letter.document.body_section.geometry,
            engine::PageGeometry::letter()
        );
    }

    /* ---------------------------------------------------------------
    Issue #348 — package limits.
    --------------------------------------------------------------- */

    use crate::opc::limits::PackageLimit;
    use crate::test_fixtures::{
        compressible_bomb_docx, lying_size_docx, nested_sdt_docx, nested_tables_docx,
    };

    fn limit_of(err: &DocxError) -> Option<PackageLimit> {
        match err {
            DocxError::PackageTooLarge { limit, .. } => Some(*limit),
            _ => None,
        }
    }

    fn body_text(doc: &DocumentTree) -> String {
        doc.to_plain_text()
    }

    /// A directory record claiming 4 GiB (or `u64::MAX`) for a 100-byte
    /// part used to reach `Vec::with_capacity(declared)` — a "capacity
    /// overflow" trap on wasm32 (and natively for the larger lie). The
    /// declared size is never trusted now: the real bytes are read.
    #[test]
    fn a_lying_declared_size_never_reaches_the_allocator() {
        for declared in [4 * 1024 * 1024 * 1024, u64::MAX - 1] {
            let docx = lying_size_docx(declared);
            let archive = read_docx(&docx).expect("the real part is small");
            assert_eq!(body_text(&archive.document), "small", "declared {declared}");
            /* The harness guard reads through the same bounded path. */
            check_document_xml_well_formed(&docx).expect("well-formed");
        }
    }

    /// A compression bomb is inflated only up to its budget + 1 byte and
    /// refused with the typed error, naming the part.
    #[test]
    fn a_compressible_bomb_is_refused_at_the_part_budget() {
        let limits = PackageLimits {
            max_part_bytes: 1024 * 1024,
            ..PackageLimits::DEFAULT
        };
        let docx = compressible_bomb_docx(8 * 1024 * 1024);
        assert!(
            docx.len() < 64 * 1024,
            "the bomb is small on disk: {}",
            docx.len()
        );
        let err = read_docx_with_limits(&docx, engine::DefaultPageSize::A4, true, &limits)
            .expect_err("8 MiB > 1 MiB part budget");
        assert_eq!(limit_of(&err), Some(PackageLimit::PartBytes), "{err:?}");
        assert!(err.to_string().contains("word/media/bomb.bin"), "{err}");
        /* Under the total budget instead: 3 × 600 KiB parts, 1 MiB total. */
        let limits = PackageLimits {
            max_total_bytes: 1024 * 1024,
            ..PackageLimits::DEFAULT
        };
        let part = vec![0u8; 600 * 1024];
        let docx = crate::test_fixtures::package_with_document_xml(
            &crate::test_fixtures::document_xml_with_body("<w:p/>"),
            &[("word/media/a.bin", &part), ("word/media/b.bin", &part)],
        );
        let err = read_docx_with_limits(&docx, engine::DefaultPageSize::A4, true, &limits)
            .expect_err("1.2 MiB > 1 MiB total");
        assert_eq!(limit_of(&err), Some(PackageLimit::TotalBytes), "{err:?}");
        /* The stock budget reads the same package. */
        read_docx(&docx).expect("well under the default limits");
    }

    #[test]
    fn too_many_entries_are_refused_before_any_is_read() {
        let limits = PackageLimits {
            max_entries: 4,
            ..PackageLimits::DEFAULT
        };
        let docx = crate::test_fixtures::package_with_document_xml(
            &crate::test_fixtures::document_xml_with_body("<w:p/>"),
            &[("word/media/a.bin", b"a")],
        );
        let err = read_docx_with_limits(&docx, engine::DefaultPageSize::A4, true, &limits)
            .expect_err("5 entries > 4");
        assert_eq!(limit_of(&err), Some(PackageLimit::Entries), "{err:?}");
    }

    /// 5000 content controls nested inside each other: refused by the XML
    /// depth cap with the typed error instead of being walked.
    #[test]
    fn five_thousand_nested_sdts_are_refused_by_the_depth_cap() {
        let err = read_docx(&nested_sdt_docx(5000)).expect_err("depth 10 000 > 256");
        assert_eq!(limit_of(&err), Some(PackageLimit::XmlDepth), "{err:?}");
        assert!(err.to_string().contains("word/document.xml"), "{err}");
        /* A shallow chain still reads. */
        let ok = read_docx(&nested_sdt_docx(20)).expect("40 levels");
        assert_eq!(body_text(&ok.document), "deep");
    }

    /// 60 nested tables (depth ≈ 185) stay under the cap and read fully —
    /// the innermost cell's text is reachable in the typed model.
    #[test]
    fn sixty_nested_tables_still_read() {
        let archive = read_docx(&nested_tables_docx(60)).expect("60 nested tables read");
        assert!(archive.warnings.is_empty(), "{:?}", archive.warnings);
        let mut table = archive.document.blocks[0].as_table().expect("table");
        let mut levels = 1;
        loop {
            let cell = &table.rows[0].cells[0];
            match cell.blocks.first() {
                Some(engine::Block::Table(inner)) => {
                    table = inner;
                    levels += 1;
                }
                Some(engine::Block::Paragraph(p)) => {
                    assert_eq!(p.text, "deep");
                    break;
                }
                None => panic!("empty cell at level {levels}"),
            }
        }
        assert_eq!(levels, 60);
    }

    /// The element-count cap refuses a part with too many elements.
    #[test]
    fn the_element_cap_is_enforced_on_walked_parts() {
        let limits = PackageLimits {
            max_xml_elements: 50,
            ..PackageLimits::DEFAULT
        };
        let body = "<w:p><w:r><w:t>x</w:t></w:r></w:p>".repeat(20);
        let docx = crate::test_fixtures::package_with_document_xml(
            &crate::test_fixtures::document_xml_with_body(&body),
            &[],
        );
        let err = read_docx_with_limits(&docx, engine::DefaultPageSize::A4, true, &limits)
            .expect_err("60+ elements > 50");
        assert_eq!(limit_of(&err), Some(PackageLimit::XmlElements), "{err:?}");
    }
}
