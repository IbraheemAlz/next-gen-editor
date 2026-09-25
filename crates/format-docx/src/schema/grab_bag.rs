//! Issue #84 — in-part OOXML grab bags, read-side capture helpers.
//!
//! A dirty paragraph / table regenerates from the typed engine model, so
//! every `<w:rPr>` / `<w:pPr>` / `<w:tblPr>` / `<w:trPr>` / `<w:tcPr>`
//! child the reader does not model used to vanish on the first edit. The
//! part parsers now route each unmodeled child through this module:
//!
//! 1. the raw bytes of the whole child element are sliced out of the
//!    source part (`slice_fragment` for an empty tag, [`capture_subtree`]
//!    for a container — the subtree is consumed so nested `<w:rPr>` /
//!    `<w:pPr>` inside a `*Change` history element can no longer leak
//!    into the live properties);
//! 2. the fragment's namespace prefixes are checked against the part's
//!    root bindings ([`bound_by_root`]): the writer synthesizes its own
//!    root element but re-declares everything the source root bound
//!    (`DocxArchive::document_root_attrs`), so `w:` fragments and
//!    root-bound foreign ones (`w14:`, `mc:`, …) are preserved
//!    byte-for-byte. A prefix bound only on some intermediate ancestor
//!    cannot be re-bound; that fragment is dropped rather than written
//!    unbound (a namespace error would make Word refuse the whole part);
//! 3. the fragment lands in the owning struct's `grab_bag` slot via
//!    [`stash`], document order preserved.
//!
//! The writer (`crate::writer`) re-emits every fragment verbatim,
//! interleaved with the modeled children by the schema ranks the `ct_*`
//! siblings publish.

use crate::error::DocxError;
use engine::GrabBag;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::BTreeSet;

/// Namespace declarations bound on a part's root element (`<w:document>`,
/// `<w:hdr>`, …): `(prefix, escaped-uri)` in document order. Everything a
/// captured fragment may need to re-bind comes from here.
#[derive(Debug, Clone, Default)]
pub struct NamespaceScope {
    decls: Vec<(String, String)>,
}

impl NamespaceScope {
    /// Collect every `xmlns:PREFIX="…"` attribute of `root`. Attribute
    /// values are kept in their escaped source form — they are pasted back
    /// verbatim into an attribute position.
    pub fn from_root(root: &BytesStart) -> Self {
        let mut decls = Vec::new();
        for a in root.attributes().flatten() {
            if let Some(prefix) = a.key.as_ref().strip_prefix(b"xmlns:") {
                decls.push((
                    String::from_utf8_lossy(prefix).into_owned(),
                    String::from_utf8_lossy(&a.value).into_owned(),
                ));
            }
        }
        Self { decls }
    }

    /// URI bound to `prefix` on the root, if any.
    pub fn uri(&self, prefix: &str) -> Option<&str> {
        self.decls
            .iter()
            .find(|(p, _)| p == prefix)
            .map(|(_, u)| u.as_str())
    }

    /// Every `(prefix, escaped-uri)` binding, document order — what a
    /// synthesized wrapper root re-declares so a fragment parsed out of
    /// context sees the same scope (issue #101, `parts::table`).
    pub fn declarations(&self) -> impl Iterator<Item = (&str, &str)> {
        self.decls.iter().map(|(p, u)| (p.as_str(), u.as_str()))
    }
}

/// `xml[start..end]` when the range is sane, else `None`. `start` is the
/// byte offset of the element's `<`; `end` the reader position just past
/// its closing `>`.
pub fn slice_fragment(xml: &[u8], start: usize, end: usize) -> Option<Vec<u8>> {
    (start < end && end <= xml.len()).then(|| xml[start..end].to_vec())
}

