//! Issue #355 — theme fonts and colours: `theme_word_default.docx` (a Word
//! default-template document — body text bound to `+Body` through
//! docDefaults, Arabic to `+Body CS` through the theme's `Arab` row, a
//! heading on `+Headings`, theme-coloured runs) through read → edit →
//! save.

use super::{assert_document_xml_well_formed, entry_bytes, extract_doc_xml, read_docx, write_docx};
use anyhow::{Context, Result, bail};
use engine::{BlockPath, FontClass, LogicalPos, SpanStyle};
use format_docx::test_fixtures::{THEME_FIXTURE_TEXTS, theme_word_default_docx};

fn at(block: u32, offset: usize) -> LogicalPos {
    LogicalPos::new(BlockPath::top(block), offset as u32)
}

/// Family names the layout resolves for paragraph `idx`'s first run of
/// text: `(Latin, complex script)`.
fn resolved(doc: &engine::DocumentTree, idx: u32) -> (Option<String>, Option<String>) {
    let p = doc.nth_paragraph(idx).expect("paragraph");
    let base = doc.resolve_style_run_cascade(p.style_id.as_deref());
    let style = match p.spans.first().filter(|s| s.start == 0) {
        Some(run) => base.merged_with(run.style.clone()),
        None => base,
    };
    let theme = doc.theme.as_deref();
    let name = |class| {
        style
            .resolve_font(theme, class, Some("Arab"))
            .map(|r| r.family.display_name().to_string())
    };
    (name(FontClass::Latin), name(FontClass::ComplexScript))
}

