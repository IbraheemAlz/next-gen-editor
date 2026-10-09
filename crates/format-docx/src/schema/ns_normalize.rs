//! Issue #325 — prefix normalisation for parts whose root binds the
//! OOXML namespaces under non-canonical prefixes (`<d:document
//! xmlns:d="…/wordprocessingml/2006/main">`) or as the default namespace.
//!
//! The reader matches literal qnames (`w:p`, `r:id`, `a:blip`), so such a
//! part used to read as an EMPTY document without a word. Rather than
//! teach every parser a resolver, the part is rewritten once, up front,
//! into the canonical spelling: every element and attribute whose
//! namespace is in the family table (either family — the original URIs
//! are kept) is re-prefixed with its conventional prefix, and the root
//! gains the matching `xmlns:` declarations. Foreign namespaces
//! (`w14`, `mc`, …) and every non-markup byte (text, comments, PIs) are
//! copied verbatim.
//!
//! A part that needed this is **regenerate-only**: its bytes are no longer
//! the source's, so a zero-edit save re-emits the normalised part rather
//! than the original (the reader reports it as
//! `DocxWarning::NonCanonicalNamespaces`). Canonical parts never come
//! through here — they stay byte-identical.

use crate::error::DocxError;
use crate::schema::family::{self, RootBinding};
use crate::schema::grab_bag::NamespaceScope;
use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::reader::{NsReader, Reader};
use std::collections::{BTreeMap, BTreeSet};

const BOM: &[u8] = b"\xEF\xBB\xBF";

/// Classify the root element of `xml` (see
/// [`NamespaceScope::classify_root`]). A part that does not parse, or has
/// no root, reports `Canonical` so the ordinary parse path produces its
/// usual error.
pub fn inspect_root(xml: &[u8]) -> RootBinding {
    let xml = xml.strip_prefix(BOM).unwrap_or(xml);
    let mut reader = Reader::from_reader(xml);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                return NamespaceScope::from_root(&e).classify_root(e.name().as_ref());
            }
            Ok(Event::Eof) | Err(_) => return RootBinding::Canonical(Default::default()),
            _ => {}
        }
        buf.clear();
    }
}

fn utf8(b: &[u8]) -> Result<&str, DocxError> {
    Ok(std::str::from_utf8(b)?)
}

fn prefix_of(raw: &[u8]) -> Option<String> {
    let colon = raw.iter().position(|&b| b == b':')?;
    Some(String::from_utf8_lossy(&raw[..colon]).into_owned())
}

/// Working state of one normalisation pass.
#[derive(Default)]
struct Pass {
    /// Canonical prefix → source URI actually used by a rewritten name.
    used: BTreeMap<&'static str, String>,
    /// Prefixes of names left in a foreign / unbound namespace (and root
    /// declarations of table prefixes bound to foreign URIs).
    foreign: BTreeSet<String>,
    /// Canonical prefixes the ROOT already declares canonically.
    root_declared: BTreeSet<String>,
}

impl Pass {
    fn note(&mut self, prefix: &'static str, uri: &str) -> Result<(), DocxError> {
        match self.used.get(prefix) {
            Some(prev) if prev != uri => Err(DocxError::MalformedXml(format!(
                "namespace prefix `{prefix}` would need two URIs (`{prev}` and `{uri}`)"
            ))),
            Some(_) => Ok(()),
            None => {
                self.used.insert(prefix, uri.to_string());
                Ok(())
            }
        }
    }

    /// The (possibly re-prefixed) spelling of an element / attribute name.
    fn spell(
        &mut self,
        res: &ResolveResult<'_>,
        raw: &[u8],
        local: &[u8],
    ) -> Result<String, DocxError> {
        if let ResolveResult::Bound(ns) = res {
            let uri = utf8(ns.as_ref())?;
            if let Some(p) = family::canonical_prefix_for(uri) {
                self.note(p, uri)?;
                return Ok(format!("{p}:{}", utf8(local)?));
            }
        }
        if let Some(p) = prefix_of(raw) {
            self.foreign.insert(p);
        }
        Ok(String::from_utf8_lossy(raw).into_owned())
    }

