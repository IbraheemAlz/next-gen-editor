//! `word/settings.xml` — minimal read-only model (Phase 6).
//!
//! Phase 6 only needs to know the part exists for pass-through; the
//! engine has no Settings consumer yet. The struct lands so later
//! phases can grow typed fields (zoom, default tab stop, document
//! direction) without touching the OPC reader.

use crate::error::DocxError;
use quick_xml::events::Event;
use quick_xml::reader::Reader;

#[derive(Debug, Clone, Default)]
pub struct SettingsPart {
    /// `<w:defaultTabStop w:val="N"/>` — twips. `None` when absent.
    pub default_tab_stop_twips: Option<u32>,
    /// `<w:evenAndOddHeaders/>` — Phase 2 audit. Toggle element; when
    /// `true`, even pages should render the `Even` header / footer
    /// slot instead of the `Default`. The paginator consumes this
    /// alongside each section's per-role header table.
    pub even_and_odd_headers: bool,
    /// Issue #80 — document-level `<w:footnotePr>` (position, number
    /// format / start / restart). The `<w:footnote w:id>` separator
    /// references inside it are not modelled (the stories carry their
    /// own `w:type`).
    pub footnote_props: engine::NoteProps,
    /// Issue #80 — document-level `<w:endnotePr>`.
    pub endnote_props: engine::NoteProps,
    /// Issue #355 — `<w:themeFontLang>`: the languages whose scripts pick
    /// the theme's supplemental `<a:font script>` entries.
    pub theme_font_lang: engine::ThemeFontLang,
    /// Issue #355 — `<w:clrSchemeMapping>`: logical theme colour → scheme
    /// slot, attribute local name → value, verbatim.
    pub clr_scheme_mapping: engine::ColorSchemeMapping,
}

/// Decode an OOXML toggle attribute (`w:val` "false" / "0" / "off"
/// → off; anything else / absent → on). Mirrors the `toggle_on`
/// helper in `ct_rpr` so behaviour stays consistent without the
/// schema module dependency.
fn toggle_attr(attrs: quick_xml::events::attributes::Attributes<'_>) -> bool {
    attrs
        .flatten()
        .find(|a| a.key.as_ref() == b"w:val")
        .and_then(|a| a.unescape_value().ok())
        .is_none_or(|v| !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "off"))
}

pub fn parse_settings_xml(xml: &[u8]) -> Result<SettingsPart, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut out = SettingsPart::default();
    let mut buf = Vec::new();
    /* Issue #80 — which `<w:footnotePr>` / `<w:endnotePr>` container is
    open; leaf children fold into the scoped props. */
    let mut note_scope: Option<engine::NoteKind> = None;
    loop {
        let evt = reader.read_event_into(&mut buf)?;
        match evt {
            Event::Start(e) if e.name().as_ref() == b"w:footnotePr" => {
                note_scope = Some(engine::NoteKind::Footnote);
            }
            Event::Start(e) if e.name().as_ref() == b"w:endnotePr" => {
                note_scope = Some(engine::NoteKind::Endnote);
            }
            Event::End(e) if matches!(e.name().as_ref(), b"w:footnotePr" | b"w:endnotePr") => {
                note_scope = None;
            }
            Event::Empty(e) | Event::Start(e) if note_scope.is_some() => {
                let props = match note_scope {
                    Some(engine::NoteKind::Footnote) => &mut out.footnote_props,
                    _ => &mut out.endnote_props,
                };
                let name = e.name();
                crate::parts::footnotes::apply_note_pr_child(name.as_ref(), &e, props);
            }
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"w:defaultTabStop" => {
                out.default_tab_stop_twips = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"w:val")
                    .and_then(|a| a.unescape_value().ok())
                    .and_then(|v| v.parse().ok());
            }
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"w:evenAndOddHeaders" => {
                out.even_and_odd_headers = toggle_attr(e.attributes());
            }
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"w:themeFontLang" => {
                for a in e.attributes().flatten() {
                    let Ok(v) = a.unescape_value() else { continue };
                    let v = Some(v.into_owned()).filter(|v| !v.trim().is_empty());
                    match a.key.as_ref() {
                        b"w:val" => out.theme_font_lang.latin = v,
                        b"w:eastAsia" => out.theme_font_lang.east_asia = v,
                        b"w:bidi" => out.theme_font_lang.bidi = v,
                        _ => {}
                    }
                }
            }
            Event::Empty(e) | Event::Start(e) if e.name().as_ref() == b"w:clrSchemeMapping" => {
                for a in e.attributes().flatten() {
                    let key = a.key.as_ref();
                    if let Some(local) = key.strip_prefix(b"w:")
                        && let Ok(v) = a.unescape_value()
                    {
                        out.clr_scheme_mapping
                            .entries
                            .insert(String::from_utf8_lossy(local).into_owned(), v.into_owned());
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #355 — the two theme-selection settings Word writes.
    #[test]
    fn reads_theme_font_lang_and_colour_mapping() {
        let xml = concat!(
            r#"<w:settings xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
            r#"<w:themeFontLang w:val="en-US" w:eastAsia="" w:bidi="ar-SA"/>"#,
            r#"<w:clrSchemeMapping w:bg1="light1" w:t1="dark1" w:bg2="light2" w:t2="dark2" "#,
            r#"w:accent1="accent1" w:hyperlink="hyperlink"/></w:settings>"#,
        );
        let s = parse_settings_xml(xml.as_bytes()).expect("parse");
        assert_eq!(s.theme_font_lang.latin.as_deref(), Some("en-US"));
        assert_eq!(s.theme_font_lang.east_asia, None, "empty = absent");
        assert_eq!(s.theme_font_lang.bidi.as_deref(), Some("ar-SA"));
        assert_eq!(s.clr_scheme_mapping.entries["t1"], "dark1");
        assert_eq!(s.clr_scheme_mapping.entries["hyperlink"], "hyperlink");
        assert_eq!(s.clr_scheme_mapping.entries.len(), 6);
    }
}
