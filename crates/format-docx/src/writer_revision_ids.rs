//! Issue #295 — every tracked-change annotation id a save emits is unique
//! across the saved package.
//!
//! Regenerated content never prints an annotation `w:id` directly. It
//! prints a TOKEN: [`token`]`(Some(n))` — KEEP `n` — for an id the model
//! carries (a source `<w:ins w:id>`, a run's `<w:rPrChange w:id>` riding
//! its grab bag or its verbatim `<w:rPr>` bytes, a `<w:pPrChange>`, …),
//! and [`token`]`(None)` — FRESH — for one the writer has to mint (an
//! engine-made revision). [`tokenize_annotations`] turns the literal ids
//! of a regenerated paragraph / table into KEEP tokens after the fact, so
//! every byte path (verbatim source run properties included) is covered.
//! Once every regenerated part of a save is built, [`finalize`] resolves
//! the tokens over all of them together:
//!
//! - an id that passthrough bytes spell literally (clean paragraphs and
//!   tables, positioned range markers) is TAKEN in its part — those
//!   bytes cannot change;
//! - a KEEP `n` keeps `n` the first time it is resolved, in document
//!   order, unless its own part spells `n` literally;
//! - every other token gets the next id above every id the package uses
//!   (the parts the save does not regenerate included), so a minted id
//!   collides with nothing.
//!
//! Fidelity first: only duplicates the save itself would create are
//! resolved — two parts the SOURCE already gave one id keep it.
//!
//! So a run split in two — sub-range formatting, a paragraph split, a
//! writer cut at a revision / hyperlink / field boundary — writes its
//! `<w:rPrChange>` with the source id on the first half and a fresh id on
//! the other; a wrapper re-opened around a hyperlink gets its own id; and
//! engine-made revisions no longer share the old per-paragraph `1, 2, …`
//! fallback (nor a paragraph mark the `w:id="0"` one). A save that
//! regenerates nothing carries no token and is left untouched.
//!
//! Range markers (`<w:moveFromRangeStart/End>`, …) are never tokenized:
//! an end shares its start's id, and both are positioned verbatim bytes.

use std::collections::HashSet;

/// The token delimiters: Private Use Area characters, legal in an XML
/// attribute value and never written by the writer otherwise.
const OPEN: char = '\u{E000}';
const CLOSE: char = '\u{E001}';

/// The elements whose `w:id` is a tracked-change annotation id (one id
/// space in WordprocessingML). Range starts / ends are listed separately:
/// their ids are reserved but never rewritten.
const ANNOTATIONS: &[&[u8]] = &[
    b"w:ins",
    b"w:del",
    b"w:moveFrom",
    b"w:moveTo",
    b"w:rPrChange",
    b"w:pPrChange",
    b"w:sectPrChange",
    b"w:numberingChange",
    b"w:tblPrChange",
    b"w:tblPrExChange",
    b"w:tblGridChange",
    b"w:trPrChange",
    b"w:tcPrChange",
    b"w:cellIns",
    b"w:cellDel",
    b"w:cellMerge",
];

const RANGE_MARKERS: &[&[u8]] = &[
    b"w:moveFromRangeStart",
    b"w:moveFromRangeEnd",
    b"w:moveToRangeStart",
    b"w:moveToRangeEnd",
    b"w:customXmlInsRangeStart",
    b"w:customXmlDelRangeStart",
    b"w:customXmlMoveFromRangeStart",
    b"w:customXmlMoveToRangeStart",
];

/// The `w:id` value regenerated content writes: KEEP `id`, or FRESH.
pub(super) fn token(id: Option<u32>) -> String {
    match id {
        Some(n) => format!("{OPEN}{n}{CLOSE}"),
        None => format!("{OPEN}{CLOSE}"),
    }
}

/// Every annotation start tag in `xml[from..]` whose `w:id` is a literal
/// number: rewrite the number into a KEEP token. Range markers and
/// already-tokenized ids are left alone.
pub(super) fn tokenize_annotations(xml: &mut String, from: usize) {
    let mut edits: Vec<(usize, usize, u32)> = Vec::new();
    for tag in start_tags(&xml.as_bytes()[from..]) {
        if !ANNOTATIONS.contains(&tag.name) {
            continue;
        }
        if let Some((s, e, IdValue::Literal(n))) = tag.id {
            edits.push((from + s, from + e, n));
        }
    }
    for (s, e, n) in edits.into_iter().rev() {
        xml.replace_range(s..e, &token(Some(n)));
    }
}

