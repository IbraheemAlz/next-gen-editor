//! Reader correctness on Word-shaped packages: issue #369 (the
//! paragraph mark's `<w:pPr><w:rPr>` formats the mark only, never the
//! paragraph's runs), issue #394 (every WordprocessingML sibling part
//! with a non-canonical namespace prefix is normalised before it is
//! parsed) and issue #395 (paragraph borders defined on styles cascade
//! per edge and resolve logical edges by the paragraph's direction).

use super::{WORD_ROOT, assert_document_xml_well_formed, extract_doc_xml, read_docx, write_docx};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, LogicalPos};

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

const DOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

/// A package of `[Content_Types].xml`, `_rels/.rels` and `parts`.
fn package(parts: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        let fixed = [
            ("[Content_Types].xml", CONTENT_TYPES),
            ("_rels/.rels", DOT_RELS),
        ];
        for (name, body) in fixed.iter().chain(parts) {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn document_xml(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n{WORD_ROOT}<w:body>{body}<w:sectPr/></w:body></w:document>"
    )
}

fn at(block: u32, offset: u32) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset)
}

/// Both save paths: `write_docx` against the source archive and the live
/// editor's `save_docx` (tree + retained source package, issue #134).
fn save_both(a: &format_docx::DocxArchive, doc: &engine::DocumentTree) -> Result<Vec<u8>> {
    let bytes = write_docx(a, doc).context("write_docx")?;
    let ui = format_docx::save_docx(doc).context("save_docx")?;
    if ui != bytes {
        bail!("the UI save path diverges from write_docx");
    }
    assert_document_xml_well_formed(&bytes)?;
    Ok(bytes)
}

/* ============================================ mark formatting (#369) ==== */

/// Paragraph 0: a bold + red mark over a plain run and an italic run.
const P_MARKED: &str = concat!(
    r#"<w:p w14:paraId="0B000001" w:rsidR="00BB0001" w:rsidRDefault="00BB0001"><w:pPr><w:rPr><w:b/><w:color w:val="C00000"/><w:lang w:val="en-GB"/></w:rPr></w:pPr>"#,
    r#"<w:r><w:t xml:space="preserve">plain </w:t></w:r><w:r><w:rPr><w:i/></w:rPr><w:t>slanted</w:t></w:r></w:p>"#,
);
/// Paragraph 1: EMPTY, its mark bold.
const P_EMPTY_BOLD: &str = r#"<w:p w14:paraId="0B000002" w:rsidR="00BB0002" w:rsidRDefault="00BB0002"><w:pPr><w:rPr><w:b/></w:rPr></w:pPr></w:p>"#;

/// Issue #369 — step 49.
///
/// a. A bold + red mark over plain runs: the runs read plain (the italic
///    one italic only), the mark keeps its formatting, a zero-edit save
///    is byte-identical.
/// b. Typing into the plain run is a pure insertion that re-reads plain.
/// c. Typing into the empty bold-marked paragraph (#293) saves a bold
///    run and re-reads bold.
pub(crate) fn run_mark_formatting_roundtrip() -> Result<()> {
    let xml = document_xml(&format!("{P_MARKED}{P_EMPTY_BOLD}"));
    let fixture = package(&[("word/document.xml", xml.as_str())]);
    let a = read_docx(&fixture).context("read mark fixture")?;
    let doc = &a.document;
    let p = doc.nth_paragraph(0).context("paragraph 0")?;
    let (plain, slanted) = (p.style_at(0), p.style_at(7));
    if plain.bold.is_some() || plain.color.is_some() {
        bail!("the mark leaked into the plain run: {:?}", p.spans);
    }
    if slanted.italic != Some(true) || slanted.bold.is_some() || slanted.color.is_some() {
        bail!("the mark leaked into the italic run: {:?}", p.spans);
    }
    let mark = p.mark_style.as_deref().context("mark modeled")?;
    if mark.bold != Some(true) || mark.color != Some([0xC0, 0, 0, 255]) {
        bail!("mark: {mark:?}");
    }
    let zero = save_both(&a, doc)?;
    if extract_doc_xml(&zero)? != xml.as_bytes() {
        bail!("document.xml drifted on a zero-edit save");
    }
    println!(
        "[roundtrip] step 49a OK — a bold mark over plain runs reads plain runs; zero-edit save byte-identical"
    );

    let typed = doc.insert_text(at(0, 2), "ai");
    let bytes = save_both(&a, &typed)?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?)?;
    if out != xml.replacen(">plain <", ">plaiain <", 1) {
        bail!("typing into the plain run is not a pure insertion:\n{out}");
    }
    let b = read_docx(&bytes).context("re-read the insertion")?;
    let s = b
        .document
        .nth_paragraph(0)
        .context("re-read 0")?
        .style_at(2);
    if s.bold.is_some() || s.color.is_some() {
        bail!("the re-read inserted text took the mark's formatting: {s:?}");
    }
    println!("[roundtrip] step 49b OK — typing under a bold mark stays plain through a save");

    let typed = doc.insert_text(at(1, 0), "loud");
    let bytes = save_both(&a, &typed)?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?)?;
    let want = xml.replacen(
        P_EMPTY_BOLD,
        &P_EMPTY_BOLD.replacen(
            "</w:pPr></w:p>",
            r#"</w:pPr><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">loud</w:t></w:r></w:p>"#,
            1,
        ),
        1,
    );
    if out != want {
        bail!("typing into the bold-marked empty paragraph:\n{out}");
    }
    let b = read_docx(&bytes).context("re-read the typed paragraph")?;
    if b.document
        .nth_paragraph(1)
        .context("re-read 1")?
        .style_at(0)
        .bold
        != Some(true)
    {
        bail!("the typed run lost its bold");
    }
    println!("[roundtrip] step 49c OK — an empty bold-marked paragraph still types bold (#293)");
    Ok(())
}