/// [`slice_fragment`] for a passthrough capture of the element `qname`
/// (`b"w:p"`, `b"w:tbl"`): additionally requires the slice to start with
/// that element's start tag and end on a `>`. A capture that fails the
/// shape check returns `None`, so the writer regenerates the block from
/// the typed model instead of splicing a misaligned byte range into the
/// saved part — the failure mode of issue #110 (a reader offset that was
/// three bytes off spliced `</w<w:sectPr/>` into `document.xml`). The
/// zero-drift roundtrip fixtures turn that fallback into a visible
/// failure; it must never be a silent one in production.
pub fn slice_element(xml: &[u8], start: usize, end: usize, qname: &[u8]) -> Option<Vec<u8>> {
    let slice = xml.get(start..end)?;
    let body = slice.strip_prefix(b"<")?.strip_prefix(qname)?;
    let opens_element = matches!(
        body.first(),
        Some(b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n')
    );
    (opens_element && slice.ends_with(b">")).then(|| slice.to_vec())
}

/// Consume the subtree of the just-read start tag `e` (through its
/// matching end tag) and return the whole element's raw bytes. `start` is
/// the offset of `e`'s `<` in `xml`. The reader is left positioned after
/// the end tag, exactly as if the caller had skipped the subtree.
pub fn capture_subtree(
    xml: &[u8],
    start: usize,
    reader: &mut Reader<&[u8]>,
    e: &BytesStart,
) -> Result<Option<Vec<u8>>, DocxError> {
    let end_tag = e.to_end().into_owned();
    let mut skip = Vec::new();
    reader.read_to_end_into(end_tag.name(), &mut skip)?;
    let end = reader.buffer_position() as usize;
    Ok(slice_fragment(xml, start, end))
}

/// Qualified name of a fragment's outermost element: `<w:framePr …/>` →
/// `w:framePr`. Empty for anything that is not an element.
pub fn fragment_qname(fragment: &[u8]) -> &[u8] {
    let trimmed = fragment
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .map_or(&[][..], |i| &fragment[i..]);
    let Some(body) = trimmed.strip_prefix(b"<") else {
        return &[];
    };
    let len = body
        .iter()
        .position(|&b| b.is_ascii_whitespace() || b == b'/' || b == b'>')
        .unwrap_or(body.len());
    &body[..len]
}

/// Prefix of a qualified name (`w14:glow` → `Some("w14")`; `glow` →
/// `None`).
fn prefix_of(qname: &[u8]) -> Option<String> {
    let colon = qname.iter().position(|&b| b == b':')?;
    Some(String::from_utf8_lossy(&qname[..colon]).into_owned())
}

/// `true` when every namespace prefix `fragment` uses (element and
/// attribute names, nested elements included) is one the writer can
/// bind: `w` (always declared), `xml` (implicit), a prefix the fragment's
/// own outermost start tag declares, or one bound on the part's root
/// (`scope`) — which the writer re-declares on its synthesized root. A
/// fragment relying on a binding from some intermediate ancestor cannot
/// be preserved safely and reports `false`.
pub fn bound_by_root(fragment: &[u8], scope: &NamespaceScope) -> bool {
    let mut reader = Reader::from_reader(fragment);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    /* Issue #119 — declarations are scoped: a prefix declared on an
    element (or any ancestor inside the fragment) binds that element
    and its subtree, exactly as XML Namespaces resolve it. A drawing
    fragment declaring `xmlns:wps` on its `<wps:wsp>` is self-contained
    and stays so wherever the writer splices it. */
    let mut declared_stack: Vec<Vec<String>> = Vec::new();
    let in_scope = |p: &str, stack: &[Vec<String>]| -> bool {
        p == "w"
            || p == "xml"
            || stack.iter().any(|d| d.iter().any(|q| q == p))
            || scope.uri(p).is_some()
    };
    loop {
        let ev = match reader.read_event_into(&mut buf) {
            Ok(ev) => ev,
            /* A fragment sliced from a part quick-xml already parsed is
            well-formed by construction; if it somehow is not, keep the
            bytes rather than lose them. */
            Err(_) => return true,
        };
        let is_empty = matches!(ev, Event::Empty(_));
        match ev {
            Event::Start(e) | Event::Empty(e) => {
                let mut declared: Vec<String> = Vec::new();
                let mut used: BTreeSet<String> = BTreeSet::new();
                if let Some(p) = prefix_of(e.name().as_ref()) {
                    used.insert(p);
                }
                for a in e.attributes().flatten() {
                    let key = a.key.as_ref();
                    if key == b"xmlns" {
                        continue;
                    }
                    if let Some(p) = key.strip_prefix(b"xmlns:") {
                        declared.push(String::from_utf8_lossy(p).into_owned());
                        continue;
                    }
                    if let Some(p) = prefix_of(key) {
                        used.insert(p);
                    }
                }
                declared_stack.push(declared);
                if !used.iter().all(|p| in_scope(p, &declared_stack)) {
                    return false;
                }
                if is_empty {
                    declared_stack.pop();
                }
            }
            Event::End(_) => {
                declared_stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    true
}

/// Issue #112 — every namespace prefix `xml` (a body: a sequence of
/// sibling elements, no root of its own) uses without an in-scope
/// declaration of its own, i.e. the prefixes the enclosing root MUST bind.
/// `w` and `xml` are always reported bound-elsewhere and never returned.
/// A prefix declared on an ancestor inside the sequence (`<a:graphic
/// xmlns:a=…>`, the way Word writes DrawingML) does not count.
pub fn unbound_prefixes(xml: &[u8]) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut declared_stack: Vec<Vec<String>> = Vec::new();
    while let Ok(ev) = reader.read_event_into(&mut buf) {
        let is_empty = matches!(ev, Event::Empty(_));
        match ev {
            Event::Start(e) | Event::Empty(e) => {
                let mut declared: Vec<String> = Vec::new();
                let mut used: BTreeSet<String> = BTreeSet::new();
                if let Some(p) = prefix_of(e.name().as_ref()) {
                    used.insert(p);
                }
                for a in e.attributes().flatten() {
                    let key = a.key.as_ref();
                    if key == b"xmlns" {
                        continue;
                    }
                    if let Some(p) = key.strip_prefix(b"xmlns:") {
                        declared.push(String::from_utf8_lossy(p).into_owned());
                        continue;
                    }
                    if let Some(p) = prefix_of(key) {
                        used.insert(p);
                    }
                }
                declared_stack.push(declared);
                for p in used {
                    let bound =
                        p == "w" || p == "xml" || declared_stack.iter().any(|d| d.contains(&p));
                    if !bound {
                        out.insert(p);
                    }
                }
                if is_empty {
                    declared_stack.pop();
                }
            }
            Event::End(_) => {
                declared_stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

/// Append `fragment` to the bag behind `slot` when the writer can keep it
/// namespace-well-formed ([`bound_by_root`]); drop it otherwise. The
/// one-liner every unmodeled-child arm in the part parsers calls.
pub fn stash(slot: &mut Option<Box<GrabBag>>, fragment: Vec<u8>, scope: &NamespaceScope) {
    if bound_by_root(&fragment, scope) {
        GrabBag::push_into(slot, fragment);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(xml: &str) -> NamespaceScope {
        let mut r = Reader::from_reader(xml.as_bytes());
        let mut buf = Vec::new();
        loop {
            match r.read_event_into(&mut buf).unwrap() {
                Event::Start(e) | Event::Empty(e) => return NamespaceScope::from_root(&e),
                Event::Eof => panic!("no root"),
                _ => {}
            }
        }
    }

    #[test]
    fn qname_of_empty_and_container_fragments() {
        assert_eq!(fragment_qname(br#"<w:framePr w:w="1"/>"#), b"w:framePr");
        assert_eq!(
            fragment_qname(b"<w:rPrChange><w:rPr/></w:rPrChange>"),
            b"w:rPrChange"
        );
        assert_eq!(fragment_qname(b"<w:noProof/>"), b"w:noProof");
        assert_eq!(fragment_qname(b"  <w14:glow>"), b"w14:glow");
        assert_eq!(fragment_qname(b"text"), b"");
    }

    #[test]
    fn w_and_root_bound_prefixes_are_accepted() {
        let s = scope(r#"<w:document xmlns:w="urn:w" xmlns:w14="urn:w14"/>"#);
        assert!(bound_by_root(br#"<w:fitText w:val="1440" w:id="7"/>"#, &s));
        assert!(bound_by_root(
            br#"<w14:glow w14:rad="63500"><w14:srgbClr w14:val="FFC000"/></w14:glow>"#,
            &s
        ));
        /* Attribute-only use of a root-bound prefix. */
        assert!(bound_by_root(br#"<w:p w14:paraId="1"/>"#, &s));
        /* `xml:` is implicit. */
        assert!(bound_by_root(br#"<w:t xml:space="preserve">x</w:t>"#, &s));
    }

    #[test]
    fn self_declared_prefix_is_accepted_without_root_binding() {
        let s = scope(r#"<w:document xmlns:w="urn:w"/>"#);
        assert!(bound_by_root(
            br#"<mc:AlternateContent xmlns:mc="urn:mc"><mc:Choice/></mc:AlternateContent>"#,
            &s
        ));
    }

    #[test]
    fn unbindable_prefix_is_rejected_and_dropped() {
        let s = scope(r#"<w:document xmlns:w="urn:w"/>"#);
        assert!(!bound_by_root(b"<foo:bar/>", &s));
        /* Declared on a NESTED element only — the outer element still
        uses it unbound. */
        assert!(!bound_by_root(
            br#"<foo:bar><foo:baz xmlns:foo="urn:foo"/></foo:bar>"#,
            &s
        ));
        let mut slot = None;
        stash(&mut slot, b"<foo:bar/>".to_vec(), &s);
        assert!(slot.is_none());
    }

    /// Issue #110 — a passthrough capture must be exactly the named
    /// element; a misaligned range (the BOM-shifted `dy><w:p>…</w`) or a
    /// different element (`<w:pPr>` for `w:p`) is refused.
    #[test]
    fn slice_element_accepts_only_the_named_element() {
        let xml = br#"<w:body><w:p w:a="1"><w:pPr/></w:p><w:p/><w:tbl>
</w:tbl></w:body>"#;
        let p_start = "<w:body>".len();
        let p_end = p_start + "<w:p w:a=\"1\"><w:pPr/></w:p>".len();
        assert_eq!(&xml[p_start..p_end], b"<w:p w:a=\"1\"><w:pPr/></w:p>");
        assert_eq!(
            slice_element(xml, p_start, p_end, b"w:p").as_deref(),
            Some(b"<w:p w:a=\"1\"><w:pPr/></w:p>".as_slice())
        );
        /* Self-closing `<w:p/>`. */
        assert_eq!(
            slice_element(xml, p_end, p_end + 6, b"w:p").as_deref(),
            Some(b"<w:p/>".as_slice())
        );
        /* Start tag broken across a newline. */
        let t_start = p_end + 6;
        let t_end = xml.len() - "</w:body>".len();
        assert_eq!(
            slice_element(xml, t_start, t_end, b"w:tbl").as_deref(),
            Some(b"<w:tbl>\n</w:tbl>".as_slice())
        );
        /* Three bytes early / late — the issue #110 shape. */
        assert_eq!(slice_element(xml, p_start - 3, p_end - 3, b"w:p"), None);
        assert_eq!(slice_element(xml, p_start + 3, p_end + 3, b"w:p"), None);
        /* A different element whose name merely starts the same way. */
        assert_eq!(slice_element(xml, p_start + 13, p_start + 21, b"w:p"), None);
        assert_eq!(
            slice_element(xml, p_start + 13, p_start + 21, b"w:pPr").as_deref(),
            Some(b"<w:pPr/>".as_slice())
        );
        /* Out-of-range / inverted ranges. */
        assert_eq!(slice_element(xml, 0, xml.len() + 1, b"w:body"), None);
        assert_eq!(slice_element(xml, 10, 8, b"w:p"), None);
    }

    /// Issue #112 — only prefixes the body uses WITHOUT an in-scope
    /// declaration of its own need the root; `w` / `xml` never count.
    #[test]
    fn unbound_prefixes_ignores_locally_declared_ones() {
        let body = concat!(
            r#"<w:p w14:paraId="1"><w:r><w:t xml:space="preserve">x</w:t>"#,
            r#"<w:drawing><wp:inline><a:graphic xmlns:a="urn:a"><a:graphicData>"#,
            r#"<pic:pic xmlns:pic="urn:pic"><pic:blipFill><a:blip r:embed="rId5"/></pic:blipFill></pic:pic>"#,
            r#"</a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#,
            r#"<w:p><w:r><w:t>plain a: text pic: text</w:t></w:r></w:p>"#,
        );
        let unbound: Vec<String> = unbound_prefixes(body.as_bytes()).into_iter().collect();
        assert_eq!(unbound, ["r", "w14", "wp"]);
        assert!(unbound_prefixes(b"<w:p><w:r><w:t>a: b</w:t></w:r></w:p>").is_empty());
    }

    #[test]
    fn stash_appends_in_order() {
        let s = scope(r#"<w:document xmlns:w="urn:w"/>"#);
        let mut slot = None;
        stash(&mut slot, b"<w:a/>".to_vec(), &s);
        stash(&mut slot, b"<w:b/>".to_vec(), &s);
        assert_eq!(
            GrabBag::fragments_of(&slot),
            &[b"<w:a/>".to_vec(), b"<w:b/>".to_vec()]
        );
    }
}
