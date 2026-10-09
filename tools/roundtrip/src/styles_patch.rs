//! Issue #371 — `ModifyStyle` patches `word/styles.xml` instead of
//! regenerating it: `styles_patch.docx` carries `<w:latentStyles>`,
//! unmodeled `<w:docDefaults>` children, character / table / numbering
//! styles and paragraph styles with unmodeled children.

use super::{entry_bytes, read_docx, write_docx};
use anyhow::{Context, Result, bail};
use engine::SpanStyle;
use format_docx::test_fixtures::{STYLES_PATCH_XML, styles_patch_docx};

/// Step 58:
///
/// a. A zero-edit save keeps `styles.xml` byte-identical.
/// b. `ModifyStyle` on `Heading 1` (a larger size) rewrites exactly its
///    `<w:sz>`: every other `<w:style>` element, `<w:latentStyles>` and
///    `<w:docDefaults>` are byte-identical, and so are Heading 1's own
///    `<w:link>` / `<w:uiPriority>` / `<w:rsid>` / `<w:kern>` / theme
///    attributes; the re-read cascade has the new size.
/// c. A display-name change rewrites only `<w:name>`.
pub fn run_styles_patch_roundtrip() -> Result<()> {
    let fixture = styles_patch_docx();
    let archive = read_docx(&fixture).context("read styles_patch.docx")?;
    let doc = &archive.document;
    let styles_of = |bytes: &[u8]| -> Result<String> {
        let back = read_docx(bytes).context("re-read")?;
        Ok(String::from_utf8(
            entry_bytes(&back, "word/styles.xml")
                .context("styles.xml")?
                .to_vec(),
        )?)
    };
    let zero = write_docx(&archive, doc).context("zero-edit save")?;
    if styles_of(&zero)? != STYLES_PATCH_XML {
        bail!("zero-edit save changed styles.xml");
    }
    println!("[roundtrip] step 58a OK — zero-edit save keeps styles.xml byte-identical");

    let bigger = SpanStyle {
        font_size: Some(20.0),
        ..Default::default()
    };
    let modified = doc.modify_style("Heading1", None, Some(bigger), None, None);
    let bytes = write_docx(&archive, &modified).context("write ModifyStyle")?;
    let out = styles_of(&bytes)?;
    let expected = STYLES_PATCH_XML.replacen(r#"<w:sz w:val="32"/>"#, r#"<w:sz w:val="40"/>"#, 1);
    if out != expected {
        bail!("ModifyStyle rewrote more than Heading 1's <w:sz>:\n{out}");
    }
    let back = read_docx(&bytes).context("re-read ModifyStyle")?;
    if back
        .document
        .resolve_style_run_cascade(Some("Heading1"))
        .font_size
        != Some(20.0)
    {
        bail!("the edited style did not re-read with its new size");
    }
    for needle in [
        "<w:latentStyles ",
        r#"w:styleId="Hyperlink""#,
        r#"w:styleId="TableNormal""#,
        r#"w:styleId="NoList""#,
        r#"w:styleId="Heading1Char""#,
    ] {
        if !out.contains(needle) {
            bail!("ModifyStyle dropped `{needle}`");
        }
    }
    println!(
        "[roundtrip] step 58b OK — ModifyStyle on Heading 1 rewrites only its <w:sz>; every other style element, latentStyles and docDefaults byte-identical"
    );

    let renamed = doc.modify_style("Heading1", None, None, None, Some("Chapter".into()));
    let out = styles_of(&write_docx(&archive, &renamed).context("write rename")?)?;
    if out
        != STYLES_PATCH_XML.replacen(
            r#"<w:name w:val="heading 1"/>"#,
            r#"<w:name w:val="Chapter"/>"#,
            1,
        )
    {
        bail!("a rename rewrote more than <w:name>:\n{out}");
    }
    println!("[roundtrip] step 58c OK — a display-name change rewrites only <w:name>");
    Ok(())
}
