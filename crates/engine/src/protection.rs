//! Issue #345 — `<w:documentProtection>` (`word/settings.xml`): the model,
//! and the form-region predicates the engine's protection gate consults.
//!
//! The source `settings.xml` rides the OPC passthrough byte-identical, so
//! the element — hash and salt included — survives every save untouched;
//! [`DocumentProtection`] is a read-only lift of it. The engine enforces
//! [`DocumentProtection::enforced_mode`] (engine-wasm `protection_gate`,
//! classified by `bridge::CommandMeta::protection`).
//!
//! Under `forms` protection only form-field content is editable
//! ([`DocumentTree::form_region_for_edit`]):
//!
//! - a **block-level** content control (`<w:sdt>` around paragraphs /
//!   tables — the `BodyFragment::Open` / `Close` envelope pair of issue
//!   #120, at the body, row, cell or cell-block level);
//! - a **run-level** content control (the `MarkerRole::Open` / `Close`
//!   marker pair of issue #245 inside a paragraph);
//! - the result of a legacy **text form field** (`FORMTEXT`, a local
//!   [`crate::Field`]) — edited through
//!   [`DocumentTree::fill_form_text_field`], which keeps the field around
//!   whatever is typed (the generic insert path would land text outside a
//!   caret-atomic field);
//! - anything in a section whose `<w:formProt w:val="false"/>` turns
//!   protection off for it.
//!
//! A content control whose `<w:lock>` locks its content, and a form field
//! whose `<w:ffData>` disables it, are not editable.

use serde::{Deserialize, Serialize};

use crate::{
    Block, BlockPath, BodyFragment, BodyPassthrough, DocumentTree, LogicalPos, MarkerRole,
    Paragraph, PathStep, SectionProps, SourceMarkup, TextEdit,
};

/// `w:edit` — the editing restriction a document asks for (ECMA-376
/// Part 1 §17.18.24 `ST_DocProtect`).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectionEdit {
    /// `none` — no editing restriction.
    None,
    /// `readOnly` — no edits at all.
    ReadOnly,
    /// `comments` — comments only.
    Comments,
    /// `trackedChanges` — every edit is a tracked change; review mode
    /// cannot be turned off.
    TrackedChanges,
    /// `forms` — only form-field content.
    Forms,
}

impl ProtectionEdit {
    /// Parse a `w:edit` value; `None` for anything outside the enumeration.
    pub fn from_ooxml(v: &str) -> Option<Self> {
        Some(match v {
            "none" => Self::None,
            "readOnly" => Self::ReadOnly,
            "comments" => Self::Comments,
            "trackedChanges" => Self::TrackedChanges,
            "forms" => Self::Forms,
            _ => return Option::None,
        })
    }

    /// The `w:edit` spelling.
    pub fn as_ooxml(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ReadOnly => "readOnly",
            Self::Comments => "comments",
            Self::TrackedChanges => "trackedChanges",
            Self::Forms => "forms",
        }
    }
}

/// Issue #345 — `<w:documentProtection>`, lifted from `word/settings.xml`.
/// Read-only: the part's bytes pass through verbatim, nothing writes this
/// back. Both attribute families are read — the transitional
/// `w:hash` / `w:salt` / `w:cryptSpinCount` / `w:cryptAlgorithmSid` Word
/// writes and the strict `w:hashValue` / `w:saltValue` / `w:spinCount` /
/// `w:algorithmName`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(default)]
pub struct DocumentProtection {
    /// `w:edit`; `None` when absent or not a spec value.
    pub edit: Option<ProtectionEdit>,
    /// `w:enforcement` (`ST_OnOff`; absent = off).
    pub enforcement: bool,
    /// `w:hash` / `w:hashValue` — base64, verbatim.
    pub hash: Option<String>,
    /// `w:salt` / `w:saltValue` — base64, verbatim.
    pub salt: Option<String>,
    /// `w:cryptSpinCount` / `w:spinCount`.
    pub spin_count: Option<u32>,
    /// `w:algorithmName` (`SHA-512`, …), else the name of the legacy
    /// `w:cryptAlgorithmSid` (`4` → `SHA-1`, `14` → `SHA-512`, …; an
    /// unknown sid reads as `sid:<n>`).
    pub algorithm: Option<String>,
}

