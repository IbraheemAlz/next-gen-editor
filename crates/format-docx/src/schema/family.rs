//! Issue #325 — ISO/IEC 29500 namespace families.
//!
//! An OOXML document is either *Transitional* (ECMA-376 1st–4th ed., what
//! Word saves by default) or *Strict* (Word's "Strict Open XML Document"):
//! same parts, same element names, same prefixes in practice — but every
//! schema namespace URI, and the base of every relationship type, changes.
//! Content types, the OPC package namespaces, core properties and MCE are
//! identical in both families.
//!
//! The reader matches literal qnames (`w:p`), so it never needs the URI to
//! find an element; the family matters at three places only: validating a
//! part root's bindings, matching relationship types, and *minting* — a
//! saved Strict document must not become a Transitional / Strict hybrid.

use super::NS_W;
use std::borrow::Cow;

/// Which ISO 29500 spelling a package uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NsFamily {
    #[default]
    Transitional,
    Strict,
}

/// Strict WordprocessingML namespace.
pub const NS_W_STRICT: &str = "http://purl.oclc.org/ooxml/wordprocessingml/main";

/// One URI that differs between the families, with the prefix every
/// producer binds it to (the reader's fast path matches on that prefix).
struct FamilyUri {
    prefix: &'static str,
    transitional: &'static str,
    strict: &'static str,
}

/// The namespace-family table. A relationship-type base
/// (`…/relationships`) is also a *prefix* of every concrete rel type, so
/// [`to_family`] treats entries as substrings, not just exact URIs.
const FAMILY_URIS: &[FamilyUri] = &[
    FamilyUri {
        prefix: "w",
        transitional: NS_W,
        strict: NS_W_STRICT,
    },
    FamilyUri {
        prefix: "r",
        transitional: "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
        strict: "http://purl.oclc.org/ooxml/officeDocument/relationships",
    },
    FamilyUri {
        prefix: "a",
        transitional: "http://schemas.openxmlformats.org/drawingml/2006/main",
        strict: "http://purl.oclc.org/ooxml/drawingml/main",
    },
    FamilyUri {
        prefix: "wp",
        transitional: "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing",
        strict: "http://purl.oclc.org/ooxml/drawingml/wordprocessingDrawing",
    },
    FamilyUri {
        prefix: "pic",
        transitional: "http://schemas.openxmlformats.org/drawingml/2006/picture",
        strict: "http://purl.oclc.org/ooxml/drawingml/picture",
    },
    FamilyUri {
        prefix: "c",
        transitional: "http://schemas.openxmlformats.org/drawingml/2006/chart",
        strict: "http://purl.oclc.org/ooxml/drawingml/chart",
    },
    FamilyUri {
        prefix: "m",
        transitional: "http://schemas.openxmlformats.org/officeDocument/2006/math",
        strict: "http://purl.oclc.org/ooxml/officeDocument/math",
    },
    FamilyUri {
        prefix: "s",
        transitional: "http://schemas.openxmlformats.org/officeDocument/2006/sharedTypes",
        strict: "http://purl.oclc.org/ooxml/officeDocument/sharedTypes",
    },
    FamilyUri {
        prefix: "cx",
        transitional: "http://schemas.openxmlformats.org/officeDocument/2006/customXml",
        strict: "http://purl.oclc.org/ooxml/officeDocument/customXml",
    },
    FamilyUri {
        prefix: "vt",
        transitional: "http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes",
        strict: "http://purl.oclc.org/ooxml/officeDocument/docPropsVTypes",
    },
    FamilyUri {
        prefix: "ep",
        transitional: "http://schemas.openxmlformats.org/officeDocument/2006/extended-properties",
        strict: "http://purl.oclc.org/ooxml/officeDocument/extendedProperties",
    },
    FamilyUri {
        prefix: "cp",
        transitional: "http://schemas.openxmlformats.org/officeDocument/2006/custom-properties",
        strict: "http://purl.oclc.org/ooxml/officeDocument/customProperties",
    },
];

/// Prefixes the reader's fast path (and the normaliser) own: when a root
/// binds one of these, it must bind it to a URI of the matching entry.
pub const FAST_PATH_PREFIXES: [&str; 5] = ["w", "r", "a", "wp", "pic"];

/// Family of a recognised schema URI (`None` for any other namespace).
pub fn family_of_uri(uri: &str) -> Option<NsFamily> {
    FAMILY_URIS.iter().find_map(|f| {
        if f.transitional == uri {
            Some(NsFamily::Transitional)
        } else if f.strict == uri {
            Some(NsFamily::Strict)
        } else {
            None
        }
    })
}

