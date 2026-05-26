//! `word/numbering.xml` — read-only typed view (Phase 4).
//!
//! Parses two indirection layers OOXML lists rely on:
//!
//! - `<w:abstractNum w:abstractNumId="N">` — the *template* list definition.
//!   Holds one `<w:lvl w:ilvl="K">` per level (0..=8 typically), each with:
//!   `<w:start>` (initial counter), `<w:numFmt>` (decimal / upperRoman /
//!   bullet / etc.), `<w:lvlText>` (the marker template with `%N`
//!   placeholders), `<w:lvlJc>` (marker alignment), `<w:lvlRestart>` (the
//!   parent-level threshold that resets this level's counter), and an
//!   optional `<w:pPr>` whose `<w:ind>` drives the marker gutter geometry.
//!
//! - `<w:num w:numId="M">` — an *instance* binding a `numId` (referenced by
//!   paragraphs via `<w:numPr>/<w:numId>`) to one `abstractNumId`, plus
//!   optional `<w:lvlOverride>`s that shadow individual levels or restart
//!   counters mid-document.
//!
//! Pass-through invariant holds: `word/numbering.xml` rides
//! [`crate::opc::archive::DocxArchive::other_entries`] verbatim. This module
//! never serialises.

use crate::error::DocxError;
use crate::schema::ct_rpr::attr_val;
use engine::Indent;
use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::HashMap;

/// `<w:numFmt w:val="…"/>`. Phase 4 ships the common formats; `Other`
/// captures the value as-is so the round-trip preserves it (the marker
/// resolver renders `Other` as the literal `lvlText` template).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NumFmt {
    Decimal,
    DecimalZero,
    LowerLetter,
    UpperLetter,
    LowerRoman,
    UpperRoman,
    Bullet,
    None,
    Other(String),
}

impl NumFmt {
    fn parse(v: &str) -> NumFmt {
        match v.trim() {
            "decimal" => NumFmt::Decimal,
            "decimalZero" => NumFmt::DecimalZero,
            "lowerLetter" => NumFmt::LowerLetter,
            "upperLetter" => NumFmt::UpperLetter,
            "lowerRoman" => NumFmt::LowerRoman,
            "upperRoman" => NumFmt::UpperRoman,
            "bullet" => NumFmt::Bullet,
            "none" => NumFmt::None,
            o => NumFmt::Other(o.to_owned()),
        }
    }
}

/// One level inside an `<w:abstractNum>`. Defaults match an empty `<w:lvl>` —
/// the resolver still has to substitute `%N` patterns even on partially-set
/// levels Word emits without explicit numFmt / start.
#[derive(Debug, Clone)]
pub struct LvlDef {
    pub ilvl: u8,
    /// `<w:start w:val="…"/>` — initial counter for this level. Default 1.
    pub start: i32,
    pub num_fmt: NumFmt,
    /// `<w:lvlText w:val="…"/>` — marker template with `%1`/`%2`/... refs
    /// that the resolver substitutes with formatted counters at each level.
    pub lvl_text: String,
    /// `<w:lvlRestart w:val="N"/>` — restart this level whenever the
    /// counter at *parent* level `< N` increments. `None` ⇒ Word's
    /// default: restart whenever any prior level increments. `Some(0)` ⇒
    /// never restart (continuous numbering across resets).
    pub lvl_restart: Option<u8>,
    /// `<w:pPr>/<w:ind>` — marker gutter geometry. The resolver hands
    /// these to layout to position the marker; absent ⇒ defaults from
    /// the paragraph's own pPr.
    pub indent: Indent,
}

