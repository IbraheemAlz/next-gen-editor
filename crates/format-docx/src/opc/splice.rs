//! Issue #213 — additively splice extra OPC parts (`styles.xml`,
//! `numbering.xml`, theme, `fontTable.xml`) from a document's retained
//! source package into an already-built minimal `.docx` package, so a
//! clipboard fragment cut from an opened document still ships the style
//! and numbering definitions its paragraphs reference.
//!
//! `build_minimal_docx` (the clipboard fragment's base builder) only ever
//! emits `word/document.xml` plus the OPC plumbing for inline images; a
//! paragraph carrying `<w:pStyle w:val="Heading1">` still writes that
//! reference (it is a per-paragraph property, unaffected by which parts
//! travel), but with no `styles.xml` in the package the reference
//! resolves to nothing on re-open — Word (or our own reader) falls back
//! to plain defaults, exactly the bug #213 tracks.
//!
//! This module is pure OPC-layer surgery: whole-part byte copies plus
//! `[Content_Types].xml` / `word/_rels/document.xml.rels` splices. It
//! never touches `word/document.xml` or any paragraph serialization —
//! [`writer::build_minimal_docx`](crate::writer::build_minimal_docx) has
//! already produced the fragment's document part before this runs.

use crate::error::DocxError;
use engine::SourcePackage;
use std::collections::HashSet;
use std::io::{Cursor, Read, Write};
use zip::ZipArchive;
use zip::write::{SimpleFileOptions, ZipWriter};

const STYLES_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml";
const STYLES_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
const NUMBERING_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";
const NUMBERING_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
const FONT_TABLE_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.fontTable+xml";
const FONT_TABLE_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/fontTable";
const THEME_CONTENT_TYPE: &str = "application/vnd.openxmlformats-officedocument.theme+xml";
const THEME_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme";

/// One extra part this splice knows how to carry over: its exact archive
/// path + bytes, the `[Content_Types].xml` `Override` content type, and
/// the relationship `Type` IRI `word/_rels/document.xml.rels` points at
/// it with.
struct ExtraPart<'a> {
    name: &'a str,
    data: &'a [u8],
    content_type: &'static str,
    rel_type: &'static str,
}

/// The extra parts `package` carries that this splice knows how to
/// re-attach: `word/styles.xml`, `word/numbering.xml`,
/// `word/fontTable.xml` by exact name, and every `word/theme/theme*.xml`
/// by prefix (ECMA-376 only ever wires one theme part, but copying every
/// one found costs nothing and drops nothing).
fn extra_parts(package: &SourcePackage) -> Vec<ExtraPart<'_>> {
    let mut out = Vec::new();
    for (name, content_type, rel_type) in [
        ("word/styles.xml", STYLES_CONTENT_TYPE, STYLES_REL_TYPE),
        (
            "word/numbering.xml",
            NUMBERING_CONTENT_TYPE,
            NUMBERING_REL_TYPE,
        ),
        (
            "word/fontTable.xml",
            FONT_TABLE_CONTENT_TYPE,
            FONT_TABLE_REL_TYPE,
        ),
    ] {
        if let Some(data) = package.entry(name) {
            out.push(ExtraPart {
                name,
                data,
                content_type,
                rel_type,
            });
        }
    }
    for entry in &package.entries {
        if entry.name.starts_with("word/theme/") && entry.name.ends_with(".xml") {
            out.push(ExtraPart {
                name: &entry.name,
                data: &entry.data,
                content_type: THEME_CONTENT_TYPE,
                rel_type: THEME_REL_TYPE,
            });
        }
    }
    out
}

