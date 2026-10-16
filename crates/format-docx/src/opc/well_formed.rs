//! Issues #439 / #434 / #435 — strict well-formedness of the WordprocessingML parts
//! the reader walks, and the up-front repair that keeps a malformed part
//! readable while every save of it stays well-formed.
//!
//! The typed parsers stream with quick-xml, which tolerates much that XML
//! 1.0 forbids as long as it never has to decode it: bytes that are not
//! UTF-8, a raw `&` or an undefined entity in character data, control
//! characters, a malformed or repeated attribute, text outside the root
//! element, a part that simply stops (truncation). Such bytes used to sit
//! in regions the reader skips or keeps verbatim (an unselected
//! `mc:Choice`, an unmodeled property, a field's hidden code, a tag name,
//! the prolog) and the writer replayed them:
//!
//! - a capture that needed UTF-8 fell back to regeneration and lost the
//!   markup around it, so a zero-edit save read back with different text
//!   (issue #439 and the #434 sweep reproducers 27778, 39869, 44477);
//! - a raw `&` copied out of a skipped region into a regenerated `<w:t>`
//!   made the writer's output unreadable by its own reader (#434, 37398);
//! - and every such save was a part Word refuses.
//!
//! ## Policy (#434)
//!
//! Every WordprocessingML part is scanned strictly BEFORE any typed walk
//! ([`repair_part`]; it runs after `limits::check_xml_part`, so the part is
//! already inside the [`PackageLimits`] shape bounds, and the repaired part
//! is held to `max_part_bytes` again):
//!
//! - a well-formed part is returned untouched — the byte-identical fast
//!   path every real-world document takes;
//! - a **lexical** defect is repaired in place: invalid UTF-8 → U+FFFD
//!   (a valid character in text, values AND names, so the markup keeps its
//!   shape); an `&` that starts no predefined entity or character
//!   reference → `&amp;` (entities a DTD declares are never expanded:
//!   XXE-safe), `<` in an attribute value → `&lt;`, `]]>` in text →
//!   `]]&gt;` — the escape repair of every verbatim span the part holds;
//!   a character XML 1.0 excludes (raw or referenced: C0 controls but tab /
//!   LF / CR, U+FFFE / U+FFFF, `&#0;`, a surrogate) → U+FFFD; a malformed
//!   or repeated attribute is dropped (the parsers skip it already); text
//!   outside the root element and a misplaced XML declaration are dropped;
//!   `--` inside a comment is split; elements still open at the end of the
//!   part (a truncated part) are closed; a namespace prefix used where no
//!   `xmlns:` binds it (issue #435 — the reader skipped such an element,
//!   the writer then bound the conventional URI, and the second read saw
//!   content the first had not) is bound on the root: to the URI it
//!   conventionally names (`mc`, `wp`, `w14`, … in the part's namespace
//!   family) or to a placeholder URN that names no real namespace, so the
//!   first read already sees what every later read will. The part is reported as
//!   [`DocxWarning::MalformedPart`] `{ repaired: true }` and is then
//!   **regenerate-only**, exactly like a #325 namespace-normalised part:
//!   the repaired bytes REPLACE the source bytes before any capture, so
//!   every passthrough, verbatim span and in-place patch starts from them
//!   and a zero-edit save re-emits the repaired (well-formed) part, never
//!   the source;
//! - a **structural** defect has no faithful repair — an end tag that
//!   does not close the open element (where did the producer mean it to
//!   end?), an end tag with nothing open, markup cut off inside a tag, a
//!   second root element, no root at all: the part is left exactly as it
//!   is and reported with `repaired: false`. The main part's typed parse
//!   then refuses it with a typed error as before (quick-xml checks
//!   end-tag names); a sibling reads as before (usually as defaults).
//!
//! The save side runs the same scan ([`defects`], from
//! `archive::check_part_xml_well_formed`), so the reader and the writer's
//! well-formedness gate agree on what "well-formed" means.
//!
//! Issue #468 — a **UTF-16** part (a `FF FE` / `FE FF` byte-order mark;
//! some generators write them although Word never does) is decoded to
//! UTF-8 BEFORE the scan (`char::decode_utf16` over the BOM-selected byte
//! order; an unpaired surrogate or a trailing odd byte is U+FFFD, a
//! declared `encoding` becomes `UTF-8`), scanned and repaired like any other
//! part, and reported `MalformedPart { repaired: true }` with the encoding
//! named. The decoded bytes REPLACE the source's from then on, so every
//! save writes the part as UTF-8 with a correct declaration; the decoded
//! part is held to the #348 shape caps again. A UTF-32 part has no
//! decoder: reported, `repaired: false` (the main part refuses).
//!
//! Issue #468 — **names** are checked too ([`is_xml_name`], the XML 1.0
//! `Name` production, an ASCII fast path in front of the full
//! `NameStartChar` / `NameChar` ranges): quick-xml accepts any bytes up to
//! a space, `/` or `>` as an element or attribute name, so a byte flip that
//! put `;`, `=`, `"` or a leading digit into a tag or prefix name used to be
//! neither reported nor repaired — and a prefix that is no `NCName` cannot
//! be bound by [`bind_on_root`]. There is no faithful repair (which name did
//! the producer mean?), so an invalid element or attribute name is a
//! STRUCTURAL defect: the main part refuses, a sibling is left verbatim and
//! reported `repaired: false`.
//!
//! Not checked (known gap): entity / attribute-list declarations inside a
//! DOCTYPE (left as they are; OPC forbids DTDs and nothing is ever
//! expanded).

use crate::error::{DocxError, DocxWarning};
use crate::opc::limits::{PackageLimit, PackageLimits};
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::borrow::Cow;

const BOM: &[u8] = b"\xEF\xBB\xBF";
/// U+FFFD REPLACEMENT CHARACTER, UTF-8.
const REPLACEMENT: &[u8] = "\u{FFFD}".as_bytes();

/// `(bom, body)` — a leading UTF-8 byte-order mark split off (quick-xml
/// strips it without counting it in its positions, so every scan runs on
/// the body alone).
fn split_bom(xml: &[u8]) -> (&[u8], &[u8]) {
    match xml.strip_prefix(BOM) {
        Some(body) => (BOM, body),
        None => (&[][..], xml),
    }
}

/// `true` for a part encoded as UTF-32 (a UTF-32 byte-order mark; the LE
/// one starts like UTF-16LE's, but a UTF-16 part never opens with U+0000):
/// beyond a faithful repair — reported, never decoded as UTF-16 noise.
fn is_utf32(xml: &[u8]) -> bool {
    xml.starts_with(b"\xFF\xFE\x00\x00") || xml.starts_with(b"\x00\x00\xFE\xFF")
}

/// Issue #468 — a part encoded as UTF-16 (a UTF-16 byte-order mark):
/// `(little endian, the encoding's name)`. The reader's tokenizer decodes
/// UTF-8 only, so such a part is decoded up front ([`decode_utf16_part`])
/// and then scanned like any other.
fn utf16_order(xml: &[u8]) -> Option<(bool, &'static str)> {
    if is_utf32(xml) {
        None
    } else if xml.starts_with(b"\xFF\xFE") {
        Some((true, "UTF-16LE"))
    } else if xml.starts_with(b"\xFE\xFF") {
        Some((false, "UTF-16BE"))
    } else {
        None
    }
}