    /// Rewrite one start tag; returns `(name, attributes-text)` where the
    /// attributes text starts with a space per attribute.
    fn start_tag(
        &mut self,
        reader: &NsReader<&[u8]>,
        res: &ResolveResult<'_>,
        e: &BytesStart<'_>,
        is_root: bool,
    ) -> Result<(String, String), DocxError> {
        let name = self.spell(res, e.name().as_ref(), e.local_name().as_ref())?;
        let mut attrs = String::new();
        for a in e.attributes() {
            let a = a?;
            let key = a.key.as_ref();
            let value = String::from_utf8_lossy(&a.value).replace('"', "&quot;");
            if key == b"xmlns" || key.starts_with(b"xmlns:") {
                let uri = utf8(&a.value)?;
                if let Some(canon) = family::canonical_prefix_for(uri) {
                    /* A table namespace declared under its OWN canonical
                    prefix is kept (root: byte-identical); under any
                    other prefix / as the default it is dropped — the
                    names using it were re-prefixed. */
                    if key.strip_prefix(b"xmlns:") == Some(canon.as_bytes()) {
                        if is_root {
                            self.root_declared.insert(canon.to_string());
                        }
                        attrs.push_str(&format!(" {}=\"{value}\"", utf8(key)?));
                    }
                } else {
                    if let Some(p) = key.strip_prefix(b"xmlns:")
                        && family::is_table_prefix(utf8(p)?)
                    {
                        self.foreign.insert(utf8(p)?.to_string());
                    }
                    attrs.push_str(&format!(" {}=\"{value}\"", utf8(key)?));
                }
                continue;
            }
            let (ares, local) = reader.resolve_attribute(a.key);
            let spelled = self.spell(&ares, key, local.as_ref())?;
            attrs.push_str(&format!(" {spelled}=\"{value}\""));
        }
        Ok((name, attrs))
    }

    fn end_tag(&mut self, res: &ResolveResult<'_>, e: &BytesEnd<'_>) -> Result<String, DocxError> {
        self.spell(res, e.name().as_ref(), e.local_name().as_ref())
    }
}