/// Resolve every token in `parts` (document order: the first part first);
/// `reserved` adds the annotation ids of the parts the save passes through
/// untouched. A KEEP `n` keeps `n` unless the same part spells `n`
/// literally or an earlier KEEP already claimed it (a duplicate this save
/// would introduce); every other token gets an id above every id of the
/// package — so a fresh id never collides with anything. An id two parts
/// of the SOURCE already shared is left as the source had it (fidelity
/// first: only duplicates the save would create are resolved). A no-op —
/// not a byte changed — when no part carries a token.
pub(super) fn finalize(parts: &mut [&mut Vec<u8>], reserved: &HashSet<u32>) {
    if !parts.iter().any(|p| has_tokens(p)) {
        return;
    }
    let mut max = reserved.iter().copied().max().unwrap_or(0);
    let mut literals: Vec<HashSet<u32>> = Vec::with_capacity(parts.len());
    for p in parts.iter() {
        let mut own = HashSet::new();
        for tag in start_tags(p) {
            let known = ANNOTATIONS.contains(&tag.name) || RANGE_MARKERS.contains(&tag.name);
            match tag.id {
                Some((_, _, IdValue::Literal(n))) if known => {
                    own.insert(n);
                    max = max.max(n);
                }
                Some((_, _, IdValue::Keep(n))) => max = max.max(n),
                _ => {}
            }
        }
        literals.push(own);
    }
    let mut next = max.saturating_add(1);
    let mut assigned: HashSet<u32> = HashSet::new();
    for (p, own) in parts.iter_mut().zip(&literals) {
        if !has_tokens(p) {
            continue;
        }
        let mut out = Vec::with_capacity(p.len());
        let mut rest: &[u8] = p;
        while let Some(at) = find(rest, OPEN_UTF8) {
            out.extend_from_slice(&rest[..at]);
            let body = &rest[at + OPEN_UTF8.len()..];
            let Some(close) = find(body, CLOSE_UTF8) else {
                /* Unbalanced (never written so): keep the bytes. */
                out.extend_from_slice(&rest[at..]);
                rest = &[];
                break;
            };
            let keep = std::str::from_utf8(&body[..close])
                .ok()
                .and_then(|d| d.parse::<u32>().ok());
            let id = match keep {
                Some(n) if !own.contains(&n) && assigned.insert(n) => n,
                /* Above every id of the package: collides with nothing. */
                _ => {
                    let c = next;
                    next = next.saturating_add(1);
                    assigned.insert(c);
                    c
                }
            };
            out.extend_from_slice(id.to_string().as_bytes());
            rest = &body[close + CLOSE_UTF8.len()..];
        }
        out.extend_from_slice(rest);
        **p = out;
    }
}

/// [`finalize`] for one standalone part (a part built outside a package
/// save — unit tests).
#[cfg(test)]
pub(super) fn finalize_one(xml: String) -> String {
    let mut bytes = xml.into_bytes();
    finalize(&mut [&mut bytes], &HashSet::new());
    String::from_utf8(bytes).expect("finalize replaces tokens by ASCII digits")
}

/// Every literal annotation / range-marker id in `xml` — what a part the
/// save passes through untouched reserves.
pub(super) fn literal_ids(xml: &[u8], into: &mut HashSet<u32>) {
    for tag in start_tags(xml) {
        if (ANNOTATIONS.contains(&tag.name) || RANGE_MARKERS.contains(&tag.name))
            && let Some((_, _, IdValue::Literal(n))) = tag.id
        {
            into.insert(n);
        }
    }
}

const OPEN_UTF8: &[u8] = "\u{E000}".as_bytes();
const CLOSE_UTF8: &[u8] = "\u{E001}".as_bytes();

fn has_tokens(xml: &[u8]) -> bool {
    find(xml, OPEN_UTF8).is_some()
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[derive(Clone, Copy)]
enum IdValue {
    Literal(u32),
    Keep(u32),
    Fresh,
    Other,
}

struct StartTag<'a> {
    name: &'a [u8],
    /// `(value start, value end, value)` of the tag's `w:id` attribute.
    id: Option<(usize, usize, IdValue)>,
}

/// Every `<w:…` start tag of `xml` (comments / PIs / CDATA do not occur
/// in what the writer splices; an end tag is skipped).
fn start_tags(xml: &[u8]) -> impl Iterator<Item = StartTag<'_>> {
    let mut i = 0;
    std::iter::from_fn(move || {
        let at = i + find(&xml[i..], b"<w:")?;
        let name_start = at + 1;
        let name_len = xml[name_start..]
            .iter()
            .position(|&b| b.is_ascii_whitespace() || b == b'/' || b == b'>')
            .unwrap_or(xml.len() - name_start);
        let name = &xml[name_start..name_start + name_len];
        let tag_end = xml[name_start..]
            .iter()
            .position(|&b| b == b'>')
            .map_or(xml.len(), |p| name_start + p);
        i = tag_end.max(at + 1);
        Some(StartTag {
            name,
            id: id_attr(xml, name_start + name_len, tag_end),
        })
    })
}

/// The `w:id` attribute value within `xml[from..to)` (one start tag's
/// attribute list).
fn id_attr(xml: &[u8], from: usize, to: usize) -> Option<(usize, usize, IdValue)> {
    let attrs = &xml[from..to];
    let mut j = 0;
    while let Some(p) = find(&attrs[j..], b"w:id") {
        let k = j + p;
        j = k + 4;
        if !attrs[..k].last().is_some_and(u8::is_ascii_whitespace) {
            continue;
        }
        let rest = &attrs[j..];
        let eq = rest.iter().position(|&b| !b.is_ascii_whitespace())?;
        if rest[eq] != b'=' {
            continue;
        }
        let after = &rest[eq + 1..];
        let q = after.iter().position(|&b| !b.is_ascii_whitespace())?;
        let quote = after[q];
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        let vs = from + j + eq + 1 + q + 1;
        let len = xml[vs..to].iter().position(|&b| b == quote)?;
        let value = &xml[vs..vs + len];
        let parsed = if let Some(inner) = value
            .strip_prefix(OPEN_UTF8)
            .and_then(|v| v.strip_suffix(CLOSE_UTF8))
        {
            match std::str::from_utf8(inner).ok().filter(|d| !d.is_empty()) {
                Some(d) => d.parse().map_or(IdValue::Other, IdValue::Keep),
                None => IdValue::Fresh,
            }
        } else if !value.is_empty() && value.iter().all(u8::is_ascii_digit) {
            std::str::from_utf8(value)
                .ok()
                .and_then(|d| d.parse().ok())
                .map_or(IdValue::Other, IdValue::Literal)
        } else {
            IdValue::Other
        };
        return Some((vs, vs + len, parsed));
    }
    None
}