/// Splice `package`'s styles/numbering/theme/fontTable parts into an
/// already-built minimal `.docx` (`docx`), additively: new `[Content_
/// Types].xml` overrides, new `word/_rels/document.xml.rels` rows, and
/// the whole part bytes appended verbatim. Every existing entry —
/// `word/document.xml` included — is copied through byte-identical. A
/// package carrying none of the known part names is a no-op that returns
/// `docx` unchanged.
pub fn add_style_parts(docx: &[u8], package: &SourcePackage) -> Result<Vec<u8>, DocxError> {
    let extras = extra_parts(package);
    if extras.is_empty() {
        return Ok(docx.to_vec());
    }

    let mut archive = ZipArchive::new(Cursor::new(docx))?;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(archive.len() + extras.len());
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();
        let mut buf = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut buf)?;
        entries.push((name, buf));
    }

    /* Mint non-colliding relationship ids — the minimal package's own
    rels only ever carries image relationships keyed by the engine's
    media rel ids (or `nge_media_N`), so a distinct prefix is enough on
    its own, but check anyway rather than assume the minimal builder's
    id shapes never change. */
    let mut existing_ids: HashSet<String> = HashSet::new();
    if let Some((_, rels)) = entries
        .iter()
        .find(|(n, _)| n == "word/_rels/document.xml.rels")
        && let Ok(text) = std::str::from_utf8(rels)
        && let Ok(parsed) = crate::opc::relationships::parse_relationships(text.as_bytes())
    {
        existing_ids.extend(parsed.items.into_iter().map(|r| r.id));
    }

    let mut ct_overrides = String::new();
    let mut rel_rows = String::new();
    let mut next = 0u32;
    for part in &extras {
        ct_overrides.push_str(&format!(
            "<Override PartName=\"/{}\" ContentType=\"{}\"/>",
            part.name, part.content_type
        ));
        let target = part
            .name
            .strip_prefix("word/")
            .expect("extra parts are always under word/");
        let id = loop {
            next += 1;
            let candidate = format!("rIdSplice{next}");
            if existing_ids.insert(candidate.clone()) {
                break candidate;
            }
        };
        rel_rows.push_str(&format!(
            "<Relationship Id=\"{id}\" Type=\"{}\" Target=\"{target}\"/>",
            part.rel_type
        ));
    }

    if let Some((_, ct)) = entries.iter_mut().find(|(n, _)| n == "[Content_Types].xml") {
        let text = std::str::from_utf8(ct)
            .map_err(|_| DocxError::MalformedXml("[Content_Types].xml is not UTF-8".into()))?;
        let Some(head) = text.strip_suffix("</Types>") else {
            return Err(DocxError::MalformedXml(
                "[Content_Types].xml missing closing </Types>".into(),
            ));
        };
        *ct = format!("{head}{ct_overrides}</Types>").into_bytes();
    }
    if let Some((_, rels)) = entries
        .iter_mut()
        .find(|(n, _)| n == "word/_rels/document.xml.rels")
    {
        let text = std::str::from_utf8(rels)
            .map_err(|_| DocxError::MalformedXml("document.xml.rels is not UTF-8".into()))?;
        let Some(head) = text.strip_suffix("</Relationships>") else {
            return Err(DocxError::MalformedXml(
                "document.xml.rels missing closing </Relationships>".into(),
            ));
        };
        *rels = format!("{head}{rel_rows}</Relationships>").into_bytes();
    }

    for part in &extras {
        entries.push((part.name.to_string(), part.data.to_vec()));
    }

    let extra_bytes: usize = extras.iter().map(|p| p.data.len()).sum();
    let mut buf = Vec::with_capacity(docx.len() + extra_bytes);
    {
        let mut zip = ZipWriter::new(Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, data) in &entries {
            zip.start_file(name, opts)?;
            zip.write_all(data)?;
        }
        zip.finish()?;
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opc::content_types::parse_content_types;
    use crate::opc::relationships::parse_relationships;
    use crate::writer::build_minimal_docx;
    use engine::{DocumentTree, Paragraph};

    fn zip_entries(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut a = ZipArchive::new(Cursor::new(bytes)).expect("zip");
        (0..a.len())
            .map(|i| {
                let mut f = a.by_index(i).expect("entry");
                let mut b = Vec::new();
                f.read_to_end(&mut b).expect("read");
                (f.name().to_owned(), b)
            })
            .collect()
    }

    fn entry<'a>(entries: &'a [(String, Vec<u8>)], name: &str) -> &'a [u8] {
        entries
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("missing {name}"))
            .1
            .as_slice()
    }

    fn package_with(parts: &[(&str, &[u8])]) -> SourcePackage {
        SourcePackage::from_entries(parts.iter().map(|(n, d)| (n.to_string(), d.to_vec())))
    }

    fn styled_doc() -> DocumentTree {
        let mut doc = DocumentTree::default();
        doc.blocks.push_back(engine::Block::Paragraph(Paragraph {
            text: "Package title".into(),
            style_id: Some("Heading1".into()),
            ..Default::default()
        }));
        doc
    }

    const STYLES_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:rPr><w:b/></w:rPr></w:style></w:styles>"#;
    const NUMBERING_XML: &[u8] =
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;
    const THEME_XML: &[u8] =
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office"/>"#;
    const FONT_TABLE_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:fonts xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"/>"#;

    #[test]
    fn no_extra_parts_is_a_byte_identical_no_op() {
        let minimal = build_minimal_docx(&styled_doc()).expect("build");
        let empty = SourcePackage::default();
        let out = add_style_parts(&minimal, &empty).expect("splice");
        assert_eq!(out, minimal);
    }

    #[test]
    fn splices_styles_numbering_theme_and_font_table() {
        let minimal = build_minimal_docx(&styled_doc()).expect("build");
        let package = package_with(&[
            ("word/styles.xml", STYLES_XML),
            ("word/numbering.xml", NUMBERING_XML),
            ("word/theme/theme1.xml", THEME_XML),
            ("word/fontTable.xml", FONT_TABLE_XML),
        ]);
        let out = add_style_parts(&minimal, &package).expect("splice");
        let entries = zip_entries(&out);

        assert_eq!(entry(&entries, "word/styles.xml"), STYLES_XML);
        assert_eq!(entry(&entries, "word/numbering.xml"), NUMBERING_XML);
        assert_eq!(entry(&entries, "word/theme/theme1.xml"), THEME_XML);
        assert_eq!(entry(&entries, "word/fontTable.xml"), FONT_TABLE_XML);

        /* `[Content_Types].xml` gains one Override per part; the
        original document.xml Override is kept untouched. */
        let ct = parse_content_types(entry(&entries, "[Content_Types].xml")).expect("parse ct");
        assert_eq!(ct.lookup("/word/styles.xml"), Some(STYLES_CONTENT_TYPE));
        assert_eq!(
            ct.lookup("/word/numbering.xml"),
            Some(NUMBERING_CONTENT_TYPE)
        );
        assert_eq!(
            ct.lookup("/word/theme/theme1.xml"),
            Some(THEME_CONTENT_TYPE)
        );
        assert_eq!(
            ct.lookup("/word/fontTable.xml"),
            Some(FONT_TABLE_CONTENT_TYPE)
        );
        assert_eq!(
            ct.lookup("/word/document.xml"),
            Some(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
            )
        );

        /* `word/_rels/document.xml.rels` gains one row per part, each
        with a unique id. */
        let rels =
            parse_relationships(entry(&entries, "word/_rels/document.xml.rels")).expect("rels");
        assert_eq!(rels.items.len(), 4);
        let mut ids: Vec<&str> = rels.items.iter().map(|r| r.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4, "relationship ids are unique");
        assert!(
            rels.by_type(STYLES_REL_TYPE)
                .any(|r| r.target == "styles.xml")
        );
        assert!(
            rels.by_type(NUMBERING_REL_TYPE)
                .any(|r| r.target == "numbering.xml")
        );
        assert!(
            rels.by_type(THEME_REL_TYPE)
                .any(|r| r.target == "theme/theme1.xml")
        );
        assert!(
            rels.by_type(FONT_TABLE_REL_TYPE)
                .any(|r| r.target == "fontTable.xml")
        );

        /* `word/document.xml` is untouched by the splice. */
        assert_eq!(
            entry(&entries, "word/document.xml"),
            entry(&zip_entries(&minimal), "word/document.xml")
        );
    }

    #[test]
    fn only_present_parts_are_spliced() {
        let minimal = build_minimal_docx(&styled_doc()).expect("build");
        /* A package that only carries styles.xml (no numbering / theme /
        fontTable) splices exactly that one part. */
        let package = package_with(&[("word/styles.xml", STYLES_XML)]);
        let out = add_style_parts(&minimal, &package).expect("splice");
        let entries = zip_entries(&out);
        assert!(entries.iter().any(|(n, _)| n == "word/styles.xml"));
        assert!(!entries.iter().any(|(n, _)| n == "word/numbering.xml"));
        assert!(!entries.iter().any(|(n, _)| n == "word/theme/theme1.xml"));
        assert!(!entries.iter().any(|(n, _)| n == "word/fontTable.xml"));
        let rels =
            parse_relationships(entry(&entries, "word/_rels/document.xml.rels")).expect("rels");
        assert_eq!(rels.items.len(), 1);
    }
}
