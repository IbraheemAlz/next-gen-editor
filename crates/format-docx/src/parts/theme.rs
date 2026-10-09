//! Issue #355 — `word/theme/theme1.xml` (`a:theme`, ECMA-376 Part 1
//! §20.1.6.9) → [`engine::DocumentTheme`]: the font scheme (`a:fontScheme`,
//! §20.1.4.1.18 — `a:majorFont` / `a:minorFont`, each with `a:latin`,
//! `a:ea`, `a:cs` and the supplemental `<a:font script typeface>` list) and
//! the colour scheme (`a:clrScheme`, §20.1.6.2 — twelve slots, each an
//! `a:srgbClr val` or an `a:sysClr` whose `lastClr` caches the system
//! colour). Read-only: the part keeps riding `other_entries` verbatim.
//!
//! Matching is by LOCAL name (`latin`, not `a:latin`): DrawingML parts are
//! not bound to a fixed prefix, and a producer that declares the namespace
//! as the default (`<theme xmlns="…/drawingml/2006/main">`) is legal.

use crate::error::DocxError;
use engine::{DocumentTheme, SchemeColor, ThemeFonts};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// Where Word puts the theme when no relationship says otherwise.
pub const DEFAULT_THEME_PART: &str = "word/theme/theme1.xml";

fn local_name(qname: &[u8]) -> &[u8] {
    match qname.iter().rposition(|b| *b == b':') {
        Some(i) => &qname[i + 1..],
        None => qname,
    }
}

/// Unprefixed attribute `key`, unescaped.
fn attr(e: &BytesStart, key: &[u8]) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

fn parse_rgb(v: &str) -> Option<[u8; 3]> {
    let v = v.trim();
    if v.len() != 6 || !v.is_ascii() {
        return None;
    }
    let d = |i: usize| u8::from_str_radix(&v[i..i + 2], 16).ok();
    Some([d(0)?, d(2)?, d(4)?])
}

/// The document's theme part, located through `word/_rels/document.xml.rels`
/// (the `…/relationships/theme` row — Transitional or Strict) with Word's
/// conventional path as the fallback, parsed and joined with the
/// `<w:themeFontLang>` / `<w:clrSchemeMapping>` settings. `None` when the
/// package has no theme part or it does not parse.
pub fn read_document_theme(
    entries: &[(String, Vec<u8>)],
    settings: Option<&crate::parts::settings::SettingsPart>,
) -> Option<DocumentTheme> {
    let entry = |name: &str| {
        entries
            .iter()
            .find_map(|(n, b)| (n == name).then_some(b.as_slice()))
    };
    let from_rels = entry(crate::opc::archive::RELS_XML)
        .and_then(|b| crate::opc::relationships::parse_relationships(b).ok())
        .and_then(|rels| {
            rels.items
                .iter()
                .find(|r| r.rel_type.ends_with("/relationships/theme"))
                .map(|r| crate::parts::rels::resolve_target(&r.target))
        });
    let bytes = from_rels
        .as_deref()
        .and_then(entry)
        .or_else(|| entry(DEFAULT_THEME_PART))?;
    let mut theme = parse_theme_xml(bytes).ok()?;
    if let Some(s) = settings {
        theme.font_lang = s.theme_font_lang.clone();
        theme.color_map = s.clr_scheme_mapping.clone();
    }
    Some(theme)
}

/// Which font collection a `typeface` child belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Collection {
    Major,
    Minor,
}