impl DocumentProtection {
    /// The restriction the engine enforces: `w:enforcement` on AND a
    /// restricting `w:edit` (`none`, absent or unknown restricts nothing).
    pub fn enforced_mode(&self) -> Option<ProtectionEdit> {
        match self.edit {
            Some(ProtectionEdit::None) | None => None,
            Some(mode) if self.enforcement => Some(mode),
            Some(_) => None,
        }
    }

    /// `true` when a password hash is recorded (lifting the protection
    /// needs the password — not offered by the engine).
    pub fn has_password(&self) -> bool {
        self.hash.as_deref().is_some_and(|h| !h.is_empty())
    }
}

/// The name of a legacy `w:cryptAlgorithmSid` (MS-OI29500 / ECMA-376
/// Part 4 transitional), `sid:<n>` for one outside the table.
pub fn crypt_algorithm_sid_name(sid: &str) -> String {
    match sid.trim() {
        "1" => "MD2".into(),
        "2" => "MD4".into(),
        "3" => "MD5".into(),
        "4" => "SHA-1".into(),
        "5" => "MAC".into(),
        "6" => "RIPEMD".into(),
        "7" => "RIPEMD-160".into(),
        "9" => "HMAC".into(),
        "12" => "SHA-256".into(),
        "13" => "SHA-384".into(),
        "14" => "SHA-512".into(),
        other => format!("sid:{other}"),
    }
}

/// The shape of an edit a forms-protection check is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormEdit {
    /// Remove `[start, end)` (possibly empty), then — when `inserts` —
    /// insert text at `start`.
    Text { inserts: bool },
    /// Remove `[start, end)`, then break the paragraph at `start`.
    Break,
}

/// The editable form content an edit falls in
/// ([`DocumentTree::form_region_for_edit`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormRegion {
    /// A section whose `<w:formProt>` is off: not protected at all.
    UnprotectedSection,
    /// Inside a block-level content control (both ends in the same one).
    BlockSdt,
    /// Inside the run-level content control whose opener sits at byte
    /// `open` and closer at byte `close` of the paragraph.
    RunSdt { open: u32, close: u32 },
    /// The result of the legacy text form field `field` (an index into
    /// the paragraph's `fields`).
    TextField { field: usize },
}

/// Word's result for an empty text form field: five EN SPACEs.
pub const FORM_TEXT_PLACEHOLDER: &str = "\u{2002}\u{2002}\u{2002}\u{2002}\u{2002}";

/// `true` for a `w:val`-style off value.
fn is_off(v: &[u8]) -> bool {
    matches!(v, b"0" | b"false" | b"off")
}

/// Find the first start tag named `tag` (e.g. `<w:lock`) inside `xml`:
/// `None` when there is none, else `Some(value)` of its attribute `attr`
/// (e.g. `w:val`) — `Some(None)` when the tag lacks the attribute.
fn attr_of_first<'a>(xml: &'a [u8], tag: &[u8], attr: &[u8]) -> Option<Option<&'a [u8]>> {
    let at = xml
        .windows(tag.len())
        .enumerate()
        .find(|(i, w)| {
            *w == tag
                && xml
                    .get(i + tag.len())
                    .is_some_and(|c| matches!(c, b' ' | b'/' | b'>' | b'\t' | b'\r' | b'\n'))
        })
        .map(|(i, _)| i)?;
    let rest = &xml[at + tag.len()..];
    let end = rest.iter().position(|&c| c == b'>').unwrap_or(rest.len());
    let tag_bytes = &rest[..end];
    let mut needle = attr.to_vec();
    needle.extend_from_slice(b"=\"");
    let value = tag_bytes
        .windows(needle.len())
        .position(|w| w == needle.as_slice())
        .map(|p| {
            let v = &tag_bytes[p + needle.len()..];
            &v[..v.iter().position(|&c| c == b'"').unwrap_or(v.len())]
        });
    Some(value)
}

/// `true` when an opener's bytes (`<w:sdt>…<w:sdtPr>…</w:sdtPr>
/// <w:sdtContent>`) start a content control whose content may be edited
/// (no `<w:lock w:val="contentLocked|sdtContentLocked"/>`).
fn is_editable_sdt(open_xml: &[u8]) -> bool {
    let is_sdt = open_xml.starts_with(b"<w:sdt")
        && open_xml
            .get(6)
            .is_some_and(|c| matches!(c, b'>' | b' ' | b'\t' | b'\r' | b'\n'));
    if !is_sdt {
        return false;
    }
    !matches!(
        attr_of_first(open_xml, b"<w:lock", b"w:val"),
        Some(Some(b"contentLocked" | b"sdtContentLocked"))
    )
}