/// Step 46:
///
/// a. The theme is modeled and every theme-bound run resolves: body
///    Calibri / Arial, heading Calibri Light / Times New Roman; the
///    heading's `accent1` + `BF` shade resolves to Word's cached
///    `2F5496`. A zero-edit save is byte-identical, theme part included.
/// b. Typing into the heading and the Arabic paragraph is a pure
///    insertion: the run properties (theme attributes included) are
///    reused verbatim, the theme part untouched.
/// c. A toolbar font on a theme-bound run writes the explicit family and
///    drops exactly the theme bindings it claims (`w:cstheme` kept on a
///    run that only rebinds Arabic is not touched elsewhere); the reread
///    lays out in the new family.
pub fn run_theme_fonts_roundtrip() -> Result<()> {
    let fixture = theme_word_default_docx();
    let archive = read_docx(&fixture).context("read theme fixture")?;
    let doc = &archive.document;
    let theme = doc.theme.as_deref().context("theme not modeled")?;
    if theme.fonts.minor.latin != "Calibri"
        || theme.fonts.major.by_script["Arab"] != "Times New Roman"
    {
        bail!("theme font scheme misread: {:?}", theme.fonts);
    }
    let some = |a: &str, b: &str| (Some(a.to_string()), Some(b.to_string()));
    for (idx, want) in [
        (0, some("Calibri Light", "Times New Roman")),
        (1, some("Calibri", "Arial")),
        (2, some("Calibri", "Arial")),
    ] {
        let got = resolved(doc, idx);
        if got != want {
            bail!("paragraph {idx}: resolved {got:?}, expected {want:?}");
        }
    }
    let heading = doc.resolve_style_run_cascade(Some("Heading1"));
    if heading.resolve_color(Some(theme)) != Some([0x2F, 0x54, 0x96, 255]) {
        bail!(
            "heading theme colour: {:?}",
            heading.resolve_color(Some(theme))
        );
    }
    /* The theme part is the one `PartNames` discovered (#353) — here
    Word's path, related from the document rels. */
    let theme_part = archive.part_names.theme.as_str();
    if theme_part != "word/theme/theme1.xml" {
        bail!("theme part discovered as {theme_part:?}");
    }
    let saved = write_docx(&archive, doc).context("zero-edit save")?;
    let saved_archive = read_docx(&saved).context("reread zero-edit save")?;
    if extract_doc_xml(&saved)? != extract_doc_xml(&fixture)?
        || entry_bytes(&saved_archive, theme_part) != entry_bytes(&archive, theme_part)
        || entry_bytes(&saved_archive, theme_part).is_none()
    {
        bail!("zero-edit save drifted");
    }
    println!(
        "[roundtrip] step 46a OK — theme fonts / colours resolve; zero-edit save byte-identical"
    );

    let source = String::from_utf8(extract_doc_xml(&fixture)?)?;
    let heading_end = THEME_FIXTURE_TEXTS[0].len();
    let arabic_end = THEME_FIXTURE_TEXTS[2].len();
    let edited = doc
        .insert_text(at(0, heading_end), " 2")
        .insert_text(at(2, arabic_end), " تم");
    let bytes = write_docx(&archive, &edited).context("write edited")?;
    assert_document_xml_well_formed(&bytes).context("edited theme fixture")?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?)?;
    let expected = source
        .replacen(
            &format!(">{}<", THEME_FIXTURE_TEXTS[0]),
            &format!(">{} 2<", THEME_FIXTURE_TEXTS[0]),
            1,
        )
        /* Paragraph 2's last run is the bare "." (paragraph 3's comes later). */
        .replacen(">.</w:t>", ">. تم</w:t>", 1);
    if out != expected {
        bail!("edited save is not a pure insertion:\n{out}\nexpected:\n{expected}");
    }
    let back = read_docx(&bytes).context("reread edited")?;
    if entry_bytes(&back, theme_part) != entry_bytes(&archive, theme_part) {
        bail!("theme part drifted on an edited save");
    }
    if resolved(&back.document, 0) != some("Calibri Light", "Times New Roman") {
        bail!("edited heading lost its theme binding");
    }
    println!("[roundtrip] step 46b OK — typing into theme-bound runs is a pure insertion");

    /* The toolbar picks Amiri for the body paragraph's first run — both
    script slots, as the ribbon does (issues #359 / #249). */
    let body_len = THEME_FIXTURE_TEXTS[1].find("explicit").expect("needle");
    let amiri = SpanStyle {
        font_family: Some(engine::FontFamily::Amiri),
        ..Default::default()
    };
    let refonted = doc.apply_style(at(1, 0), at(1, body_len), amiri.clone().with_cs_twins());
    let bytes = write_docx(&archive, &refonted).context("write re-fonted")?;
    assert_document_xml_well_formed(&bytes).context("re-fonted theme fixture")?;
    let out = String::from_utf8(extract_doc_xml(&bytes)?)?;
    if !out.contains(
        r#"<w:rFonts w:ascii="Amiri" w:hAnsi="Amiri" w:cs="Amiri"/></w:rPr><w:t xml:space="preserve">Body text"#,
    ) {
        bail!("the picked family was not written:\n{out}");
    }
    if !out.contains(r#"<w:rFonts w:cstheme="majorBidi"/>"#) {
        bail!("an unrelated run lost its complex-script binding:\n{out}");
    }
    let back = read_docx(&bytes).context("reread re-fonted")?;
    if resolved(&back.document, 1) != some("Amiri", "Amiri") {
        bail!("re-fonted run resolves {:?}", resolved(&back.document, 1));
    }
    if resolved(&back.document, 2) != some("Calibri", "Arial") {
        bail!("an untouched paragraph changed its theme fonts");
    }
    /* Issue #249 — a Latin-only pick (`FontSlot::Latin`) claims ascii /
    hAnsi alone: the run's complex-script slot stays on the theme. */
    let latin_only = doc.apply_style(at(1, 0), at(1, body_len), amiri);
    let bytes = write_docx(&archive, &latin_only).context("write Latin-only re-font")?;
    assert_document_xml_well_formed(&bytes).context("Latin-only re-font")?;
    let back = read_docx(&bytes).context("reread Latin-only re-font")?;
    if resolved(&back.document, 1) != some("Amiri", "Arial") {
        bail!(
            "Latin-only re-font resolves {:?}",
            resolved(&back.document, 1)
        );
    }
    println!(
        "[roundtrip] step 46c OK — a picked family claims its slots (a Latin-only pick keeps the cs theme face); other bindings untouched"
    );
    Ok(())
}

