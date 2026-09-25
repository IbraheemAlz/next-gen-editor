//! Issue #278 — note references outside top-level body paragraphs: a
//! footnote referenced from a header part, from a table cell and from a
//! text-box story, next to an ordinary body reference.

use super::{
    WORD_ROOT, assert_document_xml_well_formed, entry_bytes, extract_doc_xml, read_docx, write_docx,
};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, DocumentTree, LogicalPos, NoteAnchor, NoteContainer, NoteKind};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

fn fn_ref(id: u32) -> String {
    format!(
        r#"<w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="{id}"/></w:r>"#
    )
}

fn text_run(t: &str) -> String {
    format!(r#"<w:r><w:t xml:space="preserve">{t}</w:t></w:r>"#)
}

/// An in-line DrawingML text box (Word's choice + VML fallback) whose
/// story references footnote `id`.
fn text_box_with_ref(id: u32) -> String {
    let story = format!("<w:p>{}{}</w:p>", text_run("In the box"), fn_ref(id));
    format!(
        concat!(
            r#"<w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing>"#,
            r#"<wp:inline distT="0" distB="0" distL="0" distR="0"><wp:extent cx="1828800" cy="914400"/>"#,
            r#"<wp:effectExtent l="0" t="0" r="0" b="0"/><wp:docPr id="1" name="Text Box 1"/>"#,
            r#"<a:graphic><a:graphicData uri="http://schemas.microsoft.com/office/word/2010/wordprocessingShape">"#,
            r#"<wps:wsp><wps:cNvSpPr txBox="1"/><wps:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="1828800" cy="914400"/></a:xfrm>"#,
            r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></wps:spPr>"#,
            r#"<wps:txbx><w:txbxContent>{story}</w:txbxContent></wps:txbx>"#,
            r#"<wps:bodyPr rot="0"/></wps:wsp></a:graphicData></a:graphic></wp:inline></w:drawing></mc:Choice>"#,
            r##"<mc:Fallback><w:pict><v:shape id="Text Box 1" o:spid="_x0000_s1026" type="#_x0000_t202" style="width:144pt;height:1in">"##,
            r#"<v:textbox><w:txbxContent>{story}</w:txbxContent></v:textbox></v:shape></w:pict></mc:Fallback>"#,
            r#"</mc:AlternateContent></w:r>"#,
        ),
        story = story
    )
}

fn document_xml() -> String {
    let body = [
        format!("<w:p>{}{}</w:p>", text_run("Body text"), fn_ref(1)),
        format!(
            concat!(
                r#"<w:tbl><w:tblGrid><w:gridCol w:w="4000"/></w:tblGrid><w:tr><w:tc>"#,
                "<w:p>{}{}</w:p>",
                "</w:tc></w:tr></w:tbl>"
            ),
            text_run("Cell text"),
            fn_ref(2)
        ),
        format!(
            "<w:p>{}{}{}</w:p>",
            text_run("Host "),
            text_box_with_ref(3),
            text_run(" after")
        ),
    ]
    .concat();
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n{root}\r\n<w:body>{body}",
            r#"<w:sectPr><w:headerReference w:type="default" r:id="rId4"/>"#,
            r#"<w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1417" w:right="1417" w:bottom="1134" w:left="1417" w:header="708" w:footer="708" w:gutter="0"/></w:sectPr>"#,
            "</w:body>\r\n</w:document>\r\n"
        ),
        root = WORD_ROOT,
        body = body
    )
}

fn header_xml() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:hdr xmlns:w="{W_NS}"><w:p>{}{}</w:p></w:hdr>"#,
        text_run("Running head"),
        fn_ref(4)
    )
}

fn footnotes_xml() -> String {
    let note = |id: u32, text: &str| {
        format!(
            r#"<w:footnote w:id="{id}"><w:p><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t xml:space="preserve"> {text}</w:t></w:r></w:p></w:footnote>"#
        )
    };
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            "\n",
            r#"<w:footnotes xmlns:w="{w}">"#,
            r#"<w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>"#,
            r#"<w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>"#,
            "{n1}{n2}{n3}{n4}</w:footnotes>"
        ),
        w = W_NS,
        n1 = note(1, "Body note."),
        n2 = note(2, "Cell note."),
        n3 = note(3, "Box note."),
        n4 = note(4, "Header note."),
    )
}