/// `true` when a `<w:sectPr>`'s bytes turn forms protection off for the
/// section (`<w:formProt w:val="false"/>`; an absent `w:val` means on).
fn section_unprotected(props: &SectionProps) -> bool {
    props.source_xml.as_deref().is_some_and(
        |x| matches!(attr_of_first(x, b"<w:formProt", b"w:val"), Some(Some(v)) if is_off(v)),
    )
}

/// `true` when a legacy form field's source prologue disables it
/// (`<w:ffData><w:enabled w:val="0"/>`).
fn form_field_disabled(f: &crate::Field) -> bool {
    f.source.as_deref().is_some_and(
        |s| matches!(attr_of_first(&s.open, b"<w:enabled", b"w:val"), Some(Some(v)) if is_off(v)),
    )
}

/// Key of one open envelope: the container it lives in (a path prefix +
/// a level tag — body/cell blocks, rows, cells) and its id.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EnvelopeKey {
    container: Vec<PathStep>,
    level: u8,
    group: u32,
    id: u32,
}

/// Envelopes open around slot `target` of a container whose slots carry
/// `slots` passthrough markup (`Open` in a slot's `before`, `Close` in its
/// `after`). Only editable content controls are reported.
fn open_envelopes<'a>(
    slots: impl Iterator<Item = Option<&'a BodyPassthrough>>,
    target: usize,
    key: (&[PathStep], u8, u32),
    out: &mut Vec<EnvelopeKey>,
) {
    let mut stack: Vec<(u32, bool)> = Vec::new();
    for (k, slot) in slots.enumerate() {
        if k > target {
            break;
        }
        let Some(bx) = slot else {
            continue;
        };
        for frag in &bx.before {
            if let BodyFragment::Open { id, open_xml, .. } = frag {
                stack.push((*id, is_editable_sdt(open_xml)));
            }
        }
        if k == target {
            break;
        }
        for frag in &bx.after {
            if let BodyFragment::Close { id } = frag
                && let Some(pos) = stack.iter().rposition(|(i, _)| i == id)
            {
                stack.remove(pos);
            }
        }
    }
    out.extend(
        stack
            .into_iter()
            .filter(|(_, editable)| *editable)
            .map(|(id, _)| EnvelopeKey {
                container: key.0.to_vec(),
                level: key.1,
                group: key.2,
                id,
            }),
    );
}

/// Every editable block-level content control enclosing the block at
/// `path` (at any level: body / cell blocks, rows, cells).
fn enclosing_block_sdts(doc: &DocumentTree, path: &BlockPath) -> Vec<EnvelopeKey> {
    let mut out = Vec::new();
    let steps = &path.steps;
    let Some(PathStep::Block(first)) = steps.first() else {
        return out;
    };
    open_envelopes(
        doc.blocks.iter().map(Block::body_xml),
        *first as usize,
        (&[], 0, 0),
        &mut out,
    );
    let Some(mut block) = doc.blocks.get(*first as usize) else {
        return out;
    };
    let mut i = 1;
    while i + 1 < steps.len() {
        let (PathStep::Cell { row, col }, PathStep::Block(b)) = (steps[i], steps[i + 1]) else {
            break;
        };
        let Block::Table(t) = block else {
            break;
        };
        let prefix = &steps[..i];
        open_envelopes(
            t.rows.iter().map(|r| {
                r.source_markup
                    .as_deref()
                    .and_then(|m| m.body_xml.as_deref())
            }),
            row as usize,
            (prefix, 1, 0),
            &mut out,
        );
        let Some(r) = t.rows.get(row as usize) else {
            break;
        };
        open_envelopes(
            r.cells.iter().map(|c| {
                c.source_markup
                    .as_deref()
                    .and_then(|m| m.body_xml.as_deref())
            }),
            col as usize,
            (prefix, 2, row),
            &mut out,
        );
        let Some(cell) = r.cells.get(col as usize) else {
            break;
        };
        open_envelopes(
            cell.blocks.iter().map(Block::body_xml),
            b as usize,
            (&steps[..i + 1], 0, 0),
            &mut out,
        );
        let Some(next) = cell.blocks.get(b as usize) else {
            break;
        };
        block = next;
        i += 2;
    }
    out
}