/* ================================= sibling namespace prefixes (#394) ==== */

const NS_W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

const PREFIXED_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
</Relationships>"#;

/// A Word-shaped main part over a `styles.xml` and a `header1.xml` that
/// bind WordprocessingML to `x:`.
fn prefixed_siblings_docx() -> (String, Vec<u8>) {
    let body = concat!(
        r#"<w:p w14:paraId="0C000001"><w:pPr><w:pStyle w:val="Quote"/></w:pPr><w:r><w:t>quoted</w:t></w:r></w:p>"#,
        r#"<w:sectPr><w:headerReference w:type="default" r:id="rId2"/></w:sectPr>"#,
    );
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n{WORD_ROOT}<w:body>{body}</w:body></w:document>"
    );
    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<x:styles xmlns:x=\"{NS_W}\">\
         <x:style x:type=\"paragraph\" x:styleId=\"Quote\"><x:name x:val=\"Quote\"/>\
         <x:pPr><x:jc x:val=\"center\"/></x:pPr><x:rPr><x:i/></x:rPr></x:style></x:styles>"
    );
    let header = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n<x:hdr xmlns:x=\"{NS_W}\">\
         <x:p><x:r><x:t>running head</x:t></x:r></x:p></x:hdr>"
    );
    let docx = package(&[
        ("word/_rels/document.xml.rels", PREFIXED_RELS),
        ("word/document.xml", xml.as_str()),
        ("word/styles.xml", styles.as_str()),
        ("word/header1.xml", header.as_str()),
    ]);
    (xml, docx)
}

/// Issue #394 — step 50.
///
/// a. `styles.xml` and `header1.xml` binding WordprocessingML to `x:` are
///    normalised before they are parsed: the style cascades, the header
///    renders, each part is reported by name.
/// b. They are regenerate-only: a zero-edit save keeps `document.xml`
///    byte-identical and re-emits the two parts canonically spelled,
///    which re-read silently into the same model; typing into the body
///    is still a pure insertion.
pub(crate) fn run_sibling_prefix_roundtrip() -> Result<()> {
    let (xml, fixture) = prefixed_siblings_docx();
    let a = read_docx(&fixture).context("read prefixed-sibling fixture")?;
    let check = |doc: &engine::DocumentTree, what: &str| -> Result<()> {
        let p = doc.nth_paragraph(0).context("paragraph 0")?;
        if p.props.alignment != Some(engine::Alignment::Center)
            || doc.resolve_style_run_cascade(Some("Quote")).italic != Some(true)
        {
            bail!(
                "{what}: the x:-prefixed style did not cascade: {:?}",
                p.props
            );
        }
        let head: Vec<&str> = doc
            .headers
            .get("rId2")
            .map(|b| {
                b.iter()
                    .filter_map(engine::Block::as_paragraph)
                    .map(|p| p.text.as_str())
                    .collect()
            })
            .unwrap_or_default();
        if head != ["running head"] {
            bail!("{what}: the x:-prefixed header read {head:?}");
        }
        Ok(())
    };
    check(&a.document, "read")?;
    let mut reported: Vec<&str> = a
        .warnings
        .iter()
        .filter_map(|w| match w {
            format_docx::DocxWarning::NonCanonicalNamespaces {
                part,
                normalized: true,
                ..
            } => Some(part.as_str()),
            _ => None,
        })
        .collect();
    reported.sort();
    if reported != ["word/header1.xml", "word/styles.xml"] || a.warnings.len() != 2 {
        bail!("warnings: {:?}", a.warnings);
    }
    println!(
        "[roundtrip] step 50a OK — x:-prefixed styles.xml / header1.xml cascade and render, reported per part"
    );

    let zero = save_both(&a, &a.document)?;
    if extract_doc_xml(&zero)? != xml.as_bytes() {
        bail!("document.xml drifted on a zero-edit save");
    }
    let z = read_docx(&zero).context("re-read zero-edit save")?;
    if !z.warnings.is_empty() {
        bail!(
            "the normalised parts re-read with warnings: {:?}",
            z.warnings
        );
    }
    for name in ["word/styles.xml", "word/header1.xml"] {
        format_docx::check_part_xml_well_formed(&zero, name)?;
        let bytes = super::entry_bytes(&z, name).context(name)?;
        if bytes.windows(3).any(|w| w == b"<x:") {
            bail!("{name} was re-emitted with its source prefix");
        }
    }
    check(&z.document, "re-read")?;
    let typed = a.document.insert_text(at(0, 6), "!");
    let bytes = save_both(&a, &typed)?;
    if extract_doc_xml(&bytes)? != xml.replacen(">quoted<", ">quoted!<", 1).as_bytes() {
        bail!("typing into the body is not a pure insertion");
    }
    println!(
        "[roundtrip] step 50b OK — the normalised parts are regenerate-only and re-read silently; body edits stay pure insertions"
    );
    Ok(())
}