/// Step 47 (issue #424) — a family the font registry does not ship
/// ("Sakkal Majalla") claims exactly the script slot the pick targeted,
/// whichever way the engine holds it: as the `FontFamily::Custom` the
/// UI's `ApplyFormatting` builds from the picked id, or as an unresolved
/// `raw_font_family` name. On Word's default template (every slot bound
/// to the theme through docDefaults):
///
/// a. A complex-script pick writes `w:cs="Sakkal Majalla"` and nothing
///    for `w:ascii` / `w:hAnsi`: the reread resolves Calibri (theme) for
///    the Latin text and Sakkal Majalla for the Arabic.
/// b. A Latin pick writes `w:ascii` / `w:hAnsi` only; the Arabic stays on
///    the theme's Arial.
/// c. A both-slot pick (the ribbon) writes all three names.
/// d. The zero-edit save is still byte-identical.
pub fn run_unregistered_family_roundtrip() -> Result<()> {
    const NAME: &str = "Sakkal Majalla";
    let fixture = theme_word_default_docx();
    let archive = read_docx(&fixture).context("read theme fixture")?;
    let doc = &archive.document;
    let body_len = THEME_FIXTURE_TEXTS[1].find("explicit").expect("needle");
    let picked = r#"<w:t xml:space="preserve">Body text"#;
    let some = |a: &str, b: &str| (Some(a.to_string()), Some(b.to_string()));
    /* The UI path builds a `Custom` face from the picked id
    (`engine-wasm`'s `parse_font_family`); a raw name is the other shape
    an unresolvable family can take. Both must claim the same slots. */
    let shapes = [
        (
            "Custom",
            SpanStyle {
                font_family: engine::FontFamily::from_id(NAME),
                ..Default::default()
            },
        ),
        (
            "raw",
            SpanStyle {
                raw_font_family: Some(NAME.into()),
                ..Default::default()
            },
        ),
    ];
    for (shape, latin) in shapes {
        for (step, patch, rfonts, want) in [
            (
                "47a",
                latin.clone().into_cs_only(),
                format!(r#"<w:rFonts w:cs="{NAME}"/>"#),
                some("Calibri", NAME),
            ),
            (
                "47b",
                latin.clone(),
                format!(r#"<w:rFonts w:ascii="{NAME}" w:hAnsi="{NAME}"/>"#),
                some(NAME, "Arial"),
            ),
            (
                "47c",
                latin.clone().with_cs_twins(),
                format!(r#"<w:rFonts w:ascii="{NAME}" w:hAnsi="{NAME}" w:cs="{NAME}"/>"#),
                some(NAME, NAME),
            ),
        ] {
            let edited = doc.apply_style(at(1, 0), at(1, body_len), patch);
            let bytes = write_docx(&archive, &edited)
                .with_context(|| format!("step {step} ({shape}): write"))?;
            assert_document_xml_well_formed(&bytes)
                .with_context(|| format!("step {step} ({shape})"))?;
            let out = String::from_utf8(extract_doc_xml(&bytes)?)?;
            let run = format!("{rfonts}</w:rPr>{picked}");
            if !out.contains(&run) {
                bail!("step {step} ({shape}): expected the run to write {rfonts}:\n{out}");
            }
            let back = read_docx(&bytes).with_context(|| format!("step {step}: reread"))?;
            if resolved(&back.document, 1) != want {
                bail!(
                    "step {step} ({shape}): the reread resolves {:?}, expected {want:?}",
                    resolved(&back.document, 1)
                );
            }
            if resolved(&back.document, 2) != some("Calibri", "Arial") {
                bail!("step {step} ({shape}): an untouched paragraph changed its fonts");
            }
        }
    }
    let saved = write_docx(&archive, doc).context("zero-edit save")?;
    if extract_doc_xml(&saved)? != extract_doc_xml(&fixture)? {
        bail!("step 47d: zero-edit save drifted");
    }
    println!(
        "[roundtrip] step 47 OK — an unregistered family ({NAME}) claims exactly the picked slot (cs-only → w:cs, Latin-only → w:ascii/w:hAnsi, both → all three; Custom and raw alike); zero-edit identity holds"
    );
    Ok(())
}
