//! `word/styles.xml` — read-only typed view (Phase 3).
//!
//! Parses three things:
//!
//! - `<w:docDefaults>` — the document-wide `<w:rPrDefault>` + `<w:pPrDefault>`
//!   that sit at the *bottom* of every style cascade.
//! - `<w:style w:type="paragraph">` and `<w:style w:type="character">` —
//!   each becomes a [`StyleDef`] with its `<w:basedOn>` parent id (if any),
//!   plus the `<w:pPr>` and `<w:rPr>` overrides it carries.
//! - `<w:style w:type="table">` / `<w:type="numbering">` — captured by id
//!   so a [`StyleDef::kind`] consumer can tell them apart, but their
//!   properties are not currently folded into the engine model.
//!
//! The pass-through invariant is preserved: `word/styles.xml` rides
//! [`crate::opc::archive::DocxArchive::other_entries`] verbatim. This module
//! never serialises.

use crate::error::DocxError;
use crate::schema::ct_ppr::apply_ppr;
use crate::schema::ct_rpr::{apply_rpr, attr_val};
use engine::{ParaProperties, SpanStyle};
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::HashMap;

/// Top of the cascade — every paragraph / run merges these first.
#[derive(Debug, Clone, Default)]
pub struct DocDefaults {
    pub para: ParaProperties,
    pub run: SpanStyle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleKind {
    Paragraph,
    Character,
    /// Parsed but not consumed by the resolver in Phase 3.
    Table,
    /// Parsed but not consumed by the resolver in Phase 3 (handled in Phase 4).
    Numbering,
}

/// One `<w:style>` entry.
#[derive(Debug, Clone)]
pub struct StyleDef {
    pub id: String,
    pub kind: StyleKind,
    /// `<w:basedOn w:val="…"/>` — the parent style id. The resolver folds
    /// the cascade root-first along this chain.
    pub based_on: Option<String>,
    pub para: ParaProperties,
    pub run: SpanStyle,
}

#[derive(Debug, Clone, Default)]
pub struct StyleTable {
    pub defaults: DocDefaults,
    /// `style id → definition`. Lookup feeds [`crate::style_resolver`].
    pub by_id: HashMap<String, StyleDef>,
}

/* ------------------------------------------------------------ helpers --- */

fn parse_kind(v: &str) -> Option<StyleKind> {
    match v.trim().to_ascii_lowercase().as_str() {
        "paragraph" => Some(StyleKind::Paragraph),
        "character" => Some(StyleKind::Character),
        "table" => Some(StyleKind::Table),
        "numbering" => Some(StyleKind::Numbering),
        _ => None,
    }
}

/// Single-pass parser. We do not split helpers across borrowed `buf` — keeping
/// everything in one driver lets quick-xml's event references coexist with
/// our accumulators without the lifetime gymnastics that splitting `read_*`
/// helpers across nested `&mut buf` borrows would force.
pub fn parse_styles_xml(xml: &[u8]) -> Result<StyleTable, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut table = StyleTable::default();