impl Default for LvlDef {
    fn default() -> Self {
        Self {
            ilvl: 0,
            start: 1,
            num_fmt: NumFmt::Decimal,
            lvl_text: String::new(),
            lvl_restart: None,
            indent: Indent::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AbstractNum {
    pub id: u32,
    pub levels: Vec<LvlDef>,
}

impl AbstractNum {
    pub fn level(&self, ilvl: u8) -> Option<&LvlDef> {
        self.levels.iter().find(|l| l.ilvl == ilvl)
    }
}

/// `<w:lvlOverride>` — replaces a single level's `<w:lvl>` and/or restarts
/// the counter via `<w:startOverride>`.
#[derive(Debug, Clone)]
pub struct LvlOverride {
    pub ilvl: u8,
    pub start_override: Option<i32>,
    pub lvl: Option<LvlDef>,
}

/// `<w:num w:numId="M">` — paragraph-referenced instance.
#[derive(Debug, Clone)]
pub struct NumInstance {
    pub num_id: u32,
    pub abstract_num_id: u32,
    pub overrides: Vec<LvlOverride>,
}

impl NumInstance {
    pub fn lvl_override(&self, ilvl: u8) -> Option<&LvlOverride> {
        self.overrides.iter().find(|o| o.ilvl == ilvl)
    }
}

#[derive(Debug, Clone, Default)]
pub struct NumberingDefinitions {
    pub abstract_nums: HashMap<u32, AbstractNum>,
    pub num_instances: HashMap<u32, NumInstance>,
}

impl NumberingDefinitions {
    /// Resolve `(num_id, ilvl)` → the effective `LvlDef`. Follows the
    /// `numId → abstractNumId → ilvl` indirection, with `<w:lvlOverride>`
    /// winning over the abstract definition.
    pub fn level_for(&self, num_id: u32, ilvl: u8) -> Option<&LvlDef> {
        let num = self.num_instances.get(&num_id)?;
        if let Some(ovr) = num.lvl_override(ilvl)
            && let Some(lvl) = &ovr.lvl
        {
            return Some(lvl);
        }
        let abs = self.abstract_nums.get(&num.abstract_num_id)?;
        abs.level(ilvl)
    }

    /// Effective `<w:start>` for `(num_id, ilvl)`: `<w:startOverride>` if
    /// present, else the level's `<w:start>`.
    pub fn start_for(&self, num_id: u32, ilvl: u8) -> Option<i32> {
        let num = self.num_instances.get(&num_id)?;
        if let Some(ovr) = num.lvl_override(ilvl)
            && let Some(s) = ovr.start_override
        {
            return Some(s);
        }
        self.level_for(num_id, ilvl).map(|l| l.start)
    }
}

/* ---------------------------------------------------------------- parser --- */

#[derive(Debug, Default)]
struct LvlScratch {
    ilvl: Option<u8>,
    start: Option<i32>,
    num_fmt: Option<NumFmt>,
    lvl_text: Option<String>,
    lvl_restart: Option<u8>,
    indent: Indent,
}

impl LvlScratch {
    fn from_start(e: &BytesStart) -> Self {
        Self {
            ilvl: attr_val(e, b"w:ilvl").and_then(|v| v.parse().ok()),
            ..Default::default()
        }
    }
    fn into_def(self) -> Option<LvlDef> {
        let ilvl = self.ilvl?;
        let mut d = LvlDef {
            ilvl,
            ..LvlDef::default()
        };
        if let Some(v) = self.start {
            d.start = v;
        }
        if let Some(v) = self.num_fmt {
            d.num_fmt = v;
        }
        if let Some(v) = self.lvl_text {
            d.lvl_text = v;
        }
        d.lvl_restart = self.lvl_restart;
        d.indent = self.indent;
        Some(d)
    }
}

/// Parse `word/numbering.xml` into a [`NumberingDefinitions`].
pub fn parse_numbering_xml(xml: &[u8]) -> Result<NumberingDefinitions, DocxError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out = NumberingDefinitions::default();

    /* Element stack so child elements know their context without juggling
    borrow lifetimes on `quick_xml`'s event references. */
    let mut stack: Vec<Vec<u8>> = Vec::new();

    let mut cur_abstract: Option<AbstractNum> = None;
    let mut cur_num: Option<NumInstance> = None;
    let mut cur_lvl: Option<LvlScratch> = None;
    let mut cur_lvl_override: Option<LvlOverride> = None;
    /* `cur_lvl_in_override` distinguishes the `<w:lvl>` nested *inside* a
    `<w:lvlOverride>` (overrides that level) from the top-level
    `<w:lvl>` children of an `<w:abstractNum>`. */
    let mut cur_lvl_in_override = false;
    /* Are we inside the `<w:pPr>` of a `<w:lvl>`? Only `<w:ind>` is
    consumed there. */
    let mut in_lvl_ppr = false;
    let mut in_lvl_rpr = false;

    let handle_lvl_child = |stack: &[Vec<u8>],
                            cur_lvl: &mut Option<LvlScratch>,
                            in_lvl_rpr: bool,
                            in_lvl_ppr: bool,
                            name: &[u8],
                            e: &BytesStart| {
        let Some(scratch) = cur_lvl.as_mut() else {
            return;
        };
        if in_lvl_rpr {
            return; // run-mark formatting (Phase 4 ignores)
        }
        if in_lvl_ppr {
            if name == b"w:ind" {
                apply_lvl_ind(e, &mut scratch.indent);
            }
            return;
        }
        let _ = stack;
        match name {
            b"w:start" => {
                scratch.start = attr_val(e, b"w:val").and_then(|v| v.parse().ok());
            }
            b"w:numFmt" => {
                scratch.num_fmt = attr_val(e, b"w:val").map(|v| NumFmt::parse(&v));
            }
            b"w:lvlText" => {
                scratch.lvl_text = attr_val(e, b"w:val");
            }
            b"w:lvlRestart" => {
                scratch.lvl_restart = attr_val(e, b"w:val").and_then(|v| v.parse().ok());
            }
            _ => {}
        }
    };

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                let name = e.name().as_ref().to_owned();
                match name.as_slice() {
                    b"w:abstractNum" => {
                        cur_abstract = Some(AbstractNum {
                            id: attr_val(&e, b"w:abstractNumId")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0),
                            levels: Vec::new(),
                        });
                    }
                    b"w:num" => {
                        cur_num = Some(NumInstance {
                            num_id: attr_val(&e, b"w:numId")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0),
                            abstract_num_id: 0,
                            overrides: Vec::new(),
                        });
                    }
                    b"w:lvl" => {
                        cur_lvl_in_override = cur_lvl_override.is_some();
                        cur_lvl = Some(LvlScratch::from_start(&e));
                    }
                    b"w:lvlOverride" => {
                        cur_lvl_override = Some(LvlOverride {
                            ilvl: attr_val(&e, b"w:ilvl")
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(0),
                            start_override: None,
                            lvl: None,
                        });
                    }
                    b"w:pPr" if cur_lvl.is_some() => in_lvl_ppr = true,
                    b"w:rPr" if cur_lvl.is_some() => in_lvl_rpr = true,
                    n if cur_lvl.is_some() => {
                        handle_lvl_child(&stack, &mut cur_lvl, in_lvl_rpr, in_lvl_ppr, n, &e);
                    }
                    _ => {}
                }
                stack.push(name);
            }
            Event::Empty(e) => {
                let name = e.name().as_ref().to_owned();
                match name.as_slice() {
                    b"w:abstractNumId" if cur_num.is_some() => {
                        if let Some(num) = cur_num.as_mut()
                            && let Some(v) = attr_val(&e, b"w:val").and_then(|v| v.parse().ok())
                        {
                            num.abstract_num_id = v;
                        }
                    }
                    b"w:startOverride" if cur_lvl_override.is_some() => {
                        if let Some(ovr) = cur_lvl_override.as_mut() {
                            ovr.start_override =
                                attr_val(&e, b"w:val").and_then(|v| v.parse().ok());
                        }
                    }
                    n if cur_lvl.is_some() => {
                        handle_lvl_child(&stack, &mut cur_lvl, in_lvl_rpr, in_lvl_ppr, n, &e);
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                let name = e.name().as_ref().to_owned();
                match name.as_slice() {
                    b"w:abstractNum" => {
                        if let Some(a) = cur_abstract.take() {
                            out.abstract_nums.insert(a.id, a);
                        }
                    }
                    b"w:num" => {
                        if let Some(n) = cur_num.take() {
                            out.num_instances.insert(n.num_id, n);
                        }
                    }
                    b"w:lvl" => {
                        if let Some(lvl) = cur_lvl.take().and_then(|s| s.into_def()) {
                            if cur_lvl_in_override {
                                if let Some(ovr) = cur_lvl_override.as_mut() {
                                    ovr.lvl = Some(lvl);
                                }
                            } else if let Some(a) = cur_abstract.as_mut() {
                                a.levels.push(lvl);
                            }
                        }
                        cur_lvl_in_override = false;
                    }
                    b"w:lvlOverride" => {
                        if let Some(ovr) = cur_lvl_override.take()
                            && let Some(num) = cur_num.as_mut()
                        {
                            num.overrides.push(ovr);
                        }
                    }
                    b"w:pPr" => in_lvl_ppr = false,
                    b"w:rPr" => in_lvl_rpr = false,
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
    Ok(out)
}

/// Parse `<w:ind w:start|left|end|right|firstLine|hanging>` exactly the same
/// way `schema::ct_ppr` does. Inlined here so this module is free of the
/// `apply_ppr` ParaProperties dependence — numbering only cares about
/// indentation.
fn apply_lvl_ind(e: &BytesStart, ind: &mut Indent) {
    let twips = |k: &[u8]| -> Option<i32> { attr_val(e, k).and_then(|v| v.trim().parse().ok()) };
    if let Some(v) = twips(b"w:start").or_else(|| twips(b"w:left")) {
        ind.start_twips = v;
    }
    if let Some(v) = twips(b"w:end").or_else(|| twips(b"w:right")) {
        ind.end_twips = v;
    }
    if let Some(v) = twips(b"w:hanging") {
        ind.hanging_twips = v;
        ind.first_line_twips = 0;
    } else if let Some(v) = twips(b"w:firstLine") {
        ind.first_line_twips = v;
        ind.hanging_twips = 0;
    }
}

/// Sprint 13 (#12) — serialize an engine `NumberingDefinitions` into
/// `word/numbering.xml`. Only called by the writer when
/// `doc.numbering.dirty` is `true`; otherwise the OPC passthrough
/// preserves the original bytes verbatim.
///
/// Output is deterministic — abstractNums and num instances sort by
/// id so consecutive saves of the same document produce identical
/// bytes (matters for the round-trip diff harness).
pub fn build_numbering_xml(defs: &engine::numbering::NumberingDefinitions) -> Vec<u8> {
    use engine::numbering as eg;
    let mut s = String::with_capacity(256 + defs.abstract_nums.len() * 400);
    s.push_str(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
         <w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">",
    );
    /* AbstractNums first (Word's canonical order: every abstract
    before any num that references it). */
    let mut abs_ids: Vec<u32> = defs.abstract_nums.keys().copied().collect();
    abs_ids.sort_unstable();
    for id in abs_ids {
        let Some(abs) = defs.abstract_nums.get(&id) else {
            continue;
        };
        s.push_str(&format!("<w:abstractNum w:abstractNumId=\"{}\">", abs.id));
        for lvl in &abs.levels {
            s.push_str(&format!("<w:lvl w:ilvl=\"{}\">", lvl.ilvl));
            s.push_str(&format!("<w:start w:val=\"{}\"/>", lvl.start));
            let fmt_tok = match &lvl.num_fmt {
                eg::NumFmt::Decimal => "decimal",
                eg::NumFmt::DecimalZero => "decimalZero",
                eg::NumFmt::LowerLetter => "lowerLetter",
                eg::NumFmt::UpperLetter => "upperLetter",
                eg::NumFmt::LowerRoman => "lowerRoman",
                eg::NumFmt::UpperRoman => "upperRoman",
                eg::NumFmt::Bullet => "bullet",
                eg::NumFmt::None => "none",
                eg::NumFmt::Other(s) => s.as_str(),
            };
            s.push_str(&format!("<w:numFmt w:val=\"{fmt_tok}\"/>"));
            s.push_str("<w:lvlText w:val=\"");
            for ch in lvl.lvl_text.chars() {
                match ch {
                    '&' => s.push_str("&amp;"),
                    '<' => s.push_str("&lt;"),
                    '>' => s.push_str("&gt;"),
                    '"' => s.push_str("&quot;"),
                    other => s.push(other),
                }
            }
            s.push_str("\"/>");
            if let Some(restart) = lvl.lvl_restart {
                s.push_str(&format!("<w:lvlRestart w:val=\"{restart}\"/>"));
            }
            let ind = &lvl.indent;
            if ind.start_twips != 0
                || ind.end_twips != 0
                || ind.first_line_twips != 0
                || ind.hanging_twips != 0
            {
                s.push_str("<w:pPr><w:ind");
                if ind.start_twips != 0 {
                    s.push_str(&format!(" w:start=\"{}\"", ind.start_twips));
                }
                if ind.end_twips != 0 {
                    s.push_str(&format!(" w:end=\"{}\"", ind.end_twips));
                }
                if ind.first_line_twips != 0 {
                    s.push_str(&format!(" w:firstLine=\"{}\"", ind.first_line_twips));
                }
                if ind.hanging_twips != 0 {
                    s.push_str(&format!(" w:hanging=\"{}\"", ind.hanging_twips));
                }
                s.push_str("/></w:pPr>");
            }
            s.push_str("</w:lvl>");
        }
        s.push_str("</w:abstractNum>");
    }
    let mut num_ids: Vec<u32> = defs.num_instances.keys().copied().collect();
    num_ids.sort_unstable();
    for id in num_ids {
        let Some(num) = defs.num_instances.get(&id) else {
            continue;
        };
        s.push_str(&format!("<w:num w:numId=\"{}\">", num.num_id));
        s.push_str(&format!(
            "<w:abstractNumId w:val=\"{}\"/>",
            num.abstract_num_id
        ));
        for ovr in &num.overrides {
            s.push_str(&format!("<w:lvlOverride w:ilvl=\"{}\">", ovr.ilvl));
            if let Some(start) = ovr.start_override {
                s.push_str(&format!("<w:startOverride w:val=\"{start}\"/>"));
            }
            s.push_str("</w:lvlOverride>");
        }
        s.push_str("</w:num>");
    }
    s.push_str("</w:numbering>");
    s.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = br#"<?xml version="1.0"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:abstractNum w:abstractNumId="0">
    <w:lvl w:ilvl="0">
      <w:start w:val="1"/>
      <w:numFmt w:val="decimal"/>
      <w:lvlText w:val="%1."/>
      <w:pPr><w:ind w:start="720" w:hanging="360"/></w:pPr>
    </w:lvl>
    <w:lvl w:ilvl="1">
      <w:start w:val="1"/>
      <w:numFmt w:val="lowerLetter"/>
      <w:lvlText w:val="%2)"/>
    </w:lvl>
  </w:abstractNum>
  <w:abstractNum w:abstractNumId="1">
    <w:lvl w:ilvl="0">
      <w:numFmt w:val="bullet"/>
      <w:lvlText w:val="*"/>
    </w:lvl>
  </w:abstractNum>
  <w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
  <w:num w:numId="2">
    <w:abstractNumId w:val="0"/>
    <w:lvlOverride w:ilvl="0"><w:startOverride w:val="5"/></w:lvlOverride>
  </w:num>
  <w:num w:numId="3"><w:abstractNumId w:val="1"/></w:num>
</w:numbering>"#;

    #[test]
    fn parses_abstract_nums_with_levels() {
        let t = parse_numbering_xml(SAMPLE).expect("parse");
        let a0 = t.abstract_nums.get(&0).expect("abstract 0");
        assert_eq!(a0.levels.len(), 2);
        assert_eq!(a0.levels[0].num_fmt, NumFmt::Decimal);
        assert_eq!(a0.levels[0].lvl_text, "%1.");
        assert_eq!(a0.levels[0].indent.start_twips, 720);
        assert_eq!(a0.levels[0].indent.hanging_twips, 360);
        assert_eq!(a0.levels[1].num_fmt, NumFmt::LowerLetter);
        assert_eq!(a0.levels[1].lvl_text, "%2)");
    }

    #[test]
    fn parses_num_instances_and_bindings() {
        let t = parse_numbering_xml(SAMPLE).expect("parse");
        assert_eq!(t.num_instances.get(&1).unwrap().abstract_num_id, 0);
        assert_eq!(t.num_instances.get(&3).unwrap().abstract_num_id, 1);
    }

    #[test]
    fn level_for_resolves_through_indirection() {
        let t = parse_numbering_xml(SAMPLE).expect("parse");
        let l = t.level_for(1, 0).expect("level");
        assert_eq!(l.num_fmt, NumFmt::Decimal);
        let bullet = t.level_for(3, 0).expect("bullet level");
        assert_eq!(bullet.num_fmt, NumFmt::Bullet);
    }

    #[test]
    fn start_override_takes_priority() {
        let t = parse_numbering_xml(SAMPLE).expect("parse");
        assert_eq!(t.start_for(1, 0), Some(1));
        assert_eq!(t.start_for(2, 0), Some(5));
    }
}
