//! Issue #353 — relationship-driven part discovery.
//!
//! The reader used to hard-code `word/document.xml` and its siblings
//! (`word/styles.xml`, `word/settings.xml`, …). OPC (ECMA-376 Part 2 §9)
//! says otherwise: the main part is whatever the package's `_rels/.rels`
//! `officeDocument` relationship targets, and the main part's siblings are
//! whatever *its* relationships (`word/_rels/document.xml.rels`) target by
//! `Type`. Producers do rename the main part (`word/document2.xml`,
//! `word/main.xml`), percent-encode targets (`my%20styles.xml`), write
//! `../`-relative or `/`-absolute ones, and spell the rel types in the
//! Strict family (#325) — all of which a fixed-name reader silently
//! ignores.
//!
//! [`PartNames::discover`] resolves every name once; `DocxArchive` carries
//! the result and the writer uses it everywhere it used to use a constant.
//! The fixed names remain the **fallback** (no `_rels/.rels`, no matching
//! relationship, or the target part is not in the archive), so a package
//! that already worked keeps working byte-for-byte.

use crate::error::{DocxError, DocxWarning};
use crate::opc::archive::{
    COMMENTS_EXTENDED_XML, COMMENTS_EXTENSIBLE_XML, COMMENTS_IDS_XML, COMMENTS_XML, CORE_PROPS_XML,
    DOC_XML, ENDNOTES_XML, FOOTNOTES_XML, NUMBERING_XML, SETTINGS_XML, STYLES_XML,
};
use crate::opc::relationships::{TargetMode, parse_relationships};

const REL_BASE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
const REL_CORE_PROPS: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";
const REL_COMMENTS_EXTENDED: &str =
    "http://schemas.microsoft.com/office/2011/relationships/commentsExtended";
/// Issue #282 — Word 2016+ comment side parts (rows keyed by paraId /
/// durable id; a deleted comment's rows are removed from them).
const REL_COMMENTS_IDS: &str =
    "http://schemas.microsoft.com/office/2016/09/relationships/commentsIds";
const REL_COMMENTS_EXTENSIBLE: &str =
    "http://schemas.microsoft.com/office/2018/08/relationships/commentsExtensible";

/// The archive entry names of the main part and the siblings the reader
/// and writer treat specially. Every field is an archive entry name (no
/// leading slash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartNames {
    pub main: String,
    pub main_rels: String,
    pub styles: String,
    pub numbering: String,
    pub settings: String,
    pub footnotes: String,
    pub endnotes: String,
    pub comments: String,
    pub comments_extended: String,
    /// Issue #282 — `word/commentsIds.xml` / `word/commentsExtensible.xml`.
    pub comments_ids: String,
    pub comments_extensible: String,
    pub core_props: String,
}

impl Default for PartNames {
    fn default() -> Self {
        Self::for_main(DOC_XML)
    }
}

/// `word/document.xml` → `word/_rels/document.xml.rels`.
pub fn rels_entry_name(part: &str) -> String {
    match part.rsplit_once('/') {
        Some((dir, base)) => format!("{dir}/_rels/{base}.rels"),
        None => format!("_rels/{part}.rels"),
    }
}

fn dir_of(part: &str) -> &str {
    part.rsplit_once('/').map_or("", |(d, _)| d)
}