/// Issue #468 — the UTF-8 spelling of a UTF-16 part (BOM included in
/// `xml`, dropped from the result): `char::decode_utf16` over the
/// BOM-selected byte order, an unpaired surrogate or a trailing odd byte
/// becomes U+FFFD (the scan repairs what it decodes to like any other
/// text), and a declared `encoding` is rewritten to `UTF-8` — the bytes are
/// UTF-8 from here on, in the reader's walk AND in every save.
fn decode_utf16_part(xml: &[u8], little_endian: bool) -> Vec<u8> {
    let body = xml.get(2..).unwrap_or_default();
    let units = body.chunks_exact(2).map(|c| {
        if little_endian {
            u16::from_le_bytes([c[0], c[1]])
        } else {
            u16::from_be_bytes([c[0], c[1]])
        }
    });
    let mut text = String::with_capacity(body.len());
    for c in char::decode_utf16(units) {
        text.push(c.unwrap_or('\u{FFFD}'));
    }
    if body.len() % 2 == 1 {
        text.push('\u{FFFD}');
    }
    let mut out = text.into_bytes();
    declare_utf8(&mut out);
    out
}

/// Rewrite the `encoding` pseudo-attribute of a leading XML declaration to
/// `UTF-8` (a part decoded from UTF-16 says `encoding="UTF-16"`, which is
/// no longer true of its bytes). Anything else is left alone: no
/// declaration, no `encoding`, or one that already names UTF-8.
fn declare_utf8(text: &mut Vec<u8>) {
    if !text.starts_with(b"<?xml") {
        return;
    }
    let Some(end) = text.windows(2).position(|w| w == b"?>") else {
        return;
    };
    let decl = &text[..end];
    let Some(key) = decl.windows(8).position(|w| w == b"encoding") else {
        return;
    };
    let mut i = key + 8;
    let skip_ws = |i: &mut usize| {
        while decl.get(*i).is_some_and(u8::is_ascii_whitespace) {
            *i += 1;
        }
    };
    skip_ws(&mut i);
    if decl.get(i) != Some(&b'=') {
        return;
    }
    i += 1;
    skip_ws(&mut i);
    let Some(&quote @ (b'"' | b'\'')) = decl.get(i) else {
        return;
    };
    let start = i + 1;
    let Some(len) = decl[start..].iter().position(|&b| b == quote) else {
        return;
    };
    if !decl[start..start + len].eq_ignore_ascii_case(b"utf-8") {
        text.splice(start..start + len, b"UTF-8".iter().copied());
    }
}