/// Parse a theme part. Unknown / unmodeled content is skipped; a part with
/// no font or colour scheme yields the empty model (every lookup misses
/// and layout falls back exactly as it did before the theme was read).
pub fn parse_theme_xml(xml: &[u8]) -> Result<DocumentTheme, DocxError> {
    let mut reader = Reader::from_reader(crate::parts::document::strip_utf8_bom(xml));
    reader.config_mut().trim_text(false);
    let mut theme = DocumentTheme::default();
    let mut buf = Vec::new();
    /* Element stack by local name, so a child is interpreted only under
    the parent the schema puts it in (an `a:latin` inside an effect style
    or an `a:srgbClr` inside a fill list never leaks into the schemes). */
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut collection: Option<Collection> = None;
    loop {
        let evt = reader.read_event_into(&mut buf)?;
        let (e, empty) = match &evt {
            Event::Start(e) => (e, false),
            Event::Empty(e) => (e, true),
            Event::End(_) => {
                if let Some(name) = stack.pop()
                    && matches!(name.as_slice(), b"majorFont" | b"minorFont")
                {
                    collection = None;
                }
                buf.clear();
                continue;
            }
            Event::Eof => break,
            _ => {
                buf.clear();
                continue;
            }
        };
        let name = local_name(e.name().as_ref()).to_vec();
        let parent = stack.last().map(Vec::as_slice);
        match (parent, name.as_slice()) {
            (None, b"theme") => theme.name = attr(e, b"name").unwrap_or_default(),
            (Some(b"themeElements"), b"clrScheme") => {
                theme.colors.name = attr(e, b"name").unwrap_or_default();
            }
            (Some(p), b"srgbClr" | b"sysClr")
                if stack.len() >= 2 && stack[stack.len() - 2] == b"clrScheme" =>
            {
                /* `a:sysClr` names a live system colour (`windowText`); its
                `lastClr` is the value the producer saw, which is what Word
                itself renders with when it cannot ask the OS. */
                let key: &[u8] = if name == b"srgbClr" {
                    b"val"
                } else {
                    b"lastClr"
                };
                if let Some(slot) = SchemeColor::from_scheme_element(&String::from_utf8_lossy(p))
                    && let Some(rgb) = attr(e, key).as_deref().and_then(parse_rgb)
                    && theme.colors.get(slot).is_none()
                {
                    theme.colors.set(slot, rgb);
                }
            }
            (Some(b"fontScheme"), b"majorFont") => collection = Some(Collection::Major),
            (Some(b"fontScheme"), b"minorFont") => collection = Some(Collection::Minor),
            (Some(b"majorFont" | b"minorFont"), child) if collection.is_some() => {
                let fonts: &mut ThemeFonts = match collection {
                    Some(Collection::Major) => &mut theme.fonts.major,
                    _ => &mut theme.fonts.minor,
                };
                let typeface = attr(e, b"typeface").unwrap_or_default();
                match child {
                    b"latin" => fonts.latin = typeface,
                    b"ea" => fonts.ea = typeface,
                    b"cs" => fonts.cs = typeface,
                    b"font" => {
                        if let Some(script) = attr(e, b"script")
                            && !script.is_empty()
                        {
                            /* First entry wins — a duplicate script row is
                            malformed; Word keeps the first it reads. */
                            fonts.by_script.entry(script).or_insert(typeface);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        if !empty {
            stack.push(name);
        }
        buf.clear();
    }
    Ok(theme)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape Word writes for its stock "Office" theme (2013+): empty
    /// `a:ea` / `a:cs`, the complex-script and East Asian faces per
    /// script, system colours for dk1 / lt1.
    const WORD_THEME: &str = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        "\r\n",
        r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" name="Office Theme">"#,
        r#"<a:themeElements><a:clrScheme name="Office">"#,
        r#"<a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1>"#,
        r#"<a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1>"#,
        r#"<a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2>"#,
        r#"<a:accent1><a:srgbClr val="4472C4"/></a:accent1><a:accent2><a:srgbClr val="ED7D31"/></a:accent2>"#,
        r#"<a:accent3><a:srgbClr val="A5A5A5"/></a:accent3><a:accent4><a:srgbClr val="FFC000"/></a:accent4>"#,
        r#"<a:accent5><a:srgbClr val="5B9BD5"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6>"#,
        r#"<a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink>"#,
        r#"</a:clrScheme><a:fontScheme name="Office">"#,
        r#"<a:majorFont><a:latin typeface="Calibri Light" panose="020F0302020204030204"/><a:ea typeface=""/><a:cs typeface=""/>"#,
        r#"<a:font script="Jpan" typeface="游ゴシック Light"/><a:font script="Arab" typeface="Times New Roman"/>"#,
        r#"<a:font script="Hebr" typeface="Times New Roman"/></a:majorFont>"#,
        r#"<a:minorFont><a:latin typeface="Calibri" panose="020F0502020204030204"/><a:ea typeface=""/><a:cs typeface=""/>"#,
        r#"<a:font script="Jpan" typeface="游明朝"/><a:font script="Arab" typeface="Arial"/>"#,
        r#"<a:font script="Hebr" typeface="Arial"/></a:minorFont></a:fontScheme>"#,
        r#"<a:fmtScheme name="Office"><a:fillStyleLst><a:solidFill><a:srgbClr val="FF0000"/></a:solidFill>"#,
        r#"</a:fillStyleLst></a:fmtScheme></a:themeElements></a:theme>"#,
    );

    #[test]
    fn reads_the_word_default_font_and_colour_schemes() {
        let t = parse_theme_xml(WORD_THEME.as_bytes()).expect("parse");
        assert_eq!(t.name, "Office Theme");
        assert_eq!(t.fonts.major.latin, "Calibri Light");
        assert_eq!(t.fonts.minor.latin, "Calibri");
        assert_eq!(t.fonts.minor.cs, "");
        assert_eq!(t.fonts.minor.by_script["Arab"], "Arial");
        assert_eq!(t.fonts.major.by_script["Arab"], "Times New Roman");
        assert_eq!(t.fonts.minor.by_script["Jpan"], "游明朝");
        assert_eq!(t.fonts.minor.by_script.len(), 3);
        assert_eq!(t.colors.name, "Office");
        assert_eq!(t.colors.get(SchemeColor::Dark1), Some([0, 0, 0]));
        assert_eq!(t.colors.get(SchemeColor::Light1), Some([255, 255, 255]));
        assert_eq!(t.colors.get(SchemeColor::Accent1), Some([0x44, 0x72, 0xC4]));
        assert_eq!(
            t.colors.get(SchemeColor::FollowedHyperlink),
            Some([0x95, 0x4F, 0x72])
        );
        assert!(SchemeColor::ALL.iter().all(|c| t.colors.get(*c).is_some()));
    }

    /// A default-namespace theme (no `a:` prefix) parses the same; a
    /// colour under a fill list never lands in a scheme slot.
    #[test]
    fn matches_by_local_name_and_scope() {
        let xml = concat!(
            r#"<theme xmlns="http://schemas.openxmlformats.org/drawingml/2006/main" name="T">"#,
            r#"<themeElements><clrScheme name="C"><accent2><srgbClr val="112233"/></accent2></clrScheme>"#,
            r#"<fontScheme name="F"><minorFont><latin typeface="Amiri"/><cs typeface="Noto Naskh Arabic"/>"#,
            r#"<font script="Arab" typeface="Amiri"/><font script="Arab" typeface="Second"/></minorFont></fontScheme>"#,
            r#"<fmtScheme><fillStyleLst><solidFill><srgbClr val="FF0000"/></solidFill></fillStyleLst>"#,
            r#"<effectStyleLst><latin typeface="Nope"/></effectStyleLst></fmtScheme>"#,
            r#"</themeElements></theme>"#,
        );
        let t = parse_theme_xml(xml.as_bytes()).expect("parse");
        assert_eq!(t.colors.get(SchemeColor::Accent2), Some([0x11, 0x22, 0x33]));
        assert_eq!(t.colors.get(SchemeColor::Accent1), None);
        assert_eq!(t.fonts.minor.latin, "Amiri");
        assert_eq!(t.fonts.minor.cs, "Noto Naskh Arabic");
        assert_eq!(t.fonts.minor.by_script["Arab"], "Amiri", "first entry wins");
        assert_eq!(t.fonts.major, ThemeFonts::default());
    }

    fn themed_package(settings: bool) -> Vec<u8> {
        let base = crate::writer::build_minimal_docx(&engine::DocumentTree::from_text("Body"))
            .expect("minimal");
        let settings_xml = crate::test_fixtures::theme_settings_xml();
        crate::test_fixtures::with_theme_parts(
            &base,
            &crate::test_fixtures::word_default_theme_xml(),
            settings.then_some(settings_xml.as_str()),
        )
    }

    /// The reader lands the theme on the tree, joined with the settings
    /// that select into it; the part itself stays a byte-identical
    /// sibling on both save paths.
    #[test]
    fn read_docx_lands_the_theme_and_keeps_the_part_verbatim() {
        let bytes = themed_package(true);
        let archive = crate::read_docx(&bytes).expect("read");
        let theme = archive.document.theme.as_deref().expect("theme read");
        assert_eq!(theme.fonts.minor.latin, "Calibri");
        assert_eq!(theme.fonts.major.by_script["Arab"], "Times New Roman");
        assert_eq!(theme.font_lang.bidi.as_deref(), Some("ar-SA"));
        assert_eq!(theme.color_map.entries["t1"], "dark1");
        let source = archive
            .part_by_name(DEFAULT_THEME_PART)
            .expect("part")
            .to_vec();
        for saved in [
            crate::write_docx(&archive, &archive.document).expect("write"),
            crate::save_docx(&archive.document).expect("save"),
        ] {
            let back = crate::read_docx(&saved).expect("reread");
            assert_eq!(
                back.part_by_name(DEFAULT_THEME_PART),
                Some(source.as_slice())
            );
            assert_eq!(back.document.theme, archive.document.theme);
        }
        /* No settings part → the identity defaults (empty selections). */
        let bare = crate::read_docx(&themed_package(false)).expect("read");
        let theme = bare.document.theme.as_deref().expect("theme read");
        assert_eq!(theme.font_lang, engine::ThemeFontLang::default());
        assert!(theme.color_map.entries.is_empty());
        /* No theme part at all → no model. */
        let plain = crate::writer::build_minimal_docx(&engine::DocumentTree::from_text("x"))
            .expect("minimal");
        assert!(
            crate::read_docx(&plain)
                .expect("read")
                .document
                .theme
                .is_none()
        );
    }

    /// The relationship wins over the conventional path.
    #[test]
    fn the_theme_relationship_locates_the_part() {
        let mut entries = vec![
            (
                crate::opc::archive::RELS_XML.to_string(),
                format!(
                    "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
                     <Relationship Id=\"rId9\" Type=\"{}\" Target=\"theme/theme7.xml\"/></Relationships>",
                    crate::test_fixtures::THEME_REL
                )
                .into_bytes(),
            ),
            ("word/theme/theme7.xml".to_string(), WORD_THEME.as_bytes().to_vec()),
        ];
        let t = read_document_theme(&entries, None).expect("theme");
        assert_eq!(t.fonts.minor.latin, "Calibri");
        entries[1].0 = "word/theme/elsewhere.xml".into();
        assert!(read_document_theme(&entries, None).is_none());
    }

    #[test]
    fn malformed_values_are_skipped_not_fatal() {
        let xml = concat!(
            r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">"#,
            r#"<a:themeElements><a:clrScheme name="C"><a:dk1><a:srgbClr val="nothex"/></a:dk1>"#,
            r#"<a:lt1><a:scrgbClr r="0" g="0" b="0"/></a:lt1></a:clrScheme>"#,
            r#"<a:fontScheme name="F"><a:majorFont><a:font typeface="NoScript"/></a:majorFont>"#,
            r#"</a:fontScheme></a:themeElements></a:theme>"#,
        );
        let t = parse_theme_xml(xml.as_bytes()).expect("parse");
        assert_eq!(t.colors.get(SchemeColor::Dark1), None);
        assert_eq!(t.colors.get(SchemeColor::Light1), None);
        assert!(t.fonts.major.by_script.is_empty());
    }
}