impl PartNames {
    /// The fixed-name layout around `main`: siblings sit next to the main
    /// part (for `word/document.xml` these are the historical constants).
    pub fn for_main(main: &str) -> Self {
        let dir = dir_of(main);
        let next_to_main = |base: &str, default: &str| {
            if dir == "word" {
                default.to_string()
            } else if dir.is_empty() {
                base.to_string()
            } else {
                format!("{dir}/{base}")
            }
        };
        Self {
            main: main.to_string(),
            main_rels: rels_entry_name(main),
            styles: next_to_main("styles.xml", STYLES_XML),
            numbering: next_to_main("numbering.xml", NUMBERING_XML),
            settings: next_to_main("settings.xml", SETTINGS_XML),
            footnotes: next_to_main("footnotes.xml", FOOTNOTES_XML),
            endnotes: next_to_main("endnotes.xml", ENDNOTES_XML),
            comments: next_to_main("comments.xml", COMMENTS_XML),
            comments_extended: next_to_main("commentsExtended.xml", COMMENTS_EXTENDED_XML),
            comments_ids: next_to_main("commentsIds.xml", COMMENTS_IDS_XML),
            comments_extensible: next_to_main("commentsExtensible.xml", COMMENTS_EXTENSIBLE_XML),
            core_props: CORE_PROPS_XML.to_string(),
        }
    }

    /// Resolve the package's part names from its relationships.
    ///
    /// `entries` is every archive entry (the main part itself may be
    /// absent from it — `DocumentTree::source_package` strips it);
    /// `main_present` says whether a candidate main-part name exists in
    /// the archive. Errors: a main-part target that escapes the package
    /// ([`DocxError::UnsafePartName`]) or names no part at all while the
    /// fixed fallback is missing too ([`DocxError::MissingEntry`]).
    /// Non-fatal findings (a fallback taken, an unsafe sibling target)
    /// are appended to `warnings`.
    pub fn discover(
        entries: &[(String, Vec<u8>)],
        main_present: &dyn Fn(&str) -> bool,
        warnings: &mut Vec<DocxWarning>,
    ) -> Result<Self, DocxError> {
        let get = |name: &str| {
            entries
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, b)| b.as_slice())
        };
        let exists = |name: &str| get(name).is_some();

        /* 1. The main part: `_rels/.rels` → officeDocument. */
        let root_rels = get("_rels/.rels").and_then(|b| parse_relationships(b).ok());
        let mut main = DOC_XML.to_string();
        if let Some(rels) = &root_rels
            && let Some(rel) = rels
                .by_type(&format!("{REL_BASE}officeDocument"))
                .find(|r| r.target_mode == TargetMode::Internal)
        {
            let candidates = target_candidates("", &rel.target)?;
            match candidates.iter().find(|c| main_present(c)) {
                Some(found) => main = found.clone(),
                None if main_present(DOC_XML) => {
                    warnings.push(DocxWarning::MainPartFallback {
                        target: rel.target.clone(),
                    });
                }
                None => {
                    return Err(DocxError::MissingEntry(
                        candidates.into_iter().next().unwrap_or_default(),
                    ));
                }
            }
        }
        let mut names = Self::for_main(&main);

        /* 2. Siblings by relationship Type, from the main part's rels. */
        if let Some(rels) = get(&names.main_rels).and_then(|b| parse_relationships(b).ok()) {
            let mut pick = |rel_type: &str, slot: &mut String| {
                for rel in rels
                    .by_type(rel_type)
                    .filter(|r| r.target_mode == TargetMode::Internal)
                {
                    match target_candidates(&main, &rel.target) {
                        Ok(cs) => {
                            if let Some(found) = cs.into_iter().find(|c| exists(c)) {
                                *slot = found;
                                return;
                            }
                        }
                        Err(_) => warnings.push(DocxWarning::UnsafeRelationshipTarget {
                            target: rel.target.clone(),
                        }),
                    }
                }
            };
            pick(&format!("{REL_BASE}styles"), &mut names.styles);
            pick(&format!("{REL_BASE}numbering"), &mut names.numbering);
            pick(&format!("{REL_BASE}settings"), &mut names.settings);
            pick(&format!("{REL_BASE}footnotes"), &mut names.footnotes);
            pick(&format!("{REL_BASE}endnotes"), &mut names.endnotes);
            pick(&format!("{REL_BASE}comments"), &mut names.comments);
            pick(REL_COMMENTS_EXTENDED, &mut names.comments_extended);
            pick(REL_COMMENTS_IDS, &mut names.comments_ids);
            pick(REL_COMMENTS_EXTENSIBLE, &mut names.comments_extensible);
        }
        /* 3. Core properties hang off the package root, not the main part. */
        if let Some(rels) = &root_rels {
            for rel in rels
                .by_type(REL_CORE_PROPS)
                .filter(|r| r.target_mode == TargetMode::Internal)
            {
                if let Ok(cs) = target_candidates("", &rel.target)
                    && let Some(found) = cs.into_iter().find(|c| exists(c))
                {
                    names.core_props = found;
                    break;
                }
            }
        }
        Ok(names)
    }
}

