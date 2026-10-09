//! Issue #371 — `word/styles.xml` edited like `document.xml`.
//!
//! `ModifyStyle` used to regenerate the whole part from the engine's
//! paragraph-style table, dropping every character / table / numbering
//! style, `<w:latentStyles>`, unmodeled `<w:docDefaults>` children and
//! every unmodeled child of the styles it kept. Now the SOURCE part is
//! patched: each top-level `<w:style>` is located by its `w:styleId`, a
//! paragraph style whose model (name, `basedOn`, `next`, pPr, rPr) still
//! equals what the source element produced keeps its bytes, an edited one
//! is spliced child by child ([`super::ppr_splice::splice_style`]), a new
//! one is appended before `</w:styles>`, and `<w:docDefaults>` is
//! regenerated only when the defaults changed. Every other byte — the
//! prolog, the root, `<w:latentStyles>`, the other styles — is copied.

use super::ppr_splice::{StyleParts, splice_style};
use super::{
    emit_doc_defaults, emit_paragraph_style, emit_ppr, emit_rpr, ppr_children, rpr_children,
};
use crate::parts::styles::{StyleDef, StyleKind, parse_styles_xml};
use engine::{DocumentTree, ParagraphStyle};
use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// One top-level `<w:style>` of the source part.
struct StyleSlot {
    id: Option<String>,
    paragraph: bool,
    start: usize,
    end: usize,
}

/// Where things sit in the source part (byte offsets into it).
struct StylesLayout {
    /// End of the `<w:styles …>` start tag.
    root_open_end: usize,
    doc_defaults: Option<(usize, usize)>,
    styles: Vec<StyleSlot>,
    /// Start of `</w:styles>`.
    close: usize,
}

