//! Issue #351 — Markup Compatibility and Extensibility (ECMA-376 Part 3):
//! `mc:AlternateContent` branch selection and `mc:Ignorable`.
//!
//! An `<mc:AlternateContent>` holds one or more `<mc:Choice Requires="…">`
//! branches and an optional `<mc:Fallback>`. A consumer takes the FIRST
//! choice whose every required namespace it understands, else the
//! fallback, and ignores the rest. Word writes them around text boxes
//! (`Requires="wps"` with a VML fallback), at run, paragraph and block
//! level. The reader used to understand them only inside a run (and there
//! always took the first `<w:drawing>`, never consulting `Requires`); at
//! paragraph and block level it walked every branch, so a table cell's
//! text came out once per branch.
//!
//! Now every walker selects with [`select_branch`] / [`choice_selectable`]
//! and keeps the branches it did not take as source bytes (the run-level
//! object, the paragraph's wrapper markers, the block envelope), so a
//! zero-edit save stays byte-identical and an edit keeps every branch.
//!
//! "Understood" is decided from the bytes of the `AlternateContent` element
//! alone — a prefix declared on the element or its choice is judged by its
//! namespace URI, any other by its conventional name — so the reader and
//! the writer's verified re-scan of the same bytes always agree.

use super::ct_rpr::attr_val;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

/// The Markup Compatibility namespace.
pub(crate) const NS_MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";

/// `(conventional prefix, namespace URI)` of every namespace whose content
/// the reader processes (or preserves faithfully while processing what it
/// models): WordprocessingML and DrawingML, VML, OMML, and Word's 2010+
/// extensions.
const UNDERSTOOD: &[(&str, &str)] = &[
    (
        "w",
        "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
    ),
    (
        "r",
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
    ),
    (
        "wp",
        "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing",
    ),
    ("a", "http://schemas.openxmlformats.org/drawingml/2006/main"),
    (
        "pic",
        "http://schemas.openxmlformats.org/drawingml/2006/picture",
    ),
    (
        "m",
        "http://schemas.openxmlformats.org/officeDocument/2006/math",
    ),
    ("mc", NS_MC),
    ("v", "urn:schemas-microsoft-com:vml"),
    ("o", "urn:schemas-microsoft-com:office:office"),
    ("w10", "urn:schemas-microsoft-com:office:word"),
    (
        "wps",
        "http://schemas.microsoft.com/office/word/2010/wordprocessingShape",
    ),
    (
        "wpg",
        "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup",
    ),
    (
        "wpc",
        "http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas",
    ),
    (
        "wpi",
        "http://schemas.microsoft.com/office/word/2010/wordprocessingInk",
    ),
    (
        "wp14",
        "http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing",
    ),
    (
        "a14",
        "http://schemas.microsoft.com/office/drawing/2010/main",
    ),
    (
        "w14",
        "http://schemas.microsoft.com/office/word/2010/wordml",
    ),
    (
        "w15",
        "http://schemas.microsoft.com/office/word/2012/wordml",
    ),
    (
        "w16se",
        "http://schemas.microsoft.com/office/word/2015/wordml/symex",
    ),
    (
        "w16cid",
        "http://schemas.microsoft.com/office/word/2016/wordml/cid",
    ),
    (
        "w16",
        "http://schemas.microsoft.com/office/word/2018/wordml",
    ),
    (
        "w16cex",
        "http://schemas.microsoft.com/office/word/2018/wordml/cex",
    ),
    (
        "w16sdtdh",
        "http://schemas.microsoft.com/office/word/2020/wordml/sdtdatahash",
    ),
    (
        "wne",
        "http://schemas.microsoft.com/office/word/2006/wordml",
    ),
];

/// Issue #435 — the namespace URI `prefix` conventionally names among the
/// namespaces the reader understands (`wps` → the WordprocessingShape
/// URI), `None` for any other prefix.
pub(crate) fn conventional_uri(prefix: &str) -> Option<&'static str> {
    UNDERSTOOD
        .iter()
        .find_map(|(p, u)| (*p == prefix).then_some(*u))
}

/// `true` when the reader understands namespace `uri`.
pub(crate) fn understands_uri(uri: &str) -> bool {
    UNDERSTOOD.iter().any(|(_, u)| *u == uri)
}

/// `true` when the reader understands prefix `prefix`: by URI when `local`
/// (the `xmlns:*` declarations in scope inside the fragment) binds it,
/// else by its conventional name.
fn understands_prefix(prefix: &str, local: &[(String, String)]) -> bool {
    match local.iter().rev().find(|(p, _)| p == prefix) {
        Some((_, uri)) => understands_uri(uri),
        None => UNDERSTOOD.iter().any(|(p, _)| *p == prefix),
    }
}

/// `xmlns:*` declarations of one start tag, appended to `out`.
fn push_decls(e: &BytesStart, out: &mut Vec<(String, String)>) {
    for a in e.attributes().flatten() {
        if let Some(prefix) = a.key.as_ref().strip_prefix(b"xmlns:") {
            out.push((
                String::from_utf8_lossy(prefix).into_owned(),
                String::from_utf8_lossy(&a.value).into_owned(),
            ));
        }
    }
}

/// `true` when every prefix a `Requires` value lists is understood. An
/// empty (or absent) value requires nothing.
pub(crate) fn requires_satisfied(requires: &str, local: &[(String, String)]) -> bool {
    requires
        .split_ascii_whitespace()
        .all(|p| understands_prefix(p, local))
}

/// A streaming walker's `<mc:Choice>` start tag `e` (its `<mc:AlternateContent>`
/// start tag `ac` supplies declarations too): may this choice be taken?
pub(crate) fn choice_selectable(ac: Option<&BytesStart>, e: &BytesStart) -> bool {
    let mut local = Vec::new();
    if let Some(ac) = ac {
        push_decls(ac, &mut local);
    }
    push_decls(e, &mut local);
    requires_satisfied(&attr_val(e, b"Requires").unwrap_or_default(), &local)
}