/// `%XX` escapes decoded; an invalid escape or a result that is not UTF-8
/// leaves the input as it was.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// Resolve a relationship `target` against `source_part` (the part whose
/// rels file holds it; `""` for the package root) per OPC §9.3. `.` and
/// empty segments vanish, `..` pops, a leading `/` is package-absolute,
/// `\` counts as `/` (producers write it). A `..` that would climb above
/// the package root is an error — a hostile `Target="../../x"` must not be
/// silently clamped into some other part's name.
pub fn resolve_part_target(source_part: &str, target: &str) -> Result<String, DocxError> {
    let target = target.replace('\\', "/");
    let target = target.split('#').next().unwrap_or("");
    let mut segs: Vec<&str> = Vec::new();
    if !target.starts_with('/') {
        segs.extend(dir_of(source_part).split('/').filter(|s| !s.is_empty()));
    }
    for s in target.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                if segs.pop().is_none() {
                    return Err(DocxError::UnsafePartName(target.to_string()));
                }
            }
            s if s.contains('\0') => return Err(DocxError::UnsafePartName(target.to_string())),
            s => segs.push(s),
        }
    }
    Ok(segs.join("/"))
}

/// The archive entry names a `target` may denote, best first: the
/// percent-decoded resolution, then (when different) the literal one.
pub fn target_candidates(source_part: &str, target: &str) -> Result<Vec<String>, DocxError> {
    let decoded = resolve_part_target(source_part, &percent_decode(target))?;
    let literal = resolve_part_target(source_part, target)?;
    let mut out = vec![decoded];
    if out[0] != literal {
        out.push(literal);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rels(rows: &[(&str, &str)]) -> Vec<u8> {
        let mut s = String::from(
            "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
        );
        for (i, (ty, target)) in rows.iter().enumerate() {
            s.push_str(&format!(
                "<Relationship Id=\"rId{i}\" Type=\"{ty}\" Target=\"{target}\"/>"
            ));
        }
        s.push_str("</Relationships>");
        s.into_bytes()
    }

    fn rt(name: &str) -> String {
        format!("{REL_BASE}{name}")
    }

    #[test]
    fn target_resolution_normalises() {
        let r = |src: &str, t: &str| resolve_part_target(src, t).unwrap();
        assert_eq!(r("word/document.xml", "styles.xml"), "word/styles.xml");
        assert_eq!(
            r("word/document.xml", "./a/../styles.xml"),
            "word/styles.xml"
        );
        assert_eq!(
            r("word/document.xml", "../media/image1.png"),
            "media/image1.png"
        );
        assert_eq!(
            r("word/document.xml", "/word/styles.xml"),
            "word/styles.xml"
        );
        assert_eq!(r("", "word\\document.xml"), "word/document.xml");
        assert_eq!(r("", "word/document.xml#frag"), "word/document.xml");
        assert!(matches!(
            resolve_part_target("word/document.xml", "../../x.xml"),
            Err(DocxError::UnsafePartName(_))
        ));
        assert!(matches!(
            resolve_part_target("", "../x.xml"),
            Err(DocxError::UnsafePartName(_))
        ));
        assert_eq!(
            target_candidates("word/document.xml", "my%20styles.xml").unwrap(),
            vec!["word/my styles.xml", "word/my%20styles.xml"]
        );
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn renamed_main_and_siblings_are_found_by_type_in_both_families() {
        for strict in [false, true] {
            let ty = |n: &str| {
                let t = rt(n);
                if strict {
                    crate::schema::family::to_family(&t, crate::schema::NsFamily::Strict)
                        .into_owned()
                } else {
                    t
                }
            };
            let entries = vec![
                (
                    "_rels/.rels".to_string(),
                    rels(&[(&ty("officeDocument"), "/office/main%20doc.xml")]),
                ),
                ("office/main doc.xml".to_string(), b"<w:document/>".to_vec()),
                (
                    "office/_rels/main doc.xml.rels".to_string(),
                    rels(&[
                        (&ty("styles"), "../styles/s%201.xml"),
                        (&ty("settings"), "settings.xml"),
                        (REL_COMMENTS_EXTENDED, "cx.xml"),
                    ]),
                ),
                ("styles/s 1.xml".to_string(), b"<s/>".to_vec()),
                ("office/settings.xml".to_string(), b"<s/>".to_vec()),
                ("office/cx.xml".to_string(), b"<s/>".to_vec()),
            ];
            let mut warnings = Vec::new();
            let names = PartNames::discover(
                &entries,
                &|n| entries.iter().any(|(e, _)| e == n),
                &mut warnings,
            )
            .unwrap();
            assert!(warnings.is_empty(), "{warnings:?}");
            assert_eq!(names.main, "office/main doc.xml");
            assert_eq!(names.main_rels, "office/_rels/main doc.xml.rels");
            assert_eq!(names.styles, "styles/s 1.xml");
            assert_eq!(names.settings, "office/settings.xml");
            assert_eq!(names.comments_extended, "office/cx.xml");
            /* Unreferenced siblings fall back next to the main part. */
            assert_eq!(names.numbering, "office/numbering.xml");
        }
    }

    #[test]
    fn fixed_names_are_the_fallback() {
        let entries = vec![("word/document.xml".to_string(), b"<w:document/>".to_vec())];
        let mut w = Vec::new();
        let names = PartNames::discover(&entries, &|n| entries.iter().any(|(e, _)| e == n), &mut w)
            .unwrap();
        assert_eq!(names, PartNames::default());
        assert!(w.is_empty());

        /* A root relationship naming a missing part falls back, loudly. */
        let entries = vec![
            (
                "_rels/.rels".to_string(),
                rels(&[(&rt("officeDocument"), "word/gone.xml")]),
            ),
            ("word/document.xml".to_string(), b"<w:document/>".to_vec()),
        ];
        let mut w = Vec::new();
        let names = PartNames::discover(&entries, &|n| entries.iter().any(|(e, _)| e == n), &mut w)
            .unwrap();
        assert_eq!(names.main, DOC_XML);
        assert_eq!(
            w,
            vec![DocxWarning::MainPartFallback {
                target: "word/gone.xml".into()
            }]
        );
    }

    #[test]
    fn traversal_in_the_main_target_is_a_typed_error() {
        let entries = vec![(
            "_rels/.rels".to_string(),
            rels(&[(&rt("officeDocument"), "../../etc/x.xml")]),
        )];
        let mut w = Vec::new();
        let err = PartNames::discover(&entries, &|_| true, &mut w).unwrap_err();
        assert!(matches!(err, DocxError::UnsafePartName(_)), "{err}");
    }

    #[test]
    fn unsafe_sibling_target_is_a_warning() {
        let entries = vec![
            (
                "word/_rels/document.xml.rels".to_string(),
                rels(&[(&rt("styles"), "../../styles.xml")]),
            ),
            ("word/styles.xml".to_string(), b"<s/>".to_vec()),
        ];
        let mut w = Vec::new();
        let names = PartNames::discover(&entries, &|_| true, &mut w).unwrap();
        assert_eq!(names.styles, STYLES_XML, "falls back to the fixed name");
        assert_eq!(
            w,
            vec![DocxWarning::UnsafeRelationshipTarget {
                target: "../../styles.xml".into()
            }]
        );
    }
}