/// XML 1.0 `Char`: tab, LF, CR, and everything from U+0020 except the
/// surrogates and U+FFFE / U+FFFF.
fn is_xml_char(c: u32) -> bool {
    matches!(c, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

/// XML 1.0 `NameStartChar` (colon included: the `Name` production, not
/// `NCName`).
fn is_name_start(c: char) -> bool {
    matches!(c,
        ':' | 'A'..='Z' | '_' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

/// XML 1.0 `NameChar`.
fn is_name_char(c: char) -> bool {
    is_name_start(c)
        || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

/// Issue #468 — `true` when `name` (an element or attribute name as the
/// tokenizer cut it) is an XML 1.0 `Name`: a `NameStartChar` then
/// `NameChar`s. ASCII names (every real WordprocessingML name) take a byte
/// loop; anything else is decoded (a name that is not UTF-8 is no name — the
/// scan runs on UTF-8-repaired bytes, so U+FFFD is the only way one shows).
pub(crate) fn is_xml_name(name: &[u8]) -> bool {
    let Some((&first, rest)) = name.split_first() else {
        return false;
    };
    if name.is_ascii() {
        let start = |b: u8| b.is_ascii_alphabetic() || matches!(b, b'_' | b':');
        return start(first)
            && rest
                .iter()
                .all(|&b| start(b) || b.is_ascii_digit() || matches!(b, b'-' | b'.'));
    }
    let Ok(text) = std::str::from_utf8(name) else {
        return false;
    };
    let mut chars = text.chars();
    chars.next().is_some_and(is_name_start) && chars.all(is_name_char)
}

/// Issue #468 — the first element or attribute name of start / empty tag
/// `e` that is no XML `Name`, if any.
fn first_invalid_name<'a>(e: &'a BytesStart<'_>) -> Option<Cow<'a, str>> {
    let name = e.name().into_inner();
    if !is_xml_name(name) {
        return Some(lossy(name));
    }
    /* Nothing after the name but `/` / whitespace: no attributes to read. */
    let after = e.as_ref().get(name.len()..).unwrap_or_default();
    if after.iter().all(|b| b.is_ascii_whitespace() || *b == b'/') {
        return None;
    }
    for a in e.attributes().with_checks(false).flatten() {
        if !is_xml_name(a.key.into_inner()) {
            return Some(lossy(a.key.into_inner()));
        }
    }
    None
}

/// What one strict scan found, per defect class (the warning's detail).
#[derive(Debug, Default)]
struct Tally {
    /// Issue #468 — the part was UTF-16 (the encoding's name): decoded to
    /// UTF-8 before the scan.
    utf16: Option<&'static str>,
    /// Offset of the first byte sequence that is not UTF-8.
    not_utf8: Option<usize>,
    /// `&` starting no reference, `<` in an attribute value, `]]>` in text.
    escapes: u64,
    /// Characters XML 1.0 excludes, raw or as a character reference.
    characters: u64,
    /// Malformed or repeated attributes (dropped).
    attributes: u64,
    /// Text outside the root element, a misplaced XML declaration.
    stray: u64,
    /// Comments holding `--` or ending in `-`.
    comments: u64,
    /// Issue #468 — a `<!DOCTYPE …>` (OPC forbids DTDs): dropped, and the
    /// offset of the first one.
    doctype: Option<usize>,
    /// Issue #468 — offset (in the UTF-8-repaired body) of the start of the
    /// first markup item that carried a counted defect.
    first_at: Option<usize>,
    /// Elements still open at the end of the part.
    unclosed: u64,
    /// Issue #435 — namespace prefixes used where no `xmlns:` binds them,
    /// first use first (bound on the root by the repair).
    unbound: Vec<String>,
}

impl Tally {
    /// Sum of the counted defects — to tell whether one item added any.
    fn weight(&self) -> u64 {
        self.escapes
            + self.characters
            + self.attributes
            + self.stray
            + self.comments
            + self.unbound.len() as u64
            + u64::from(self.doctype.is_some())
    }

    fn detail(&self) -> String {
        let mut parts = Vec::new();
        if let Some(name) = self.utf16 {
            parts.push(format!(
                "{name} encoded (decoded to UTF-8; saved as UTF-8 with the declaration rewritten)"
            ));
        }
        if let Some(at) = self.not_utf8 {
            parts.push(format!("not UTF-8 (first invalid byte at offset {at})"));
        }
        if !self.unbound.is_empty() {
            parts.push(format!(
                "{} undeclared namespace prefixes ({})",
                self.unbound.len(),
                self.unbound.join(", ")
            ));
        }
        if let Some(at) = self.doctype {
            parts.push(format!(
                "DOCTYPE declaration at offset {at} (OPC forbids DTDs)"
            ));
        }
        for (n, what) in [
            (self.escapes, "unescaped `&` / `<` / `]]>`"),
            (self.characters, "characters XML 1.0 excludes"),
            (self.attributes, "malformed or repeated attributes"),
            (self.stray, "stray items outside the root element"),
            (self.comments, "malformed comments"),
            (self.unclosed, "elements left open at the end of the part"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        if let Some(at) = self.first_at {
            parts.push(format!("first defect near offset {at}"));
        }
        parts.join("; ")
    }
}

/// `body[start..end]` replaced by `with`.
struct Patch {
    start: usize,
    end: usize,
    with: Vec<u8>,
}

/// The verdict of [`scan`].
enum Scan {
    /// Repairable — possibly nothing to repair (no patch, no closer).
    Lexical {
        tally: Tally,
        patches: Vec<Patch>,
        /// End tags to append, innermost first (a truncated part).
        closers: Vec<u8>,
    },
    /// No faithful repair exists; the detail names the first defect.
    Structural(String),
}

/// Where a run of characters sits: character data (references allowed,
/// `]]>` not), an attribute value (references allowed, `<` not), or the
/// inside of a CDATA section / PI / comment (no references at all).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    Text,
    Attribute,
    Markup,
}

/// Bytes `repair_chars` must look at: `&`, `<`, `]`, the C0 controls other
/// than tab / LF / CR, and `0xEF` (the lead byte of U+FFFE / U+FFFF).
const NEEDS_LOOK: [bool; 256] = {
    let mut t = [false; 256];
    let mut i = 0;
    while i < 0x20 {
        t[i] = !matches!(i as u8, b'\t' | b'\n' | b'\r');
        i += 1;
    }
    t[b'&' as usize] = true;
    t[b'<' as usize] = true;
    t[b']' as usize] = true;
    t[0xEF] = true;
    t
};

/// Lazily-built repaired copy of a byte run: nothing is allocated until
/// the first defect.
struct Rewrite<'s> {
    src: &'s [u8],
    out: Option<Vec<u8>>,
    copied: usize,
}

impl<'s> Rewrite<'s> {
    fn new(src: &'s [u8]) -> Self {
        Self {
            src,
            out: None,
            copied: 0,
        }
    }

    /// Replace `src[at..at + len]` with `with`.
    fn replace(&mut self, at: usize, len: usize, with: &[u8]) {
        let out = self
            .out
            .get_or_insert_with(|| Vec::with_capacity(self.src.len() + 8));
        out.extend_from_slice(&self.src[self.copied..at]);
        out.extend_from_slice(with);
        self.copied = at + len;
    }

    fn finish(self) -> Option<Vec<u8>> {
        let mut out = self.out?;
        out.extend_from_slice(&self.src[self.copied..]);
        Some(out)
    }
}

/// The value of the character reference `name` (`#123` / `#x7B`, the
/// text between `&` and `;`): `Some(Ok(code point))`, `Some(Err(()))` for a
/// syntactically valid reference to no character (overflow), `None` when
/// `name` is no character reference at all.
fn char_ref_value(name: &[u8]) -> Option<Result<u32, ()>> {
    let digits = name.strip_prefix(b"#")?;
    let (digits, radix) = match digits.strip_prefix(b"x") {
        Some(hex) => (hex, 16),
        None => (digits, 10),
    };
    if digits.is_empty() {
        return None;
    }
    let mut v: u32 = 0;
    let mut overflow = false;
    for &d in digits {
        let d = char::from(d).to_digit(radix)?;
        match v.checked_mul(radix).and_then(|v| v.checked_add(d)) {
            Some(n) => v = n,
            None => overflow = true,
        }
    }
    Some(if overflow { Err(()) } else { Ok(v) })
}

/// The repaired spelling of a run of characters (valid UTF-8 — the scan
/// runs after the UTF-8 repair), `None` when it is already well-formed.
fn repair_chars(s: &[u8], ctx: Context, tally: &mut Tally) -> Option<Vec<u8>> {
    /* Issue #468 — the save-side gate runs this over every byte of a 12 MB
    part: a byte-table prefilter for "nothing here can need repair" keeps
    the common run off the per-byte state machine below. */
    if s.iter().all(|&b| !NEEDS_LOOK[b as usize]) {
        return None;
    }
    let mut rw = Rewrite::new(s);
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'&' if ctx != Context::Markup => {
                let mut j = i + 1;
                while j < s.len() && (s[j].is_ascii_alphanumeric() || s[j] == b'#') {
                    j += 1;
                }
                let name = &s[i + 1..j];
                let terminated = j < s.len() && s[j] == b';';
                if terminated && matches!(name, b"amp" | b"lt" | b"gt" | b"quot" | b"apos") {
                    i = j + 1;
                    continue;
                }
                match terminated.then(|| char_ref_value(name)).flatten() {
                    Some(Ok(c)) if is_xml_char(c) => i = j + 1,
                    Some(_) => {
                        tally.characters += 1;
                        rw.replace(i, j + 1 - i, REPLACEMENT);
                        i = j + 1;
                    }
                    None => {
                        tally.escapes += 1;
                        rw.replace(i, 1, b"&amp;");
                        i += 1;
                    }
                }
            }
            b'<' if ctx == Context::Attribute => {
                tally.escapes += 1;
                rw.replace(i, 1, b"&lt;");
                i += 1;
            }
            b']' if ctx == Context::Text && s[i..].starts_with(b"]]>") => {
                tally.escapes += 1;
                rw.replace(i, 3, b"]]&gt;");
                i += 3;
            }
            b if b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r') => {
                tally.characters += 1;
                rw.replace(i, 1, REPLACEMENT);
                i += 1;
            }
            /* U+FFFE / U+FFFF. */
            0xEF if matches!(s.get(i + 1..i + 3), Some([0xBF, 0xBE | 0xBF])) => {
                tally.characters += 1;
                rw.replace(i, 3, REPLACEMENT);
                i += 3;
            }
            _ => i += 1,
        }
    }
    rw.finish()
}

/// The repaired inside of a comment: excluded characters, and no `--`
/// (nor a trailing `-` before the closing `-->`).
fn repair_comment(inner: &[u8], tally: &mut Tally) -> Option<Vec<u8>> {
    let chars = repair_chars(inner, Context::Markup, tally);
    let s = chars.as_deref().unwrap_or(inner);
    let mut out = Vec::with_capacity(s.len() + 2);
    let mut split = false;
    for &b in s {
        if b == b'-' && out.last() == Some(&b'-') {
            out.push(b' ');
            split = true;
        }
        out.push(b);
    }
    if out.last() == Some(&b'-') {
        out.push(b' ');
        split = true;
    }
    if split {
        tally.comments += 1;
        Some(out)
    } else {
        chars
    }
}

/// The repaired spelling of a start / empty tag — its attribute values
/// escape-repaired, malformed or repeated attributes dropped — or `None`
/// when the tag is well-formed. A rebuilt tag is spelled
/// `<name a="v" …>`: one space before each attribute, double quotes.
fn repair_tag(e: &BytesStart<'_>, empty: bool, tally: &mut Tally) -> Option<Vec<u8>> {
    /* Issue #468 — a first pass that only asks "does anything here need
    repair?" (no allocation); the collecting pass runs for the rare tag
    that does. The tally is counted by the second pass only. */
    let mut clean = true;
    let mut probe = Tally::default();
    for attr in e.attributes() {
        match attr {
            Ok(Attribute { value, .. }) => {
                if repair_chars(&value, Context::Attribute, &mut probe).is_some() {
                    clean = false;
                    break;
                }
            }
            Err(_) => {
                clean = false;
                break;
            }
        }
    }
    if clean {
        return None;
    }
    let mut kept: Vec<(&[u8], Cow<'_, [u8]>)> = Vec::new();
    let mut changed = false;
    for attr in e.attributes() {
        match attr {
            Ok(Attribute { key, value }) => {
                let fixed = repair_chars(&value, Context::Attribute, tally);
                changed |= fixed.is_some();
                kept.push((key.into_inner(), fixed.map_or(value, Cow::Owned)));
            }
            Err(_) => {
                tally.attributes += 1;
                changed = true;
            }
        }
    }
    if !changed {
        return None;
    }
    let mut out = Vec::with_capacity(e.len() + 16);
    out.push(b'<');
    out.extend_from_slice(e.name().as_ref());
    for (key, value) in kept {
        out.push(b' ');
        out.extend_from_slice(key);
        out.extend_from_slice(b"=\"");
        /* A single-quoted value may hold `"`. */
        for &b in value.iter() {
            if b == b'"' {
                out.extend_from_slice(b"&quot;");
            } else {
                out.push(b);
            }
        }
        out.push(b'"');
    }
    out.extend_from_slice(if empty { b"/>" } else { b">" });
    Some(out)
}

fn lossy(b: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(b)
}

/// Most distinct undeclared prefixes one part may bind on its root; past
/// it the part is beyond a faithful repair.
const MAX_UNBOUND_PREFIXES: usize = 256;

/// Issue #435 — `true` for a prefix the scan never reports: `xml` /
/// `xmlns` are bound by the Namespaces spec itself, and a `w:` part with no
/// `xmlns:w` is the reader's documented Transitional fast path (fragments,
/// hand-written parts).
fn is_implicit_prefix(p: &[u8]) -> bool {
    matches!(p, b"xml" | b"xmlns" | b"w")
}

/// `true` for an `NCName`-shaped prefix (ASCII letters, digits, `-`, `.`,
/// `_`, any non-ASCII character; not starting with a digit, `-` or `.`).
/// Only such a prefix can be bound by an `xmlns:` attribute. (A name that
/// is no XML `Name` at all never reaches here: the scan refuses it first,
/// [`first_invalid_name`].)
fn is_ncname(p: &[u8]) -> bool {
    let ok = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_') || b >= 0x80;
    match p.first() {
        Some(&b) => (b.is_ascii_alphabetic() || b == b'_' || b >= 0x80) && p.iter().all(|&b| ok(b)),
        None => false,
    }
}

/// The prefix of a qualified name (`wp:inline` → `wp`), when it is one an
/// `xmlns:` declaration could bind.
fn bindable_prefix(qname: &[u8]) -> Option<&[u8]> {
    let colon = qname.iter().position(|&b| b == b':')?;
    let p = &qname[..colon];
    (is_ncname(p) && !is_implicit_prefix(p)).then_some(p)
}

/// Issue #435 — record the `xmlns:` declarations of tag `e` (scoped to
/// its nesting `level`) on `decls`, and every prefix the tag's name or
/// attributes use that nothing in scope binds on `unbound` (first use
/// first, deduplicated).
fn check_prefixes(
    e: &BytesStart<'_>,
    level: usize,
    decls: &mut Vec<(usize, Vec<u8>)>,
    unbound: &mut Vec<String>,
) {
    /* Issue #468 — the attribute text after the element name: no `:` in it
    means no `xmlns:` declaration and no prefixed attribute to look at. */
    let name_len = e.name().as_ref().len();
    let has_colon = e
        .as_ref()
        .get(name_len..)
        .is_some_and(|r| r.contains(&b':'));
    if has_colon {
        for a in e.attributes().with_checks(false).flatten() {
            if let Some(p) = a.key.as_ref().strip_prefix(b"xmlns:") {
                decls.push((level, p.to_vec()));
            }
        }
    }
    let mut note = |p: &[u8], decls: &[(usize, Vec<u8>)]| {
        if !decls.iter().any(|(_, d)| d == p) {
            let p = lossy(p);
            if !unbound.iter().any(|u| *u == p) {
                unbound.push(p.into_owned());
            }
        }
    };
    if let Some(p) = bindable_prefix(e.name().as_ref()) {
        note(p, decls);
    }
    for a in e
        .attributes()
        .with_checks(false)
        .flatten()
        .take(if has_colon { usize::MAX } else { 0 })
    {
        let key = a.key.as_ref();
        if key == b"xmlns" || key.starts_with(b"xmlns:") {
            continue;
        }
        if let Some(p) = bindable_prefix(key) {
            note(p, decls);
        }
    }
}

/// Issue #435 — the namespace an undeclared `prefix` is bound to on the
/// root: the URI it conventionally names (in the part's `family` when the
/// family table owns it — `r`, `wp`, `a`, … — else among the namespaces
/// the reader understands: `mc`, `wps`, `w14`, `v`, …), or a placeholder
/// URN that names no real namespace (`urn:x-nge-undeclared:<prefix>`), so
/// an unknown element stays unknown to every consumer.
fn binding_for(prefix: &str, family: crate::schema::NsFamily) -> Cow<'static, str> {
    if let Some(uri) = crate::schema::family::uri_for_prefix(prefix, family)
        .or_else(|| crate::schema::mce::conventional_uri(prefix))
    {
        return Cow::Borrowed(uri);
    }
    let mut urn = String::from("urn:x-nge-undeclared:");
    for b in prefix.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_') {
            urn.push(char::from(b));
        } else {
            urn.push_str(&format!("%{b:02X}"));
        }
    }
    Cow::Owned(urn)
}