/// Locate the root's children. `None` for a part the scan cannot follow
/// (the caller regenerates the part as before).
fn scan(xml: &[u8]) -> Option<StylesLayout> {
    /* quick-xml strips a BOM without counting it in its positions. */
    let bom = if xml.starts_with(&[0xEF, 0xBB, 0xBF]) {
        3
    } else {
        0
    };
    let mut reader = Reader::from_reader(&xml[bom..]);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut depth = 0usize;
    let mut pos = 0usize;
    let mut root_open_end = None;
    let mut doc_defaults = None;
    let mut styles = Vec::new();
    let mut open: Option<(usize, Vec<u8>, Option<String>, bool)> = None;
    let mut close = None;
    loop {
        let ev = reader.read_event_into(&mut buf).ok()?;
        let end = reader.buffer_position() as usize;
        let attr =
            |e: &quick_xml::events::BytesStart, k: &[u8]| crate::schema::ct_rpr::attr_val(e, k);
        match ev {
            Event::Eof => break,
            Event::Start(e) => {
                match depth {
                    /* Only the canonical spelling is patched (the reader
                    re-prefixes any other, issue #394). */
                    0 if e.name().as_ref() != b"w:styles" => return None,
                    0 => root_open_end = Some(bom + end),
                    1 => {
                        let name = e.name().as_ref().to_vec();
                        let paragraph = attr(&e, b"w:type")
                            .is_none_or(|t| t.trim().eq_ignore_ascii_case("paragraph"));
                        open = Some((bom + pos, name, attr(&e, b"w:styleId"), paragraph));
                    }
                    _ => {}
                }
                depth += 1;
            }
            Event::Empty(e) if depth == 1 && e.name().as_ref() == b"w:style" => {
                styles.push(StyleSlot {
                    id: attr(&e, b"w:styleId"),
                    paragraph: false,
                    start: bom + pos,
                    end: bom + end,
                });
            }
            Event::End(e) => {
                depth = depth.checked_sub(1)?;
                match depth {
                    0 => close = Some(bom + pos),
                    1 => {
                        let (start, name, id, paragraph) = open.take()?;
                        if name.as_slice() != e.name().as_ref() {
                            return None;
                        }
                        match name.as_slice() {
                            b"w:docDefaults" => doc_defaults = Some((start, bom + end)),
                            b"w:style" => styles.push(StyleSlot {
                                id,
                                paragraph,
                                start,
                                end: bom + end,
                            }),
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        pos = end;
        buf.clear();
    }
    Some(StylesLayout {
        root_open_end: root_open_end?,
        doc_defaults,
        styles,
        close: close?,
    })
}

/// The source style still describes the live one: everything the writer
/// would emit for it is equal.
fn unchanged(rec: &StyleDef, live: &ParagraphStyle) -> bool {
    rec.name.as_deref().unwrap_or_default() == live.name
        && rec.based_on == live.based_on
        && rec.next == live.next
        && rec.para == live.para
        && rec.run == live.run
}

fn parts<'a>(
    name: Option<&'a str>,
    based_on: Option<&'a str>,
    next: Option<&'a str>,
    para: &engine::ParaProperties,
    run: &engine::SpanStyle,
) -> StyleParts<'a> {
    let mut ppr = String::new();
    emit_ppr(para, None, None, None, None, &mut ppr);
    let mut rpr = String::new();
    emit_rpr(run, &mut rpr);
    StyleParts {
        name,
        based_on,
        next,
        ppr,
        ppr_children: ppr_children(para, None, None, None).items,
        rpr,
        rpr_children: rpr_children(run).items,
    }
}

/// Issue #371 — `source` (the package's `styles.xml`) with only what
/// `doc`'s style model changed re-written. `None` when the part cannot be
/// scanned or parsed (the caller regenerates it from the model).
pub(super) fn patch_styles_xml(source: &[u8], doc: &DocumentTree) -> Option<Vec<u8>> {
    let table = parse_styles_xml(source).ok()?;
    let layout = scan(source)?;
    let text = std::str::from_utf8(source).ok()?;
    let mut out = String::with_capacity(source.len() + 256);
    let mut copied = 0usize;
    let replace = |out: &mut String, copied: &mut usize, start: usize, end: usize, with: &str| {
        out.push_str(&text[*copied..start]);
        out.push_str(with);
        *copied = end;
    };

    /* `<w:docDefaults>` — regenerated only when the defaults changed. */
    if table.defaults.para != doc.style_defaults || table.defaults.run != doc.style_run_defaults {
        let mut defaults = String::new();
        emit_doc_defaults(doc, &mut defaults);
        match layout.doc_defaults {
            Some((s, e)) => replace(&mut out, &mut copied, s, e, &defaults),
            None => replace(
                &mut out,
                &mut copied,
                layout.root_open_end,
                layout.root_open_end,
                &defaults,
            ),
        }
    }

    /* The paragraph styles the model holds, each at its LAST source
    occurrence (the one the parsed table describes). */
    let last_of = |id: &str| {
        layout
            .styles
            .iter()
            .rposition(|s| s.id.as_deref() == Some(id))
    };
    for (i, slot) in layout.styles.iter().enumerate() {
        let Some(id) = slot.id.as_deref() else {
            continue;
        };
        let (Some(live), Some(rec)) = (doc.styles.get(id), table.by_id.get(id)) else {
            continue;
        };
        if !slot.paragraph || rec.kind != StyleKind::Paragraph || last_of(id) != Some(i) {
            continue;
        }
        if unchanged(rec, live) {
            continue;
        }
        let rec_parts = parts(
            rec.name.as_deref(),
            rec.based_on.as_deref(),
            rec.next.as_deref(),
            &rec.para,
            &rec.run,
        );
        let live_parts = parts(
            Some(live.name.as_str()),
            live.based_on.as_deref(),
            live.next.as_deref(),
            &live.para,
            &live.run,
        );
        let element = &text[slot.start..slot.end];
        let spliced = splice_style(element, &rec_parts, &live_parts).unwrap_or_else(|| {
            let mut s = String::new();
            emit_paragraph_style(id, live, &mut s);
            s
        });
        replace(&mut out, &mut copied, slot.start, slot.end, &spliced);
    }

    /* Styles the source does not have (engine-authored, e.g. the TOC
    levels a regenerated TOC needs), before `</w:styles>`. */
    let mut fresh: Vec<&String> = doc
        .styles
        .keys()
        .filter(|id| {
            !layout
                .styles
                .iter()
                .any(|s| s.id.as_deref() == Some(id.as_str()))
        })
        .collect();
    fresh.sort();
    let mut appended = String::new();
    for id in fresh {
        emit_paragraph_style(id, &doc.styles[id], &mut appended);
    }
    replace(&mut out, &mut copied, layout.close, layout.close, &appended);
    out.push_str(&text[copied..]);
    Some(out.into_bytes())
}