/// The paragraph's editable run-level content controls, as
/// `(opener byte, closer byte)` — an opener whose closer was lost to a
/// split runs to the paragraph end (where the writer closes it). Empty
/// when the markup offsets are stale (never trust a stale position).
fn run_sdt_regions(p: &Paragraph) -> Vec<(u32, u32)> {
    let Some(m) = p.source_markup.as_deref() else {
        return Vec::new();
    };
    if !m.offsets_valid(p.text.len()) {
        return Vec::new();
    }
    let mut stack: Vec<(u32, u32, bool)> = Vec::new();
    let mut out = Vec::new();
    for mk in &m.markers {
        match &mk.role {
            MarkerRole::Open { id, .. } => stack.push((*id, mk.at, is_editable_sdt(&mk.xml))),
            MarkerRole::Close { id } => {
                if let Some(pos) = stack.iter().rposition(|(i, _, _)| i == id) {
                    let (_, at, editable) = stack.remove(pos);
                    if editable {
                        out.push((at, mk.at));
                    }
                }
            }
            _ => {}
        }
    }
    out.extend(
        stack
            .into_iter()
            .filter(|(_, _, editable)| *editable)
            .map(|(_, at, _)| (at, p.text.len() as u32)),
    );
    out
}

impl DocumentTree {
    /// Issue #345 — the editing restriction the engine enforces on this
    /// document, if any ([`DocumentProtection::enforced_mode`]).
    pub fn protection_mode(&self) -> Option<ProtectionEdit> {
        self.settings
            .protection
            .as_ref()
            .and_then(DocumentProtection::enforced_mode)
    }

    /// The section properties governing top-level block `index`.
    fn section_props_for_block(&self, index: u32) -> &SectionProps {
        self.blocks
            .iter()
            .skip(index as usize)
            .find_map(|b| match b {
                Block::Paragraph(p) => p.section_end.as_deref(),
                Block::Table(_) => None,
            })
            .unwrap_or(&self.body_section)
    }

    /// Issue #345 — `forms` protection: the editable form content the edit
    /// "remove `[start, end)`, then insert / break at `start`" is confined
    /// to, or `None` when it would touch protected content. Body paths
    /// only (story content is never form content here).
    ///
    /// The insertion rules follow the markup travel rules (issue #245):
    /// text inserted at a run-level control's CLOSER offset lands inside
    /// it, at its OPENER offset before it — so a pure insertion needs
    /// `open < at <= close`. A replacement starting exactly at the opener
    /// is reported as [`FormRegion::RunSdt`] all the same; the caller must
    /// insert before deleting so the text stays inside. A text form
    /// field's result accepts edits at either boundary
    /// ([`Self::fill_form_text_field`]). A paragraph break is only ever
    /// form content inside a block-level control.
    pub fn form_region_for_edit(
        &self,
        start: &LogicalPos,
        end: &LogicalPos,
        edit: FormEdit,
    ) -> Option<FormRegion> {
        let top = |p: &LogicalPos| match p.path.steps.first() {
            Some(PathStep::Block(i)) => Some(*i),
            _ => None,
        };
        let (ts, te) = (top(start)?, top(end)?);
        /* A section with `<w:formProt w:val="false"/>` is not protected:
        both ends in the same such section. */
        let section_s = self.section_props_for_block(ts);
        if section_unprotected(section_s)
            && std::ptr::eq(section_s, self.section_props_for_block(te))
        {
            return Some(FormRegion::UnprotectedSection);
        }
        /* Block-level content controls: one control enclosing both ends. */
        let around_start = enclosing_block_sdts(self, &start.path);
        if !around_start.is_empty() {
            let around_end = enclosing_block_sdts(self, &end.path);
            if around_start.iter().any(|k| around_end.contains(k)) {
                return Some(FormRegion::BlockSdt);
            }
        }
        let FormEdit::Text { inserts } = edit else {
            return None;
        };
        if start.path != end.path {
            return None;
        }
        let p = self.paragraph_at_path(&start.path)?;
        let (s, e) = (start.offset.min(end.offset), start.offset.max(end.offset));
        /* Legacy text form fields: an edit inside the result, boundaries
        included. */
        for (i, f) in p.fields.iter().enumerate() {
            if f.is_local()
                && f.keyword() == "FORMTEXT"
                && !form_field_disabled(f)
                && f.start <= s
                && e <= f.end
            {
                return Some(FormRegion::TextField { field: i });
            }
        }
        /* Run-level content controls. */
        run_sdt_regions(p)
            .into_iter()
            .find(|&(open, close)| {
                if s == e {
                    inserts && open < s && s <= close
                } else {
                    open <= s && e <= close
                }
            })
            .map(|(open, close)| FormRegion::RunSdt { open, close })
    }