/* ================================== style paragraph borders (#395) ==== */

/// The painted edges of every paragraph (`T` / `L` / `B` / `R` + colour;
/// `BorderStyle::None` strokes paint nothing).
fn painted_borders(doc: &engine::DocumentTree) -> Vec<String> {
    (0..doc.paragraph_count())
        .map(|i| {
            let Some(b) = doc.nth_paragraph(i).and_then(|p| p.props.borders.clone()) else {
                return String::new();
            };
            [("T", b.top), ("L", b.left), ("B", b.bottom), ("R", b.right)]
                .into_iter()
                .filter_map(|(side, edge)| {
                    let s = edge?;
                    if s.style == engine::BorderStyle::None {
                        return None;
                    }
                    let [r, g, bl, _] = s.color.unwrap_or([0, 0, 0, 255]);
                    Some(format!("{side}:{r:02X}{g:02X}{bl:02X}"))
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// What `styled_paragraph_borders_docx` paints, paragraph by paragraph.
const STYLE_BORDERS: [&str; 7] = [
    "B:4F81BD",
    "T:FF0000 L:FF0000 B:FF0000",
    "L:FF0000 B:FF0000",
    "L:0000FF",
    "R:0000FF",
    "R:0000FF",
    "L:0000FF",
];

/// Issue #395 — step 51 (`format_docx::test_fixtures::
/// styled_paragraph_borders_docx`: the corpus' Word `Title` style, a
/// `basedOn` box, a direct nil, logical start edges on LTR / RTL styles
/// and paragraphs).
///
/// a. The style borders read per edge, by the paragraph's direction; a
///    zero-edit save is byte-identical (`document.xml`, `styles.xml`).
/// b. Typing into the Title is a pure insertion that re-reads the same
///    borders.
/// c. ModifyStyle regenerates `styles.xml` WITH the borders (they used
///    to be lost — never read), which re-read into the same edges.
pub(crate) fn run_style_borders_roundtrip() -> Result<()> {
    let fixture = format_docx::test_fixtures::styled_paragraph_borders_docx();
    let a = read_docx(&fixture).context("read style-borders fixture")?;
    if painted_borders(&a.document) != STYLE_BORDERS {
        bail!("style borders: {:?}", painted_borders(&a.document));
    }
    let zero = save_both(&a, &a.document)?;
    if extract_doc_xml(&zero)? != extract_doc_xml(&fixture)? {
        bail!("document.xml drifted on a zero-edit save");
    }
    let z = read_docx(&zero).context("re-read zero-edit save")?;
    let src = read_docx(&fixture)?;
    if super::entry_bytes(&z, "word/styles.xml") != super::entry_bytes(&src, "word/styles.xml") {
        bail!("styles.xml drifted on a zero-edit save");
    }
    println!(
        "[roundtrip] step 51a OK — style borders cascade per edge, by the paragraph's direction; zero-edit save byte-identical"
    );

    let typed = a.document.insert_text(at(0, 5), " page");
    let bytes = save_both(&a, &typed)?;
    let want =
        String::from_utf8(extract_doc_xml(&fixture)?)?.replacen(">Title<", ">Title page<", 1);
    if extract_doc_xml(&bytes)? != want.as_bytes() {
        bail!("typing into the Title is not a pure insertion");
    }
    let b = read_docx(&bytes).context("re-read the typed Title")?;
    if painted_borders(&b.document) != STYLE_BORDERS {
        bail!("re-read borders: {:?}", painted_borders(&b.document));
    }
    println!("[roundtrip] step 51b OK — typing into a bordered Title is a pure insertion");

    let modified = a.document.modify_style(
        "Box",
        Some(engine::ParaProperties {
            keep_next: Some(true),
            ..Default::default()
        }),
        None,
        None,
        None,
    );
    let bytes = save_both(&a, &modified)?;
    let b = read_docx(&bytes).context("re-read the style edit")?;
    let styles =
        std::str::from_utf8(super::entry_bytes(&b, "word/styles.xml").context("styles.xml")?)?;
    if !styles.contains("<w:pBdr>") || !styles.contains("<w:start ") {
        bail!("the regenerated styles.xml lost the borders:\n{styles}");
    }
    if painted_borders(&b.document) != STYLE_BORDERS {
        bail!(
            "borders after ModifyStyle: {:?}",
            painted_borders(&b.document)
        );
    }
    println!("[roundtrip] step 51c OK — ModifyStyle writes the style borders back");
    Ok(())
}