/// The part's root start tag, as [`scan`] met it.
struct RootTag {
    start: usize,
    end: usize,
    empty: bool,
    /// Index of the patch rewriting the tag (a lexical repair), if any.
    patch: Option<usize>,
    /// The namespace family the root's `xmlns:w` names.
    family: crate::schema::NsFamily,
}

/// The family of the root's `w` binding (Transitional when unbound or
/// foreign).
fn root_family(e: &BytesStart<'_>) -> crate::schema::NsFamily {
    e.attributes()
        .with_checks(false)
        .flatten()
        .find(|a| a.key.as_ref() == b"xmlns:w")
        .and_then(|a| {
            std::str::from_utf8(&a.value)
                .ok()
                .and_then(crate::schema::family::family_of_w_uri)
        })
        .unwrap_or_default()
}

/// Issue #435 — bind every undeclared prefix on the root start tag
/// ([`binding_for`]): the declarations go right before the tag's closing
/// `>` / `/>`, into the patch already rewriting the tag or as an insertion
/// of their own (kept in position order).
fn bind_on_root(body: &[u8], root: &RootTag, unbound: &[String], patches: &mut Vec<Patch>) {
    let mut decls = Vec::new();
    for prefix in unbound {
        decls.extend_from_slice(b" xmlns:");
        decls.extend_from_slice(prefix.as_bytes());
        decls.extend_from_slice(b"=\"");
        decls.extend_from_slice(binding_for(prefix, root.family).as_bytes());
        decls.push(b'"');
    }
    let close = if root.empty { 2 } else { 1 };
    match root.patch {
        Some(i) => {
            let with = &mut patches[i].with;
            let at = with.len() - close;
            with.splice(at..at, decls);
        }
        None => {
            let at = root.end - close;
            debug_assert!(root.start < at && body[at..root.end].ends_with(b">"));
            let idx = patches.partition_point(|p| p.start <= at);
            patches.insert(
                idx,
                Patch {
                    start: at,
                    end: at,
                    with: decls,
                },
            );
        }
    }
}