/// Issue #351 — the selected branch of a captured `<mc:AlternateContent>`
/// element (`fragment` starts with its start tag): the byte range of the
/// CONTENT of the first `<mc:Choice>` whose `Requires` is satisfied, else
/// of the `<mc:Fallback>`. `None` when nothing is selectable (no
/// satisfiable choice and no fallback, or a self-closing branch) or the
/// fragment is not an `AlternateContent`.
pub(crate) fn select_branch(fragment: &[u8]) -> Option<(usize, usize)> {
    let mut reader = Reader::from_reader(fragment);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut local: Vec<(String, String)> = Vec::new();
    let mut fallback: Option<(usize, usize)> = None;
    let mut depth = 0usize;
    loop {
        let ev = reader.read_event_into(&mut buf).ok()?;
        match ev {
            Event::Start(e) => {
                depth += 1;
                match (depth, e.name().as_ref()) {
                    (1, b"mc:AlternateContent") => push_decls(&e, &mut local),
                    (1, _) => return None,
                    (2, b"mc:Choice" | b"mc:Fallback") => {
                        let is_choice = e.name().as_ref() == b"mc:Choice";
                        let mut decls = local.clone();
                        push_decls(&e, &mut decls);
                        let take = is_choice
                            && requires_satisfied(
                                &attr_val(&e, b"Requires").unwrap_or_default(),
                                &decls,
                            );
                        let content_start = reader.buffer_position() as usize;
                        let end_tag = e.to_end().into_owned();
                        let mut skip = Vec::new();
                        reader.read_to_end_into(end_tag.name(), &mut skip).ok()?;
                        let after = reader.buffer_position() as usize;
                        /* The content ends where the end tag starts. */
                        let close_len = end_tag.name().as_ref().len() + 3;
                        let content_end = after.checked_sub(close_len)?;
                        let range = (content_start, content_end.max(content_start));
                        depth -= 1;
                        if take {
                            return Some(range);
                        }
                        if !is_choice && fallback.is_none() {
                            fallback = Some(range);
                        }
                    }
                    _ => {
                        /* Unknown child of the AlternateContent: skip. */
                        let end_tag = e.to_end().into_owned();
                        let mut skip = Vec::new();
                        reader.read_to_end_into(end_tag.name(), &mut skip).ok()?;
                        depth -= 1;
                    }
                }
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return fallback;
                }
            }
            Event::Eof => return fallback,
            _ => {}
        }
        buf.clear();
    }
}

/// [`select_branch`] as a slice of `fragment`.
pub(crate) fn selected_content(fragment: &[u8]) -> Option<&[u8]> {
    let (s, e) = select_branch(fragment)?;
    fragment.get(s..e)
}

/// The prefix of a qualified name (`w14` of `w14:foo`), if any.
pub(crate) fn prefix_of(qname: &[u8]) -> Option<&str> {
    let i = qname.iter().position(|&b| b == b':')?;
    std::str::from_utf8(&qname[..i]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AC_WPS: &str = concat!(
        r#"<mc:AlternateContent><mc:Choice Requires="wps"><w:drawing>CHOICE</w:drawing></mc:Choice>"#,
        r#"<mc:Fallback><w:pict>FALLBACK</w:pict></mc:Fallback></mc:AlternateContent>"#,
    );

    fn sel(xml: &str) -> Option<&str> {
        selected_content(xml.as_bytes()).map(|b| std::str::from_utf8(b).unwrap())
    }

    #[test]
    fn the_first_satisfiable_choice_wins() {
        assert_eq!(sel(AC_WPS), Some("<w:drawing>CHOICE</w:drawing>"));
        let two = concat!(
            r#"<mc:AlternateContent><mc:Choice Requires="w99">A</mc:Choice>"#,
            r#"<mc:Choice Requires="w14 wps">B</mc:Choice><mc:Fallback>C</mc:Fallback></mc:AlternateContent>"#,
        );
        assert_eq!(sel(two), Some("B"));
    }

    #[test]
    fn an_unknown_requirement_takes_the_fallback() {
        let xml = AC_WPS.replace("Requires=\"wps\"", "Requires=\"w99\"");
        assert_eq!(sel(&xml), Some("<w:pict>FALLBACK</w:pict>"));
        let xml = AC_WPS.replace("Requires=\"wps\"", "Requires=\"wps w99\"");
        assert_eq!(sel(&xml), Some("<w:pict>FALLBACK</w:pict>"));
        let none =
            r#"<mc:AlternateContent><mc:Choice Requires="w99">A</mc:Choice></mc:AlternateContent>"#;
        assert_eq!(sel(none), None);
    }

    #[test]
    fn a_locally_declared_prefix_is_judged_by_its_uri() {
        let ok = r#"<mc:AlternateContent xmlns:x="http://schemas.microsoft.com/office/word/2010/wordprocessingShape"><mc:Choice Requires="x">A</mc:Choice><mc:Fallback>B</mc:Fallback></mc:AlternateContent>"#;
        assert_eq!(sel(ok), Some("A"));
        let shadowed = r#"<mc:AlternateContent><mc:Choice Requires="wps" xmlns:wps="urn:example:not-wps">A</mc:Choice><mc:Fallback>B</mc:Fallback></mc:AlternateContent>"#;
        assert_eq!(sel(shadowed), Some("B"));
    }

    #[test]
    fn not_an_alternate_content() {
        assert_eq!(sel("<w:drawing/>"), None);
        assert_eq!(sel("<mc:AlternateContent><broken"), None);
    }
}