    // Stack of open elements — push on Start, pop on End. Lets us decide
    // whether the current child belongs to <w:rPr>, <w:pPr>, etc., without
    // borrowed-event lifetime issues.
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut cur_style: Option<StyleScratch> = None;
    let mut in_doc_defaults = false;
    // True when the current `<w:rPr>` is a paragraph-mark `<w:pPr>/<w:rPr>` —
    // there's a `<w:pPr>` somewhere above on the stack. Phase 4+ may model
    // paragraph-mark run formatting; today we skip it.
    let pmark_rpr = |stack: &[Vec<u8>]| -> bool {
        stack.iter().rev().skip(1).any(|n| n.as_slice() == b"w:pPr")
    };
    // True when the current event is inside `<w:rPr>`.
    let in_rpr = |stack: &[Vec<u8>]| -> bool {
        stack
            .last()
            .map(|n| n.as_slice() == b"w:rPr")
            .unwrap_or(false)
    };
    // True when the current event is inside `<w:pPr>` (but not its nested
    // `<w:rPr>`).
    let in_ppr = |stack: &[Vec<u8>]| -> bool {
        stack
            .last()
            .map(|n| n.as_slice() == b"w:pPr")
            .unwrap_or(false)
    };

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let name = e.name().as_ref().to_owned();
                match name.as_slice() {
                    b"w:docDefaults" => in_doc_defaults = true,
                    b"w:style" => {
                        cur_style = Some(StyleScratch::from_start(&e));
                    }
                    b"w:basedOn" => {
                        if let Some(s) = cur_style.as_mut() {
                            s.based_on = attr_val(&e, b"w:val");
                        }
                    }
                    n if in_rpr(&stack) && !pmark_rpr(&stack) => {
                        apply_to_rpr(n, &e, &mut table, &mut cur_style, in_doc_defaults);
                    }
                    n if in_ppr(&stack) => {
                        apply_to_ppr(n, &e, &mut table, &mut cur_style, in_doc_defaults);
                    }
                    _ => {}
                }
                stack.push(name);
            }
            Event::Empty(e) => {
                let name = e.name().as_ref().to_owned();
                match name.as_slice() {
                    b"w:basedOn" => {
                        if let Some(s) = cur_style.as_mut() {
                            s.based_on = attr_val(&e, b"w:val");
                        }
                    }
                    n if in_rpr(&stack) && !pmark_rpr(&stack) => {
                        apply_to_rpr(n, &e, &mut table, &mut cur_style, in_doc_defaults);
                    }
                    n if in_ppr(&stack) => {
                        apply_to_ppr(n, &e, &mut table, &mut cur_style, in_doc_defaults);
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                let name = e.name().as_ref().to_owned();
                match name.as_slice() {
                    b"w:docDefaults" => in_doc_defaults = false,
                    b"w:style" => {
                        if let Some(s) = cur_style.take()
                            && let Some(def) = s.into_def()
                        {
                            table.by_id.insert(def.id.clone(), def);
                        }
                    }
                    _ => {}
                }
                if stack.last().map(|n| n.as_slice()) == Some(name.as_slice()) {
                    stack.pop();
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(table)
}

#[derive(Debug)]
struct StyleScratch {
    id: Option<String>,
    kind: Option<StyleKind>,
    based_on: Option<String>,
    para: ParaProperties,
    run: SpanStyle,
}

impl StyleScratch {
    fn from_start(e: &BytesStart) -> Self {
        Self {
            id: attr_val(e, b"w:styleId"),
            kind: attr_val(e, b"w:type").as_deref().and_then(parse_kind),
            based_on: None,
            para: ParaProperties::default(),
            run: SpanStyle::default(),
        }
    }
    fn into_def(self) -> Option<StyleDef> {
        let id = self.id?;
        let kind = self.kind?;
        Some(StyleDef {
            id,
            kind,
            based_on: self.based_on,
            para: self.para,
            run: self.run,
        })
    }
}

fn apply_to_rpr(
    name: &[u8],
    e: &BytesStart,
    table: &mut StyleTable,
    cur_style: &mut Option<StyleScratch>,
    in_doc_defaults: bool,
) {
    if in_doc_defaults && cur_style.is_none() {
        apply_rpr(name, e, &mut table.defaults.run);
    } else if let Some(s) = cur_style.as_mut() {
        apply_rpr(name, e, &mut s.run);
    }
}

fn apply_to_ppr(
    name: &[u8],
    e: &BytesStart,
    table: &mut StyleTable,
    cur_style: &mut Option<StyleScratch>,
    in_doc_defaults: bool,
) {
    if in_doc_defaults && cur_style.is_none() {
        apply_ppr(name, e, &mut table.defaults.para);
    } else if let Some(s) = cur_style.as_mut() {
        apply_ppr(name, e, &mut s.para);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::Alignment;

    const SAMPLE: &[u8] = br#"<?xml version="1.0"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:docDefaults>
    <w:rPrDefault><w:rPr><w:rFonts w:ascii="Liberation Sans"/></w:rPr></w:rPrDefault>
    <w:pPrDefault><w:pPr><w:jc w:val="start"/></w:pPr></w:pPrDefault>
  </w:docDefaults>
  <w:style w:type="paragraph" w:styleId="BaseStyle">
    <w:name w:val="Base Style"/>
    <w:rPr><w:b/></w:rPr>
  </w:style>
  <w:style w:type="paragraph" w:styleId="ChildStyle">
    <w:name w:val="Child Style"/>
    <w:basedOn w:val="BaseStyle"/>
    <w:rPr><w:i/></w:rPr>
    <w:pPr><w:jc w:val="center"/></w:pPr>
  </w:style>
  <w:style w:type="character" w:styleId="Emphasis">
    <w:rPr><w:i/></w:rPr>
  </w:style>
</w:styles>"#;

    #[test]
    fn parses_doc_defaults() {
        let t = parse_styles_xml(SAMPLE).expect("parse");
        assert_eq!(t.defaults.para.alignment, Some(Alignment::Start));
        assert!(t.defaults.run.font_family.is_some());
    }

    #[test]
    fn parses_styles_and_kinds() {
        let t = parse_styles_xml(SAMPLE).expect("parse");
        assert_eq!(t.by_id.len(), 3);
        let base = t.by_id.get("BaseStyle").unwrap();
        assert_eq!(base.kind, StyleKind::Paragraph);
        assert_eq!(base.based_on, None);
        assert_eq!(base.run.bold, Some(true));

        let child = t.by_id.get("ChildStyle").unwrap();
        assert_eq!(child.based_on.as_deref(), Some("BaseStyle"));
        assert_eq!(child.run.italic, Some(true));
        assert_eq!(child.para.alignment, Some(Alignment::Center));

        let emphasis = t.by_id.get("Emphasis").unwrap();
        assert_eq!(emphasis.kind, StyleKind::Character);
    }
}