/// The strict scan of one part body (BOM split off, valid UTF-8). See the
/// module docs for what is lexical and what is structural.
fn scan(body: &[u8]) -> Scan {
    let mut reader = Reader::from_reader(body);
    let config = reader.config_mut();
    config.trim_text(false);
    /* The scan matches end tags itself (and reports instead of failing);
    a `--` inside a comment is repaired, not refused. */
    config.check_end_names = false;
    config.check_comments = false;
    config.allow_unmatched_ends = true;
    let mut tally = Tally::default();
    let mut patches: Vec<Patch> = Vec::new();
    /* `(offset, len)` of every open element's name in `body`. */
    let mut open: Vec<(usize, usize)> = Vec::new();
    /* Issue #435 — `(nesting level, prefix)` of every `xmlns:` declaration
    in scope, innermost last. */
    let mut decls: Vec<(usize, Vec<u8>)> = Vec::new();
    /* The root start tag: its range, whether it is empty, the index of
    the patch rewriting it (if any), and the namespace family its `w`
    binding names. */
    let mut root_tag: Option<RootTag> = None;
    let mut root_seen = false;
    let mut root_closed = false;
    let mut prev = 0usize;
    loop {
        let event = match reader.read_event() {
            Ok(event) => event,
            Err(e) => {
                return Scan::Structural(format!(
                    "markup cut off or unreadable at byte {}: {e}",
                    reader.error_position()
                ));
            }
        };
        let pos = reader.buffer_position() as usize;
        let raw = &body[prev..pos];
        let weight_before = tally.weight();
        match &event {
            Event::Start(e) | Event::Empty(e) => {
                if root_closed {
                    return Scan::Structural(format!("a second root element at byte {prev}"));
                }
                root_seen = true;
                if let Some(bad) = first_invalid_name(e) {
                    return Scan::Structural(format!(
                        "`{bad}` in the tag at byte {prev} is not a valid XML name"
                    ));
                }
                let empty = matches!(event, Event::Empty(_));
                let level = open.len() + 1;
                check_prefixes(e, level, &mut decls, &mut tally.unbound);
                if tally.unbound.len() > MAX_UNBOUND_PREFIXES {
                    return Scan::Structural(format!(
                        "more than {MAX_UNBOUND_PREFIXES} undeclared namespace prefixes"
                    ));
                }
                let repaired = repair_tag(e, empty, &mut tally);
                if root_tag.is_none() {
                    root_tag = Some(RootTag {
                        start: prev,
                        end: pos,
                        empty,
                        patch: repaired.is_some().then_some(patches.len()),
                        family: root_family(e),
                    });
                }
                if let Some(fixed) = repaired {
                    patches.push(Patch {
                        start: prev,
                        end: pos,
                        with: fixed,
                    });
                }
                if !empty {
                    open.push((prev + 1, e.name().as_ref().len()));
                } else {
                    /* An empty element's declarations scope itself only. */
                    while decls.last().is_some_and(|(l, _)| *l >= level) {
                        decls.pop();
                    }
                    root_closed = open.is_empty();
                }
            }
            Event::End(e) => {
                let name = e.name();
                while decls.last().is_some_and(|(l, _)| *l >= open.len()) {
                    decls.pop();
                }
                match open.pop() {
                    Some((at, len)) if body[at..at + len] == *name.as_ref() => {}
                    Some((at, len)) => {
                        return Scan::Structural(format!(
                            "`</{}>` at byte {prev} does not close the open `<{}>`",
                            lossy(name.as_ref()),
                            lossy(&body[at..at + len])
                        ));
                    }
                    None => {
                        return Scan::Structural(format!(
                            "`</{}>` at byte {prev} closes no open element",
                            lossy(name.as_ref())
                        ));
                    }
                }
                root_closed = open.is_empty();
            }
            Event::Text(_) => {
                if open.is_empty() {
                    if !raw
                        .iter()
                        .all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
                    {
                        tally.stray += 1;
                        patches.push(Patch {
                            start: prev,
                            end: pos,
                            with: Vec::new(),
                        });
                    }
                } else if let Some(fixed) = repair_chars(raw, Context::Text, &mut tally) {
                    patches.push(Patch {
                        start: prev,
                        end: pos,
                        with: fixed,
                    });
                }
            }
            Event::CData(_) => {
                if open.is_empty() {
                    tally.stray += 1;
                    patches.push(Patch {
                        start: prev,
                        end: pos,
                        with: Vec::new(),
                    });
                } else if let Some(fixed) =
                    repair_chars(&raw[9..raw.len() - 3], Context::Markup, &mut tally)
                {
                    patches.push(Patch {
                        start: prev,
                        end: pos,
                        with: [b"<![CDATA[".as_slice(), &fixed, b"]]>"].concat(),
                    });
                }
            }
            Event::Comment(_) => {
                if let Some(fixed) = repair_comment(&raw[4..raw.len() - 3], &mut tally) {
                    patches.push(Patch {
                        start: prev,
                        end: pos,
                        with: [b"<!--".as_slice(), &fixed, b"-->"].concat(),
                    });
                }
            }
            Event::PI(_) => {
                if let Some(fixed) =
                    repair_chars(&raw[2..raw.len() - 2], Context::Markup, &mut tally)
                {
                    patches.push(Patch {
                        start: prev,
                        end: pos,
                        with: [b"<?".as_slice(), &fixed, b"?>"].concat(),
                    });
                }
            }
            Event::Decl(_) => {
                /* The XML declaration may only open the part: whatever
                precedes it goes (it is prolog junk — no element yet), and
                a declaration after the root started goes itself. */
                if prev != 0 {
                    tally.stray += 1;
                    if root_seen {
                        patches.push(Patch {
                            start: prev,
                            end: pos,
                            with: Vec::new(),
                        });
                    } else {
                        patches.clear();
                        patches.push(Patch {
                            start: 0,
                            end: prev,
                            with: Vec::new(),
                        });
                    }
                }
            }
            Event::DocType(_) => {
                /* Issue #468 — OPC forbids DTDs: the declaration goes, the
                XML declaration stays. (Entities it declared were never
                expanded anyway.) */
                tally.doctype.get_or_insert(prev);
                patches.push(Patch {
                    start: prev,
                    end: pos,
                    with: Vec::new(),
                });
            }
            Event::Eof => break,
        }
        if tally.first_at.is_none() && tally.weight() > weight_before {
            tally.first_at = Some(prev);
        }
        prev = pos;
    }
    if !root_seen {
        return Scan::Structural("no root element".to_string());
    }
    if let Some(root) = root_tag
        && !tally.unbound.is_empty()
    {
        bind_on_root(body, &root, &tally.unbound, &mut patches);
    }
    let mut closers = Vec::new();
    for &(at, len) in open.iter().rev() {
        closers.extend_from_slice(b"</");
        closers.extend_from_slice(&body[at..at + len]);
        closers.push(b'>');
    }
    tally.unclosed = open.len() as u64;
    Scan::Lexical {
        tally,
        patches,
        closers,
    }
}

/// What [`inspect`] concluded about a whole part.
enum Health<'x> {
    WellFormed,
    /// Repairable: the bytes to scan from (UTF-8-repaired when needed),
    /// the patches, the closers, the tally.
    Repairable {
        body: Cow<'x, [u8]>,
        patches: Vec<Patch>,
        closers: Vec<u8>,
        tally: Tally,
    },
    Unrepairable(String),
}