/// The conventional prefix of a recognised schema URI, either family.
pub fn canonical_prefix_for(uri: &str) -> Option<&'static str> {
    FAMILY_URIS
        .iter()
        .find(|f| f.transitional == uri || f.strict == uri)
        .map(|f| f.prefix)
}

/// `true` when `prefix` is one the fast path (and the normaliser) owns.
pub fn is_fast_path_prefix(prefix: &str) -> bool {
    FAST_PATH_PREFIXES.contains(&prefix)
}

/// `true` when `prefix` is the conventional prefix of any table entry.
pub fn is_table_prefix(prefix: &str) -> bool {
    FAMILY_URIS.iter().any(|f| f.prefix == prefix)
}

/// Does `uri` belong to the table entry that conventionally uses `prefix`?
pub fn uri_fits_prefix(prefix: &str, uri: &str) -> bool {
    FAMILY_URIS
        .iter()
        .any(|f| f.prefix == prefix && (f.transitional == uri || f.strict == uri))
}

/// Rewrite every recognised URI / relationship-type base in `text` to its
/// spelling in `family`. Idempotent; `Cow::Borrowed` when nothing changes.
/// Pure text substitution — callers use it on strings the *writer*
/// mints (constants, rel rows), never on source bytes.
pub fn to_family(text: &str, family: NsFamily) -> Cow<'_, str> {
    let mut out: Cow<'_, str> = Cow::Borrowed(text);
    for f in FAMILY_URIS {
        let (from, to) = match family {
            NsFamily::Strict => (f.transitional, f.strict),
            NsFamily::Transitional => (f.strict, f.transitional),
        };
        if out.contains(from) {
            out = Cow::Owned(out.replace(from, to));
        }
    }
    out
}

/// Issue #435 — the URI of the table entry conventionally bound to
/// `prefix`, in `family`'s spelling (`r` → the relationships URI); `None`
/// for a prefix the table does not own.
pub fn uri_for_prefix(prefix: &str, family: NsFamily) -> Option<&'static str> {
    FAMILY_URIS
        .iter()
        .find(|f| f.prefix == prefix)
        .map(|f| match family {
            NsFamily::Strict => f.strict,
            NsFamily::Transitional => f.transitional,
        })
}

/// The exact schema URI `uri` (either spelling) in `family`'s spelling;
/// unchanged when it is not a table entry. `'static` in, `'static` out.
pub fn uri_in(uri: &'static str, family: NsFamily) -> &'static str {
    FAMILY_URIS
        .iter()
        .find(|f| f.transitional == uri || f.strict == uri)
        .map_or(uri, |f| match family {
            NsFamily::Strict => f.strict,
            NsFamily::Transitional => f.transitional,
        })
}

/// [`to_family`] restricted to the first element's start tag of an XML
/// part — what a *minted* part root needs. The body is left alone, so
/// user text that happens to spell a schema URI is never rewritten.
pub fn root_tag_to_family(part: Vec<u8>, family: NsFamily) -> Vec<u8> {
    if family == NsFamily::Transitional {
        return part;
    }
    let mut i = 0;
    let root_start = loop {
        match part[i..].iter().position(|&b| b == b'<') {
            None => return part,
            Some(at) => {
                let lt = i + at;
                if matches!(part.get(lt + 1), Some(b'?') | Some(b'!')) {
                    i = lt + 1;
                } else {
                    break lt;
                }
            }
        }
    };
    let Some(close) = part[root_start..].iter().position(|&b| b == b'>') else {
        return part;
    };
    let end = root_start + close + 1;
    let Ok(tag) = std::str::from_utf8(&part[root_start..end]) else {
        return part;
    };
    match to_family(tag, family) {
        Cow::Borrowed(_) => part,
        Cow::Owned(new_tag) => {
            let mut out = Vec::with_capacity(part.len() + 64);
            out.extend_from_slice(&part[..root_start]);
            out.extend_from_slice(new_tag.as_bytes());
            out.extend_from_slice(&part[end..]);
            out
        }
    }
}

/// A relationship `Type` / namespace URI in its Transitional spelling —
/// the canonical form every comparison goes through.
pub fn canonical(uri: &str) -> Cow<'_, str> {
    to_family(uri, NsFamily::Transitional)
}

/// Family-insensitive equality of two relationship types / namespace URIs.
pub fn same_uri(a: &str, b: &str) -> bool {
    a == b || canonical(a) == canonical(b)
}