    /// Issue #345 — fill in a legacy text form field: within the result of
    /// the `FORMTEXT` field `field` of the paragraph at `path`, replace the
    /// bytes `[s, e)` (absolute paragraph offsets inside the result,
    /// boundaries included) with `text`. The field overlay keeps covering
    /// the whole new result (the generic insert path would put text typed
    /// at a boundary OUTSIDE the caret-atomic field); a result that is only
    /// Word's placeholder (EN SPACEs) is replaced whole by what is typed,
    /// and an emptied result becomes the placeholder again so the field
    /// stays visible and reachable. Source markup and comment anchors
    /// follow the same splice (the restamp discipline, issues #250 /
    /// #252). Returns the new tree and the caret offset after the edit;
    /// `None` when `path` / `field` do not address a text form field.
    pub fn fill_form_text_field(
        &self,
        path: &BlockPath,
        field: usize,
        s: u32,
        e: u32,
        text: &str,
    ) -> Option<(DocumentTree, u32)> {
        let p = self.paragraph_at_path(path)?;
        let f = p.fields.get(field)?;
        if !(f.is_local() && f.keyword() == "FORMTEXT") {
            return None;
        }
        let (fs, fe) = (
            p.snap_offset(f.start),
            p.snap_offset(f.end).max(p.snap_offset(f.start)),
        );
        let result = p.text.get(fs as usize..fe as usize)?;
        let rel_s = (p.snap_offset(s.clamp(fs, fe)) - fs) as usize;
        let rel_e = (p.snap_offset(e.clamp(fs, fe)) - fs).max(rel_s as u32) as usize;
        let placeholder = !result.is_empty() && result.chars().all(|c| c == '\u{2002}');
        let (mut new_result, mut caret_rel) = if placeholder && !text.is_empty() {
            (text.to_string(), text.len())
        } else {
            let mut r = String::with_capacity(result.len() + text.len());
            r.push_str(&result[..rel_s]);
            r.push_str(text);
            r.push_str(&result[rel_e..]);
            (r, rel_s + text.len())
        };
        if new_result.is_empty() {
            new_result = FORM_TEXT_PLACEHOLDER.to_string();
            caret_rel = 0;
        }
        if new_result == result {
            return Some((self.clone(), fs + caret_rel as u32));
        }
        let mut out = self.clone();
        let rep_len = new_result.len() as u32;
        crate::mutate_paragraph_in_top(&mut out.blocks, path, |para| {
            let old_len = para.text.len() as u32;
            *para = para.with_spliced_range(fs, fe, &new_result);
            SourceMarkup::note_replace(&mut para.source_markup, old_len, fs, fe, rep_len);
        })?;
        out.remap_text_edit_record(
            path,
            TextEdit {
                at: fs,
                removed: fe - fs,
                inserted: rep_len,
            },
        );
        Some((out, fs + caret_rel as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enforced_mode_needs_enforcement_and_a_restricting_edit() {
        let mut p = DocumentProtection {
            edit: Some(ProtectionEdit::Forms),
            enforcement: true,
            ..Default::default()
        };
        assert_eq!(p.enforced_mode(), Some(ProtectionEdit::Forms));
        p.enforcement = false;
        assert_eq!(p.enforced_mode(), None);
        p.enforcement = true;
        p.edit = Some(ProtectionEdit::None);
        assert_eq!(p.enforced_mode(), None);
        p.edit = None;
        assert_eq!(p.enforced_mode(), None);
        for m in [
            ProtectionEdit::None,
            ProtectionEdit::ReadOnly,
            ProtectionEdit::Comments,
            ProtectionEdit::TrackedChanges,
            ProtectionEdit::Forms,
        ] {
            assert_eq!(ProtectionEdit::from_ooxml(m.as_ooxml()), Some(m));
        }
        assert_eq!(ProtectionEdit::from_ooxml("everything"), None);
        assert_eq!(crypt_algorithm_sid_name("14"), "SHA-512");
        assert_eq!(crypt_algorithm_sid_name("99"), "sid:99");
    }

    #[test]
    fn sdt_lock_and_form_prot_attributes() {
        assert!(is_editable_sdt(
            b"<w:sdt><w:sdtPr><w:alias w:val=\"x\"/></w:sdtPr><w:sdtContent>"
        ));
        assert!(!is_editable_sdt(
            b"<w:sdt><w:sdtPr><w:lock w:val=\"contentLocked\"/></w:sdtPr><w:sdtContent>"
        ));
        assert!(is_editable_sdt(
            b"<w:sdt><w:sdtPr><w:lock w:val=\"sdtLocked\"/></w:sdtPr><w:sdtContent>"
        ));
        assert!(!is_editable_sdt(b"<w:sdtContent>"));
        assert!(!is_editable_sdt(b"<w:customXml w:uri=\"u\">"));
        let off = SectionProps {
            source_xml: Some(b"<w:sectPr><w:formProt w:val=\"false\"/></w:sectPr>".to_vec()),
            ..Default::default()
        };
        assert!(section_unprotected(&off));
        let on = SectionProps {
            source_xml: Some(b"<w:sectPr><w:formProt/></w:sectPr>".to_vec()),
            ..Default::default()
        };
        assert!(!section_unprotected(&on));
        assert!(!section_unprotected(&SectionProps::default()));
    }

    /// A text form field takes typing at either boundary and keeps its
    /// overlay around the result; emptying it restores the placeholder.
    #[test]
    fn filling_a_text_form_field_keeps_the_field() {
        let mut doc = DocumentTree::from_text(&format!("Name: {FORM_TEXT_PLACEHOLDER}."));
        let start = "Name: ".len() as u32;
        let end = start + FORM_TEXT_PLACEHOLDER.len() as u32;
        let path = BlockPath::top(0);
        crate::mutate_paragraph_in_top(&mut doc.blocks, &path, |p| {
            p.fields.push(crate::Field {
                start,
                end,
                instruction: "FORMTEXT".into(),
                span: None,
                source: None,
            });
        });
        let at = |d: &DocumentTree, s: u32, e: u32| {
            d.form_region_for_edit(
                &LogicalPos {
                    path: path.clone(),
                    offset: s,
                },
                &LogicalPos {
                    path: path.clone(),
                    offset: e,
                },
                FormEdit::Text { inserts: true },
            )
        };
        assert_eq!(
            at(&doc, start, end),
            Some(FormRegion::TextField { field: 0 })
        );
        assert_eq!(at(&doc, 0, 0), None, "outside the field");
        assert_eq!(at(&doc, end + 1, end + 1), None);
        /* Typing over the placeholder replaces it whole. */
        let (d1, c1) = doc.fill_form_text_field(&path, 0, end, end, "A").unwrap();
        assert_eq!(d1.paragraph_text(0).unwrap(), "Name: A.");
        assert_eq!(c1, start + 1);
        let f = &d1.paragraph_at_path(&path).unwrap().fields[0];
        assert_eq!((f.start, f.end), (start, start + 1));
        /* Typing at the END boundary grows the result. */
        let (d2, c2) = d1.fill_form_text_field(&path, 0, c1, c1, "da").unwrap();
        assert_eq!(d2.paragraph_text(0).unwrap(), "Name: Ada.");
        assert_eq!(c2, start + 3);
        let f = &d2.paragraph_at_path(&path).unwrap().fields[0];
        assert_eq!((f.start, f.end), (start, start + 3));
        /* Deleting everything restores the placeholder. */
        let (d3, c3) = d2
            .fill_form_text_field(&path, 0, start, start + 3, "")
            .unwrap();
        assert_eq!(
            d3.paragraph_text(0).unwrap(),
            format!("Name: {FORM_TEXT_PLACEHOLDER}.")
        );
        assert_eq!(c3, start);
        let f = &d3.paragraph_at_path(&path).unwrap().fields[0];
        assert_eq!((f.start, f.end), (start, end));
    }
}