fn inspect(xml: &[u8]) -> Health<'_> {
    if is_utf32(xml) {
        return Health::Unrepairable(
            "UTF-32 encoded (only UTF-8 and UTF-16 parts are read)".to_string(),
        );
    }
    /* Issue #468 — a UTF-16 part is decoded to UTF-8 first (the BOM goes
    with the encoding), then judged like any other part. */
    let utf16 = utf16_order(xml);
    let source: Cow<'_, [u8]> = match utf16 {
        Some((little_endian, _)) => Cow::Owned(decode_utf16_part(xml, little_endian)),
        None => Cow::Borrowed(split_bom(xml).1),
    };
    let not_utf8 = std::str::from_utf8(&source).err().map(|e| e.valid_up_to());
    let body: Cow<'_, [u8]> = match not_utf8 {
        None => source,
        Some(_) => Cow::Owned(String::from_utf8_lossy(&source).into_owned().into_bytes()),
    };
    match scan(&body) {
        Scan::Structural(detail) => Health::Unrepairable(detail),
        Scan::Lexical {
            mut tally,
            patches,
            closers,
        } => {
            if not_utf8.is_none() && utf16.is_none() && patches.is_empty() && closers.is_empty() {
                return Health::WellFormed;
            }
            tally.not_utf8 = not_utf8;
            tally.utf16 = utf16.map(|(_, name)| name);
            Health::Repairable {
                body,
                patches,
                closers,
                tally,
            }
        }
    }
}

/// Issue #434 — the save-side twin of [`repair_part`]: `None` when `xml`
/// (a whole part) is well-formed by the reader's strict scan, else the
/// defects. `archive::check_part_xml_well_formed` gates on it, so a save
/// the reader would have to repair fails the writer's gate.
pub(crate) fn defects(xml: &[u8]) -> Option<String> {
    match inspect(xml) {
        Health::WellFormed => None,
        Health::Repairable { tally, .. } => Some(tally.detail()),
        Health::Unrepairable(detail) => Some(detail),
    }
}

/// Issue #468 — the production save-side gate. Scan, with the reader's own
/// strict scan ([`defects`]), every XML part of a freshly written package
/// that is NOT a byte-identical copy of a `source` entry (the parts the
/// writer regenerated or spliced: `word/document.xml`, a rewritten header /
/// footer / comments / settings / numbering / styles part, the rels and
/// content types). A defect is `DocxError::MalformedXml` naming the part
/// and the first offending offset; the caller returns no bytes.
pub(crate) fn verify_saved(
    zip_bytes: &[u8],
    source: &[(String, Vec<u8>)],
    already_checked: &str,
) -> Result<(), DocxError> {
    use crate::opc::limits::{PackageLimits, read_entry_bounded};
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))?;
    let mut budget = 0u64;
    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        let name = file.name().to_string();
        if name == already_checked || !(name.ends_with(".xml") || name.ends_with(".rels")) {
            continue;
        }
        let data = read_entry_bounded(file, &name, &PackageLimits::DEFAULT, &mut budget)?;
        if source.iter().any(|(n, d)| *n == name && *d == data) {
            continue;
        }
        verify_part(&name, &data)?;
    }
    Ok(())
}

/// [`verify_saved`] for one part's bytes, before they are packed.
pub(crate) fn verify_part(name: &str, data: &[u8]) -> Result<(), DocxError> {
    match defects(data) {
        None => Ok(()),
        Some(detail) => Err(DocxError::MalformedXml(format!(
            "the saved `{name}` is not well-formed XML ({detail}); the save was refused"
        ))),
    }
}

/// Issue #435 — a main part whose root element is no `document` (markup
/// spliced in front of the real root now wraps it — `<w:t><w:document …>`)
/// is beyond a faithful repair: the reader walks it leniently, but the
/// writer synthesizes a `<w:document>` root that re-declares the FIRST
/// element's bindings only, so whatever the wrapped root declared is lost
/// on save. Reported (`repaired: false`) — the part reads as before. A
/// root that is no WordprocessingML at all is `NotWordprocessingMl`'s
/// business, not this check's.
pub(crate) fn check_main_root(name: &str, xml: &[u8], warnings: &mut Vec<DocxWarning>) {
    use crate::schema::family::RootBinding;
    let (_, body) = split_bom(xml);
    let mut reader = Reader::from_reader(body);
    let qname = loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if e.local_name().as_ref() == b"document" {
                    return;
                }
                break lossy(e.name().as_ref()).into_owned();
            }
            Ok(Event::Eof) | Err(_) => return,
            Ok(_) => {}
        }
    };
    if crate::schema::ns_normalize::inspect_root(xml) != RootBinding::NotWordprocessingMl {
        warnings.push(DocxWarning::MalformedPart {
            part: name.to_string(),
            detail: format!(
                "the root element is `{qname}`, not a WordprocessingML `document` (its own namespace bindings are not re-declared on save)"
            ),
            repaired: false,
        });
    }
}