/// Family of a package, from the URI its `w` prefix is bound to on the
/// main part's root (`None` when `w` is unbound or foreign).
pub fn family_of_w_uri(uri: &str) -> Option<NsFamily> {
    match uri {
        NS_W => Some(NsFamily::Transitional),
        NS_W_STRICT => Some(NsFamily::Strict),
        _ => None,
    }
}

/// Verdict of [`crate::schema::grab_bag::NamespaceScope::classify_root`]
/// on a part root's namespace bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootBinding {
    /// Root is `w:*` with `w` bound to one family's WordprocessingML URI
    /// (or, leniently, unbound) and every other fast-path prefix bound to
    /// its own family entry: literal-qname matching is sound.
    Canonical(NsFamily),
    /// WordprocessingML is bound, but under a prefix / default namespace
    /// the literal-qname reader would not match (or a canonical prefix is
    /// rebound to a foreign URI). `detail` names the first offender.
    NonCanonical { detail: String },
    /// No WordprocessingML binding the root element could belong to.
    NotWordprocessingMl,
}

/// Family of a part root's start tag (raw bytes, e.g. a captured
/// `DocumentEnvelope::root_tag`) or attribute list: the URI of its
/// `xmlns:w` binding. Transitional when absent or foreign.
pub fn family_of_root_tag(root_tag: &[u8]) -> NsFamily {
    let text = String::from_utf8_lossy(root_tag);
    let mut from = 0;
    while let Some(at) = text[from..].find("xmlns:w=") {
        let idx = from + at;
        let rest = &text[idx + 8..];
        from = idx + 8;
        if idx > 0 && !text.as_bytes()[idx - 1].is_ascii_whitespace() {
            continue;
        }
        let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        let inner = &rest[1..];
        if let Some(end) = inner.find(quote)
            && let Some(f) = family_of_w_uri(&inner[..end])
        {
            return f;
        }
    }
    NsFamily::Transitional
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{NS_CT, NS_R};

    #[test]
    fn transcodes_both_ways_and_is_idempotent() {
        let t = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
        let s = to_family(t, NsFamily::Strict);
        assert_eq!(
            s,
            "http://purl.oclc.org/ooxml/officeDocument/relationships/image"
        );
        assert_eq!(to_family(&s, NsFamily::Strict), s);
        assert_eq!(to_family(&s, NsFamily::Transitional), t);
        assert!(same_uri(t, &s));
        assert!(!same_uri(t, "http://example.com/x"));
    }

    #[test]
    fn mce_and_package_namespaces_are_family_neutral() {
        for u in [
            NS_R,
            NS_CT,
            "http://schemas.openxmlformats.org/markup-compatibility/2006",
        ] {
            assert_eq!(to_family(u, NsFamily::Strict), u);
            assert_eq!(family_of_uri(u), None);
        }
    }

    #[test]
    fn table_covers_every_documented_pair() {
        for (t, s) in [
            ("drawingml/2006/chart", "drawingml/chart"),
            ("officeDocument/2006/math", "officeDocument/math"),
            ("officeDocument/2006/customXml", "officeDocument/customXml"),
            (
                "officeDocument/2006/docPropsVTypes",
                "officeDocument/docPropsVTypes",
            ),
            (
                "officeDocument/2006/sharedTypes",
                "officeDocument/sharedTypes",
            ),
            (
                "officeDocument/2006/extended-properties",
                "officeDocument/extendedProperties",
            ),
            (
                "officeDocument/2006/custom-properties",
                "officeDocument/customProperties",
            ),
        ] {
            let tu = format!("http://schemas.openxmlformats.org/{t}");
            let su = format!("http://purl.oclc.org/ooxml/{s}");
            assert_eq!(to_family(&tu, NsFamily::Strict), su);
            assert_eq!(family_of_uri(&su), Some(NsFamily::Strict));
        }
    }

    #[test]
    fn root_tag_family() {
        let strict = format!(r#"<w:document xmlns:r="x" xmlns:w="{NS_W_STRICT}">"#);
        assert_eq!(family_of_root_tag(strict.as_bytes()), NsFamily::Strict);
        let trans = format!(r#"<w:document xmlns:w="{NS_W}">"#);
        assert_eq!(family_of_root_tag(trans.as_bytes()), NsFamily::Transitional);
        assert_eq!(family_of_root_tag(b"<w:document>"), NsFamily::Transitional);
    }
}