/// Rewrite `input` so every table-namespace name carries its canonical
/// prefix. See the module docs. Errors (typed, no panic) when the part is
/// malformed or when canonical prefixes cannot be claimed without a clash
/// with a foreign use of the same prefix.
pub fn canonicalize_prefixes(input: &[u8]) -> Result<Vec<u8>, DocxError> {
    let (bom, xml) = match input.strip_prefix(BOM) {
        Some(rest) => (BOM, rest),
        None => (&[][..], input),
    };
    let mut reader = NsReader::from_reader(xml);
    let mut pass = Pass::default();
    let mut buf = Vec::new();
    let mut prev = 0usize;
    let mut prolog_end: Option<usize> = None;
    let mut root_name = String::new();
    let mut root_attrs = String::new();
    let mut root_empty = false;
    let mut body: Vec<u8> = Vec::with_capacity(xml.len() + 64);
    loop {
        let ev = reader.read_event_into(&mut buf)?;
        let pos = reader.buffer_position() as usize;
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                let is_root = prolog_end.is_none();
                let (res, _) = reader.resolve_element(e.name());
                let (name, attrs) = pass.start_tag(&reader, &res, e, is_root)?;
                let empty = matches!(ev, Event::Empty(_));
                if is_root {
                    prolog_end = Some(prev);
                    root_name = name;
                    root_attrs = attrs;
                    root_empty = empty;
                } else {
                    body.extend_from_slice(
                        format!("<{name}{attrs}{}>", if empty { "/" } else { "" }).as_bytes(),
                    );
                }
            }
            Event::End(e) => {
                let (res, _) = reader.resolve_element(e.name());
                let name = pass.end_tag(&res, e)?;
                body.extend_from_slice(format!("</{name}>").as_bytes());
            }
            Event::Eof => break,
            _ => {
                if prolog_end.is_some() {
                    body.extend_from_slice(&xml[prev..pos]);
                }
            }
        }
        prev = pos;
        buf.clear();
    }
    let prolog_end = prolog_end
        .ok_or_else(|| DocxError::MalformedXml("part has no root element".to_string()))?;
    if let Some(clash) = pass
        .used
        .keys()
        .find(|p| pass.foreign.contains(**p))
    {
        return Err(DocxError::MalformedXml(format!(
            "prefix `{clash}` is used for both a schema namespace and a foreign one"
        )));
    }
    let mut out = Vec::with_capacity(body.len() + 256);
    out.extend_from_slice(bom);
    out.extend_from_slice(&xml[..prolog_end]);
    out.extend_from_slice(format!("<{root_name}").as_bytes());
    for (p, uri) in &pass.used {
        if !pass.root_declared.contains(*p) {
            out.extend_from_slice(format!(" xmlns:{p}=\"{uri}\"").as_bytes());
        }
    }
    out.extend_from_slice(root_attrs.as_bytes());
    out.extend_from_slice(if root_empty { b"/>" } else { b">" });
    out.extend_from_slice(&body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::NsFamily;
    use crate::schema::{NS_W, NS_W_STRICT};

    #[test]
    fn classifies_roots() {
        let canon = format!(r#"<w:document xmlns:w="{NS_W}"/>"#);
        assert_eq!(
            inspect_root(canon.as_bytes()),
            RootBinding::Canonical(NsFamily::Transitional)
        );
        let strict = format!(r#"<w:document xmlns:w="{NS_W_STRICT}"/>"#);
        assert_eq!(
            inspect_root(strict.as_bytes()),
            RootBinding::Canonical(NsFamily::Strict)
        );
        let other = format!(r#"<d:document xmlns:d="{NS_W}"/>"#);
        assert!(matches!(
            inspect_root(other.as_bytes()),
            RootBinding::NonCanonical { .. }
        ));
        let dflt = format!(r#"<document xmlns="{NS_W_STRICT}"/>"#);
        assert!(matches!(
            inspect_root(dflt.as_bytes()),
            RootBinding::NonCanonical { .. }
        ));
        let rebound = format!(r#"<w:document xmlns:w="{NS_W}" xmlns:r="urn:x"/>"#);
        assert!(matches!(
            inspect_root(rebound.as_bytes()),
            RootBinding::NonCanonical { .. }
        ));
        assert_eq!(
            inspect_root(b"<html xmlns=\"http://www.w3.org/1999/xhtml\"/>"),
            RootBinding::NotWordprocessingMl
        );
        /* A bare `w:` root (fragment / hand-written input) stays fast-path. */
        assert_eq!(
            inspect_root(b"<w:document><w:body/></w:document>"),
            RootBinding::Canonical(NsFamily::Transitional)
        );
    }

    #[test]
    fn rewrites_prefixed_and_default_namespaces() {
        let xml = format!(
            "\u{feff}<?xml version=\"1.0\"?>\n<d:document xmlns:d=\"{NS_W_STRICT}\" \
             xmlns:rel=\"http://purl.oclc.org/ooxml/officeDocument/relationships\" \
             xmlns:w14=\"urn:w14\"><d:body><d:p w14:paraId=\"1\"><d:r><d:t d:x=\"a&amp;b\">x &lt; y</d:t></d:r>\
             <d:hyperlink rel:id=\"rId1\"/><!-- keep --></d:p></d:body></d:document>"
        );
        let out = canonicalize_prefixes(xml.as_bytes()).expect("normalise");
        let out = String::from_utf8(out).expect("utf8");
        assert!(out.starts_with("\u{feff}<?xml version=\"1.0\"?>\n<w:document"));
        assert!(out.contains(&format!("xmlns:w=\"{NS_W_STRICT}\"")));
        assert!(
            out.contains("xmlns:r=\"http://purl.oclc.org/ooxml/officeDocument/relationships\"")
        );
        assert!(out.contains("xmlns:w14=\"urn:w14\""));
        assert!(!out.contains("xmlns:d="), "{out}");
        assert!(!out.contains("xmlns:rel="), "{out}");
        assert!(out.contains("<w:p w14:paraId=\"1\">"), "{out}");
        assert!(out.contains("<w:t w:x=\"a&amp;b\">x &lt; y</w:t>"), "{out}");
        assert!(out.contains("<w:hyperlink r:id=\"rId1\"/>"), "{out}");
        assert!(out.contains("<!-- keep -->"));
        assert_eq!(
            inspect_root(out.as_bytes()),
            RootBinding::Canonical(NsFamily::Strict)
        );

        let dflt = format!(r#"<document xmlns="{NS_W}"><body><p/></body></document>"#);
        let out = canonicalize_prefixes(dflt.as_bytes()).expect("normalise");
        let out = String::from_utf8(out).expect("utf8");
        assert_eq!(
            out,
            format!(r#"<w:document xmlns:w="{NS_W}"><w:body><w:p/></w:body></w:document>"#)
        );
    }

    #[test]
    fn canonical_prefix_clash_is_a_typed_error() {
        let xml =
            format!(r#"<d:document xmlns:d="{NS_W}" xmlns:w="urn:other"><w:foo/></d:document>"#);
        assert!(canonicalize_prefixes(xml.as_bytes()).is_err());
    }
}