/// Issues #439 / #434 — check one WordprocessingML part (`name`, its
/// archive entry) and return the bytes the reader must parse: `xml`
/// itself when it is well-formed (or beyond a faithful repair), else its
/// repaired spelling (reported on `warnings`; see the module docs). A
/// repaired part larger than `limits.max_part_bytes` is refused like any
/// oversized part.
pub(crate) fn repair_part(
    name: &str,
    xml: Vec<u8>,
    limits: &PackageLimits,
    warnings: &mut Vec<DocxWarning>,
) -> Result<Vec<u8>, DocxError> {
    let decoded_utf16;
    let out = match inspect(&xml) {
        Health::WellFormed => return Ok(xml),
        Health::Unrepairable(detail) => {
            warnings.push(DocxWarning::MalformedPart {
                part: name.to_string(),
                detail,
                repaired: false,
            });
            return Ok(xml);
        }
        Health::Repairable {
            body,
            patches,
            closers,
            tally,
        } => {
            let (bom, _) = split_bom(&xml);
            let mut out = Vec::with_capacity(xml.len() + closers.len() + 64);
            out.extend_from_slice(bom);
            let mut at = 0;
            for p in &patches {
                out.extend_from_slice(&body[at..p.start]);
                out.extend_from_slice(&p.with);
                at = p.end;
            }
            out.extend_from_slice(&body[at..]);
            out.extend_from_slice(&closers);
            decoded_utf16 = tally.utf16.is_some();
            warnings.push(DocxWarning::MalformedPart {
                part: name.to_string(),
                detail: tally.detail(),
                repaired: true,
            });
            out
        }
    };
    if out.len() as u64 > limits.max_part_bytes {
        return Err(DocxError::PackageTooLarge {
            limit: PackageLimit::PartBytes,
            max: limits.max_part_bytes,
            part: Some(name.to_string()),
        });
    }
    /* Issue #468 — the #348 shape caps ran on the UTF-16 bytes (no
    structure to count there); the decoded part is held to them now. */
    if decoded_utf16 {
        crate::opc::limits::check_xml_part(name, &out, limits)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(xml: &[u8]) -> (Vec<u8>, Vec<DocxWarning>) {
        let mut w = Vec::new();
        let out = repair_part(
            "word/document.xml",
            xml.to_vec(),
            &PackageLimits::DEFAULT,
            &mut w,
        )
        .expect("repair");
        (out, w)
    }

    /// `xml` repaired: the output (as a string) and the warning's detail.
    fn repaired(xml: &[u8]) -> (String, String) {
        let (out, w) = run(xml);
        let [
            DocxWarning::MalformedPart {
                detail,
                repaired: true,
                ..
            },
        ] = w.as_slice()
        else {
            panic!("expected one repaired-part warning, got {w:?}");
        };
        assert_eq!(defects(&out), None, "the repair is well-formed");
        (String::from_utf8(out).expect("utf-8"), detail.clone())
    }

    /// Issue #468 — a DOCTYPE is dropped from the prolog (the XML
    /// declaration stays) and is itself a save-side defect.
    #[test]
    fn a_doctype_is_dropped_and_is_a_defect() {
        let xml = b"<?xml version=\"1.0\"?>\n<!DOCTYPE r [<!ENTITY e \"X\">]><r><t>a</t></r>";
        assert!(defects(xml).is_some_and(|d| d.contains("DOCTYPE")));
        let (out, detail) = repaired(xml);
        assert_eq!(out, "<?xml version=\"1.0\"?>\n<r><t>a</t></r>");
        assert!(detail.contains("DOCTYPE"), "{detail}");
    }

    #[test]
    fn a_well_formed_part_is_returned_untouched() {
        let xml = concat!(
            "\u{feff}<?xml version=\"1.0\"?>\n<!-- a - b --><w:document a='x\"y' b=\"&amp;&#x41;&#65;\">",
            "<w:body><w:p><w:r><w:t>é &lt; &gt;] ]&quot;&apos;\t</w:t></w:r></w:p>",
            "<![CDATA[ & < ]]><?pi data?></w:body></w:document>\n"
        );
        let (out, w) = run(xml.as_bytes());
        assert_eq!(out, xml.as_bytes());
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(defects(xml.as_bytes()), None);
    }

    #[test]
    fn bytes_that_are_not_utf8_become_replacement_characters() {
        let (out, detail) =
            repaired(b"\xEF\xBB\xBF<w:document><w:fld\xC3har/><w:t>a\xFFb</w:t></w:document>");
        assert_eq!(
            out,
            "\u{feff}<w:document><w:fld\u{FFFD}har/><w:t>a\u{FFFD}b</w:t></w:document>"
        );
        assert!(detail.contains("not UTF-8"), "{detail}");
    }

    #[test]
    fn stray_ampersands_and_unknown_entities_are_escaped() {
        let (out, detail) = repaired(
            b"<r><t>a & b &bogus; &#; &#xZZ; &amp; &#x41;</t><e v=\"1 & 2 < 3\" w='q\"&x'/></r>",
        );
        assert_eq!(
            out,
            "<r><t>a &amp; b &amp;bogus; &amp;#; &amp;#xZZ; &amp; &#x41;</t>\
             <e v=\"1 &amp; 2 &lt; 3\" w=\"q&quot;&amp;x\"/></r>"
        );
        assert!(detail.contains("7 unescaped"), "{detail}");
    }

    #[test]
    fn excluded_characters_become_replacement_characters() {
        let (out, detail) = repaired(
            "<r>a&#0;b&#x1;c&#xFFFE;d&#xD800;e&#99999999999;f\u{1}g\u{FFFF}h<e v=\"\u{2}\"/></r>"
                .as_bytes(),
        );
        assert_eq!(
            out,
            "<r>a\u{FFFD}b\u{FFFD}c\u{FFFD}d\u{FFFD}e\u{FFFD}f\u{FFFD}g\u{FFFD}h<e v=\"\u{FFFD}\"/></r>"
        );
        assert!(detail.contains("8 characters"), "{detail}");
    }

    #[test]
    fn cdata_ends_comments_and_processing_instructions_are_repaired() {
        let (out, _) = repaired(b"<r>x]]>y<!-- a -- b ---><![CDATA[\x01]]><?pi \x02?></r>");
        assert_eq!(
            out,
            "<r>x]]&gt;y<!-- a - - b - --><![CDATA[\u{FFFD}]]><?pi \u{FFFD}?></r>"
        );
    }

    #[test]
    fn malformed_and_repeated_attributes_are_dropped() {
        let (out, detail) = repaired(b"<r><e a=\"1\" b=x c=\"3\" a=\"4\"/><f g></f></r>");
        assert_eq!(out, "<r><e a=\"1\" c=\"3\"/><f></f></r>");
        assert!(
            detail.contains("3 malformed or repeated attributes"),
            "{detail}"
        );
    }

    #[test]
    fn junk_outside_the_root_and_a_late_declaration_are_dropped() {
        let (out, detail) = repaired(b"PK\x03junk<?xml version=\"1.0\"?>\nzz<r>a</r>tail&#0;");
        assert_eq!(out, "<?xml version=\"1.0\"?><r>a</r>");
        assert!(detail.contains("stray"), "{detail}");
        let (out, _) = repaired(b"<r>a<?xml version=\"1.0\"?>b</r>");
        assert_eq!(out, "<r>ab</r>");
    }

    #[test]
    fn a_truncated_part_is_closed() {
        let (out, detail) = repaired(b"<w:document><w:body><w:p><w:r><w:t>abc &am");
        assert_eq!(
            out,
            "<w:document><w:body><w:p><w:r><w:t>abc &amp;am</w:t></w:r></w:p></w:body></w:document>"
        );
        assert!(detail.contains("5 elements left open"), "{detail}");
    }

    #[test]
    fn structural_defects_are_reported_and_left_alone() {
        for (xml, what) in [
            (&b"<r><a></b></r>"[..], "does not close"),
            (b"<r></r></x>", "closes no open element"),
            (b"<r/><s/>", "second root"),
            (b"  just text ", "no root"),
            (b"<r><a b=\"1</r>", "cut off"),
        ] {
            let (out, w) = run(xml);
            assert_eq!(out, xml, "left as it is");
            assert!(
                matches!(
                    w.as_slice(),
                    [DocxWarning::MalformedPart { detail, repaired: false, .. }] if detail.contains(what)
                ),
                "{}: {w:?}",
                lossy(xml)
            );
            assert!(defects(xml).is_some_and(|d| d.contains(what)));
        }
    }

    const NS_W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

    #[test]
    fn undeclared_prefixes_are_bound_on_the_root() {
        let xml = format!(
            r#"<w:document xmlns:w="{NS_W}"><w:body><mc:AlternateContent/><wp:inline/><foo:bar x:y="1"/><a:graphic xmlns:a="urn:local"><a:blip/></a:graphic><a:x/></w:body></w:document>"#
        );
        let (out, detail) = repaired(xml.as_bytes());
        let root = format!(
            concat!(
                r#"<w:document xmlns:w="{}""#,
                r#" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#,
                r#" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing""#,
                r#" xmlns:foo="urn:x-nge-undeclared:foo" xmlns:x="urn:x-nge-undeclared:x""#,
                r#" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">"#
            ),
            NS_W
        );
        assert_eq!(
            out,
            xml.replacen(&format!(r#"<w:document xmlns:w="{NS_W}">"#), &root, 1)
        );
        assert!(
            detail.contains("5 undeclared namespace prefixes (mc, wp, foo, x, a)"),
            "{detail}"
        );
    }

    #[test]
    fn a_strict_root_binds_family_prefixes_in_its_own_spelling() {
        let strict = crate::schema::NS_W_STRICT;
        let (out, _) = repaired(
            format!(r#"<w:document xmlns:w="{strict}"><w:hyperlink r:id="rId1"/></w:document>"#)
                .as_bytes(),
        );
        assert!(
            out.contains(r#" xmlns:r="http://purl.oclc.org/ooxml/officeDocument/relationships">"#),
            "{out}"
        );
    }

    #[test]
    fn bindings_join_a_rewritten_or_empty_root_tag() {
        let (out, _) = repaired(b"<w:document a=\"&\"><mc:x/></w:document>");
        assert_eq!(
            out,
            "<w:document a=\"&amp;\" xmlns:mc=\"http://schemas.openxmlformats.org/markup-compatibility/2006\"><mc:x/></w:document>"
        );
        let (out, _) = repaired("<x:root \u{FFFD}:a='1'/>".as_bytes());
        assert_eq!(
            out,
            "<x:root \u{FFFD}:a='1' xmlns:x=\"urn:x-nge-undeclared:x\" xmlns:\u{FFFD}=\"urn:x-nge-undeclared:%EF%BF%BD\"/>"
        );
    }

    #[test]
    fn implicit_and_unbindable_prefixes_are_never_reported() {
        for xml in [
            &br#"<w:document><w:p xml:space="preserve"/></w:document>"#[..],
            br#"<r xmlns:p="u"><p:a/><s xmlns:q="v"><q:b p:c="1"/></s></r>"#,
        ] {
            let (out, w) = run(xml);
            assert_eq!(out, xml);
            assert!(w.is_empty(), "{}: {w:?}", lossy(xml));
        }
    }

    /// Issue #468 — a tag or attribute name that is no XML `Name` (a byte
    /// flip put `;` `=` `"` or a leading digit in it) is a structural
    /// defect: reported, left as it is.
    #[test]
    fn an_invalid_xml_name_is_a_structural_defect() {
        for xml in [
            &b"<r><a=b:c/></r>"[..],
            b"<r><d e;f:g=\"1\"/></r>",
            b"<r><1a/></r>",
            b"<r><a:b\"c\"/></r>",
            b"<r><a 1b=\"x\"/></r>",
            b"<r><a b\"c\"=\"x\"/></r>",
            b"<r>< a/></r>",
            b"<r><a\x01b/></r>",
        ] {
            let (out, w) = run(xml);
            assert_eq!(out, xml, "left as it is");
            assert!(
                matches!(
                    w.as_slice(),
                    [DocxWarning::MalformedPart { detail, repaired: false, .. }]
                        if detail.contains("not a valid XML name")
                ),
                "{}: {w:?}",
                lossy(xml)
            );
            assert!(defects(xml).is_some_and(|d| d.contains("not a valid XML name")));
        }
    }

    #[test]
    fn xml_names_follow_the_name_production() {
        for ok in [
            "w:p",
            "_a",
            ":x",
            "a-b.c1",
            "w14:paraId",
            "\u{E9}t\u{E9}",
            "\u{FFFD}:a",
            "a\u{B7}b",
            "\u{10000}x",
        ] {
            assert!(is_xml_name(ok.as_bytes()), "{ok}");
        }
        for bad in [
            "",
            "1a",
            "-a",
            ".a",
            "a;b",
            "a=b",
            "a\"b",
            "a b",
            "a\u{D7}b",
            "\u{37E}",
            "\u{B7}a",
            "a\u{FFFE}",
        ] {
            assert!(!is_xml_name(bad.as_bytes()), "{bad:?}");
        }
        assert!(!is_xml_name(b"a\xFFb"));
    }

    fn utf16(text: &str, little_endian: bool) -> Vec<u8> {
        let mut out = if little_endian {
            b"\xFF\xFE".to_vec()
        } else {
            b"\xFE\xFF".to_vec()
        };
        for u in text.encode_utf16() {
            out.extend_from_slice(&if little_endian {
                u.to_le_bytes()
            } else {
                u.to_be_bytes()
            });
        }
        out
    }

    /// Issue #468 — a UTF-16 part (either byte order, surrogate pairs
    /// included) is decoded to UTF-8, reported repaired with the encoding
    /// named, and its declaration says UTF-8.
    #[test]
    fn utf16_parts_are_decoded_to_utf8() {
        let text = "<?xml version=\"1.0\" encoding=\"UTF-16\" standalone=\"yes\"?>\n<w:document><w:t>\u{627}\u{644}\u{639} \u{1F600} &amp; é</w:t></w:document>";
        for (little_endian, name) in [(true, "UTF-16LE"), (false, "UTF-16BE")] {
            let (out, detail) = repaired(&utf16(text, little_endian));
            assert_eq!(
                out,
                text.replace("encoding=\"UTF-16\"", "encoding=\"UTF-8\""),
                "{name}"
            );
            assert!(detail.contains(name), "{detail}");
        }
    }

    #[test]
    fn utf16_decoding_repairs_what_it_cannot_decode() {
        /* An unpaired surrogate and a trailing odd byte become U+FFFD; an
        `&` the decoded text leaves bare is escaped like in any part. */
        let mut xml = b"\xFF\xFE".to_vec();
        for u in "<r>a"
            .encode_utf16()
            .chain([0xD800])
            .chain("& b</r>".encode_utf16())
        {
            xml.extend_from_slice(&u.to_le_bytes());
        }
        xml.push(b'x');
        let (out, detail) = repaired(&xml);
        /* The odd byte decodes to U+FFFD after the root: stray text, dropped. */
        assert_eq!(out, "<r>a\u{FFFD}&amp; b</r>");
        assert!(
            detail.contains("UTF-16LE") && detail.contains("1 stray"),
            "{detail}"
        );
    }

    #[test]
    fn a_declared_encoding_is_rewritten_only_when_it_is_not_utf8() {
        for (decl, want) in [
            (
                r#"<?xml version="1.0" encoding="utf-16"?>"#,
                r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            ),
            (
                r#"<?xml version='1.0' encoding = 'UTF-16LE' standalone='yes'?>"#,
                r#"<?xml version='1.0' encoding = 'UTF-8' standalone='yes'?>"#,
            ),
            (
                r#"<?xml version="1.0" encoding="UTF-8"?>"#,
                r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            ),
            (r#"<?xml version="1.0"?>"#, r#"<?xml version="1.0"?>"#),
        ] {
            let (out, _) = repaired(&utf16(&format!("{decl}<r/>"), true));
            assert_eq!(out, format!("{want}<r/>"));
        }
    }

    /// A UTF-32 part is beyond a faithful repair (and is never read as
    /// UTF-16 noise); a UTF-16 part with a structural defect is left as
    /// the bytes it was.
    #[test]
    fn utf32_and_structurally_broken_utf16_parts_are_reported_and_kept() {
        for xml in [
            b"\xFF\xFE\x00\x00<\0\0\0r\0\0\0/\0\0\0>\0\0\0".to_vec(),
            b"\x00\x00\xFE\xFF\0\0\0<\0\0\0r\0\0\0/\0\0\0>".to_vec(),
            utf16("<r><a></b></r>", true),
        ] {
            let (out, w) = run(&xml);
            assert_eq!(out, xml);
            assert!(
                matches!(
                    w.as_slice(),
                    [DocxWarning::MalformedPart {
                        repaired: false,
                        ..
                    }]
                ),
                "{w:?}"
            );
        }
    }

    /// The decoded part is held to the #348 shape caps again (they ran on
    /// UTF-16 bytes with no structure).
    #[test]
    fn a_decoded_utf16_part_is_held_to_the_shape_caps() {
        let limits = PackageLimits {
            max_xml_depth: 4,
            ..PackageLimits::DEFAULT
        };
        let xml = utf16("<a><b><c><d><e><f/></e></d></c></b></a>", true);
        let err = repair_part("p.xml", xml, &limits, &mut Vec::new()).expect_err("too deep");
        assert!(matches!(err, DocxError::PackageTooLarge { .. }), "{err:?}");
    }

    #[test]
    fn a_repair_past_the_part_budget_is_refused() {
        let limits = PackageLimits {
            max_part_bytes: 8,
            ..PackageLimits::DEFAULT
        };
        let err = repair_part("p.xml", b"<a>&&</a>".to_vec(), &limits, &mut Vec::new())
            .expect_err("grows past 8 bytes");
        assert!(matches!(
            err,
            DocxError::PackageTooLarge {
                limit: PackageLimit::PartBytes,
                ..
            }
        ));
    }
}