pub(crate) fn build_note_containers_docx() -> Vec<u8> {
    use std::io::Write;
    use zip::write::{SimpleFileOptions, ZipWriter};
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/>
<Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/>
</Types>"#;
    let dot_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;
    let doc_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes" Target="footnotes.xml"/>
<Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>
</Relationships>"#;
    let document = document_xml();
    let footnotes = footnotes_xml();
    let header = header_xml();
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut zip = ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        for (name, body) in [
            ("[Content_Types].xml", content_types),
            ("_rels/.rels", dot_rels),
            ("word/_rels/document.xml.rels", doc_rels),
            ("word/document.xml", document.as_str()),
            ("word/footnotes.xml", footnotes.as_str()),
            ("word/header1.xml", header.as_str()),
        ] {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    buf
}

fn footnote(id: u32) -> NoteAnchor {
    NoteAnchor {
        kind: NoteKind::Footnote,
        id,
    }
}

fn refs_of(doc: &DocumentTree) -> Vec<(u32, NoteContainer)> {
    doc.note_references()
        .iter()
        .map(|r| (r.anchor.id, r.container))
        .collect()
}

const WANT_REFS: [(u32, NoteContainer); 4] = [
    (4, NoteContainer::Header),
    (1, NoteContainer::Body),
    (2, NoteContainer::TableCell),
    (3, NoteContainer::TextBox),
];

/// Issue #278 — step 35: note references in a header, a table cell and a
/// text box.
///
/// a. All four references read, in document order (the header's first —
///    it opens page 1), and number 1–4 continuously; the HTML export
///    links every one of them to its note region.
/// b. A zero-edit save is byte-identical.
/// c. Editing the BODY note regenerates `footnotes.xml`; the notes
///    referenced only from the header, the cell and the text box stay in
///    it (the writer's keep-list walks every story), and the re-read
///    document numbers the same four references the same way.
pub(crate) fn run_note_containers_roundtrip() -> Result<()> {
    let fixture = build_note_containers_docx();
    let a = read_docx(&fixture).context("read note-containers fixture")?;
    let doc = &a.document;
    if refs_of(doc) != WANT_REFS {
        bail!("note references: {:?}", refs_of(doc));
    }
    let markers = doc.note_markers();
    for (id, want) in [(4, "1"), (1, "2"), (2, "3"), (3, "4")] {
        if markers.get(&footnote(id)).map(String::as_str) != Some(want) {
            bail!(
                "footnote {id} marker: {:?} (want {want})",
                markers.get(&footnote(id))
            );
        }
    }
    let html = format_html::to_html(doc);
    for id in 1..=4 {
        let link = format!("href=\"#footnote-{id}\"");
        let region = format!("<aside role=\"doc-footnote\" id=\"footnote-{id}\">");
        if !html.contains(&link) || !html.contains(&region) {
            bail!("HTML export lacks footnote {id}'s link or region");
        }
    }
    println!(
        "[roundtrip] step 35a OK — header / cell / text-box footnote references read, number 1-4 and export"
    );

    let zero = write_docx(&a, doc).context("zero-edit save")?;
    let z = read_docx(&zero).context("re-read zero-edit save")?;
    if extract_doc_xml(&fixture)? != extract_doc_xml(&zero)? {
        bail!("document.xml drifted on a zero-edit save");
    }
    for part in ["word/footnotes.xml", "word/header1.xml"] {
        if entry_bytes(&a, part) != entry_bytes(&z, part) {
            bail!("`{part}` drifted on a zero-edit save");
        }
    }
    println!("[roundtrip] step 35b OK — zero-edit save is byte-identical");

    let story = &doc.footnote_stories[&1];
    let story_doc = DocumentTree::from_blocks(story.body.iter().cloned());
    let end = story_doc.paragraph_text(0).map_or(0, |t| t.len() as u32);
    let story_doc = story_doc.insert_text(
        LogicalPos {
            path: BlockPath::top(0),
            offset: end,
        },
        " (edited)",
    );
    let edited = doc.with_updated_note_story(
        NoteKind::Footnote,
        1,
        story_doc.blocks.iter().cloned().collect(),
    );
    let bytes = write_docx(&a, &edited).context("save after a body-note edit")?;
    assert_document_xml_well_formed(&bytes)?;
    let b = read_docx(&bytes).context("re-read after a body-note edit")?;
    let part = std::str::from_utf8(entry_bytes(&b, "word/footnotes.xml").context("footnotes")?)?;
    if !part.contains("Body note. (edited)") {
        bail!("the edited note was not regenerated:\n{part}");
    }
    for text in ["Cell note.", "Box note.", "Header note."] {
        if !part.contains(text) {
            bail!("`{text}` dropped from the regenerated footnotes.xml:\n{part}");
        }
    }
    if refs_of(&b.document) != WANT_REFS || b.document.note_markers() != markers {
        bail!(
            "re-read references / markers moved: {:?}",
            refs_of(&b.document)
        );
    }
    println!(
        "[roundtrip] step 35c OK — a regenerated footnotes.xml keeps the header / cell / box notes"
    );
    Ok(())
}
