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
//! Issue #395 — a style's (and the docDefaults') `<w:pPr><w:pBdr>` is
//! read into its paragraph properties like a paragraph's own: physical
//! edges as read, an explicit `nil` / `none` edge as a set "no border"
//! edge (so it can remove an inherited one), and the logical
//! `<w:start>` / `<w:end>` edges folded by the style's OWN `<w:bidi>`
//! once the style ends — the cascade turns them to the paragraph's
//! final direction (`engine::ParaProperties::cascade`).
//!
//! The pass-through invariant is preserved: `word/styles.xml` rides
//! [`crate::opc::archive::DocxArchive::other_entries`] verbatim. This module
//! never serialises.

use crate::error::DocxError;
use crate::schema::ct_pbdr::{PbdrLogical, apply_pbdr_edge};
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
    /// Issue #297 — `<w:name w:val="…"/>`: the style's display name
    /// (`heading 1`, a localized or custom name), distinct from the
    /// `w:styleId` (`Heading1`). `None` when the source omits it.
    pub name: Option<String>,
    pub kind: StyleKind,
    /// `<w:basedOn w:val="…"/>` — the parent style id. The resolver folds
    /// the cascade root-first along this chain.
    pub based_on: Option<String>,
    /// Issue #277 — `<w:next w:val="…"/>`: the style of the paragraph
    /// Enter creates at the end of one in this style.
    pub next: Option<String>,
    pub para: ParaProperties,
    pub run: SpanStyle,
}

#[derive(Debug, Clone, Default)]
pub struct StyleTable {
    pub defaults: DocDefaults,
    /// `style id → definition`. Lookup feeds [`crate::style_resolver`].
    pub by_id: HashMap<String, StyleDef>,
    /// Issue #556 — the id of the DEFAULT paragraph style
    /// (`<w:style w:type="paragraph" w:default="1">`: `Normal` in Word's
    /// English files, `a` / `Standard` / … in localized ones), the style
    /// every paragraph that names none is formatted with (ECMA-376
    /// §17.7.4.17). Resolved once per part by the spec's rule: when
    /// several paragraph styles claim it, the LAST one in the part wins.
    /// `None` when no paragraph style claims it.
    pub default_paragraph: Option<String>,
    /// Issue #556 — the id of the default CHARACTER style
    /// (`w:type="character" w:default="1"`, Word's `DefaultParagraphFont`),
    /// same last-wins rule. Recorded only: the run cascade does not apply
    /// it (issue #558 — its properties would have to stay out of the
    /// spans, which bake the character style they name).
    pub default_character: Option<String>,
}

impl StyleTable {
    /// Issue #556 — the paragraph style a paragraph whose `<w:pStyle>` is
    /// `p_style` is formatted with: that style when this part defines it
    /// as a PARAGRAPH style, else the default paragraph style — a
    /// paragraph naming no style, or one this part does not define as a
    /// paragraph style, takes `Normal`, as in Word. `None` when neither
    /// exists (the cascade is then the docDefaults alone). The engine's
    /// `StyleCascade::applied_style` is the same rule over the mirrored
    /// table.
    pub fn applied_paragraph_style<'s>(&'s self, p_style: Option<&'s str>) -> Option<&'s str> {
        p_style
            .filter(|id| {
                self.by_id
                    .get(*id)
                    .is_some_and(|d| d.kind == StyleKind::Paragraph)
            })
            .or(self.default_paragraph.as_deref())
    }
}

/* ------------------------------------------------------------ helpers --- */

/// Issue #556 — `<w:style w:default>` is an `ST_OnOff`: `1` / `true` /
/// `on` claim the default, anything else (or no attribute) does not.
fn default_attr_on(e: &BytesStart) -> bool {
    attr_val(e, b"w:default")
        .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on"))
}

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
    /* Issue #395 — the docDefaults' logical `<w:pBdr>` edges, folded by
    the defaults' own direction when `<w:docDefaults>` ends. */
    let mut defaults_pbdr = PbdrLogical::default();
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
    /* Issue #297 — a direct child of `<w:style>` (its `<w:name>`). */
    let in_style = |stack: &[Vec<u8>]| -> bool {
        stack
            .last()
            .map(|n| n.as_slice() == b"w:style")
            .unwrap_or(false)
    };
    /* Issue #395 — `<w:pPr>/<w:pBdr>` (a style's or the docDefaults'). */
    let in_pbdr = |stack: &[Vec<u8>]| -> bool {
        matches!(
            stack,
            [.., parent, last] if last.as_slice() == b"w:pBdr" && parent.as_slice() == b"w:pPr"
        )
    };
    /* Audit gap A.M17 — `<w:style>/<w:pPr>/<w:numPr>`. */
    let in_numpr = |stack: &[Vec<u8>]| -> bool {
        stack
            .last()
            .map(|n| n.as_slice() == b"w:numPr")
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
                    b"w:next" => {
                        if let Some(s) = cur_style.as_mut() {
                            s.next = attr_val(&e, b"w:val");
                        }
                    }
                    b"w:name" if in_style(&stack) => {
                        if let Some(s) = cur_style.as_mut() {
                            s.name = attr_val(&e, b"w:val");
                        }
                    }
                    n if in_rpr(&stack) && !pmark_rpr(&stack) => {
                        apply_to_rpr(n, &e, &mut table, &mut cur_style, in_doc_defaults);
                    }
                    n if in_numpr(&stack) => {
                        apply_to_numpr_style(
                            n,
                            &e,
                            &mut cur_style,
                            &mut table.defaults.para,
                            in_doc_defaults,
                        );
                    }
                    n if in_pbdr(&stack) => apply_to_pbdr(
                        n,
                        &e,
                        &mut table,
                        &mut cur_style,
                        &mut defaults_pbdr,
                        in_doc_defaults,
                    ),
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
                    /* Issue #556 — a self-closing `<w:style …/>` defines
                    a style with no properties (`DefaultParagraphFont`):
                    it exists, so a `pStyle` naming it is not undefined
                    (which would take the default paragraph style). */
                    b"w:style" => {
                        finish_style(StyleScratch::from_start(&e), &mut table);
                    }
                    b"w:basedOn" => {
                        if let Some(s) = cur_style.as_mut() {
                            s.based_on = attr_val(&e, b"w:val");
                        }
                    }
                    b"w:next" => {
                        if let Some(s) = cur_style.as_mut() {
                            s.next = attr_val(&e, b"w:val");
                        }
                    }
                    b"w:name" if in_style(&stack) => {
                        if let Some(s) = cur_style.as_mut() {
                            s.name = attr_val(&e, b"w:val");
                        }
                    }
                    n if in_rpr(&stack) && !pmark_rpr(&stack) => {
                        apply_to_rpr(n, &e, &mut table, &mut cur_style, in_doc_defaults);
                    }
                    n if in_numpr(&stack) => {
                        apply_to_numpr_style(
                            n,
                            &e,
                            &mut cur_style,
                            &mut table.defaults.para,
                            in_doc_defaults,
                        );
                    }
                    n if in_pbdr(&stack) => apply_to_pbdr(
                        n,
                        &e,
                        &mut table,
                        &mut cur_style,
                        &mut defaults_pbdr,
                        in_doc_defaults,
                    ),
                    n if in_ppr(&stack) => {
                        apply_to_ppr(n, &e, &mut table, &mut cur_style, in_doc_defaults);
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                let name = e.name().as_ref().to_owned();
                match name.as_slice() {
                    b"w:docDefaults" => {
                        in_doc_defaults = false;
                        std::mem::take(&mut defaults_pbdr).fold_into(&mut table.defaults.para);
                    }
                    b"w:style" => {
                        if let Some(s) = cur_style.take() {
                            finish_style(s, &mut table);
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

/// A finished `<w:style>` joins the table (the last element of an id
/// wins). Issue #556 — the LAST style of a type claiming `w:default` is
/// that type's default (§17.7.4.17).
fn finish_style(s: StyleScratch, table: &mut StyleTable) {
    let claims_default = s.is_default;
    let Some(def) = s.into_def() else {
        return;
    };
    if claims_default {
        match def.kind {
            StyleKind::Paragraph => table.default_paragraph = Some(def.id.clone()),
            StyleKind::Character => table.default_character = Some(def.id.clone()),
            StyleKind::Table | StyleKind::Numbering => {}
        }
    }
    table.by_id.insert(def.id.clone(), def);
}

#[derive(Debug)]
struct StyleScratch {
    id: Option<String>,
    name: Option<String>,
    kind: Option<StyleKind>,
    based_on: Option<String>,
    next: Option<String>,
    para: ParaProperties,
    run: SpanStyle,
    /// Issue #395 — the style's logical `<w:pBdr>` edges, folded by its
    /// own direction in [`Self::into_def`].
    pbdr_logical: PbdrLogical,
    /// Issue #556 — the start tag's `w:default` claim.
    is_default: bool,
}

impl StyleScratch {
    fn from_start(e: &BytesStart) -> Self {
        Self {
            id: attr_val(e, b"w:styleId"),
            name: None,
            kind: attr_val(e, b"w:type").as_deref().and_then(parse_kind),
            based_on: None,
            next: None,
            para: ParaProperties::default(),
            run: SpanStyle::default(),
            pbdr_logical: PbdrLogical::default(),
            is_default: default_attr_on(e),
        }
    }
    fn into_def(self) -> Option<StyleDef> {
        let id = self.id?;
        let kind = self.kind?;
        let mut para = self.para;
        self.pbdr_logical.fold_into(&mut para);
        Some(StyleDef {
            id,
            name: self.name,
            kind,
            based_on: self.based_on,
            next: self.next,
            para,
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

/// Issue #395 — one `<w:pBdr>` edge child of a style's (or the
/// docDefaults') `<w:pPr>`: a logical edge is held until the owner's
/// direction is known, a physical one lands in its slot.
fn apply_to_pbdr(
    name: &[u8],
    e: &BytesStart,
    table: &mut StyleTable,
    cur_style: &mut Option<StyleScratch>,
    defaults_pbdr: &mut PbdrLogical,
    in_doc_defaults: bool,
) {
    let (para, logical) = if in_doc_defaults && cur_style.is_none() {
        (&mut table.defaults.para, defaults_pbdr)
    } else if let Some(s) = cur_style.as_mut() {
        (&mut s.para, &mut s.pbdr_logical)
    } else {
        return;
    };
    if !logical.accept(name, e) {
        apply_pbdr_edge(name, e, para);
    }
}

/// Audit gap A.M17 — fold `<w:numPr>` children inside a style's `<w:pPr>`
/// into the style's `para.list_item`. The style cascade then propagates
/// the binding to any paragraph referencing this style by `<w:pStyle>`.
/// Called from the styles parser when the event stack carries
/// `<w:style>/<w:pPr>/<w:numPr>`. Partial refs (numId without ilvl) get
/// the same defaults as in `document.rs` — ilvl 0 stands in.
fn apply_to_numpr_style(
    name: &[u8],
    e: &BytesStart,
    cur_style: &mut Option<StyleScratch>,
    defaults_para: &mut ParaProperties,
    in_doc_defaults: bool,
) {
    let target = if in_doc_defaults && cur_style.is_none() {
        &mut defaults_para.list_item
    } else if let Some(s) = cur_style.as_mut() {
        &mut s.para.list_item
    } else {
        return;
    };
    /* Lazy-init the slot — patches `num_id` / `ilvl` independently. */
    let item = target.get_or_insert(engine::ListItem { num_id: 0, ilvl: 0 });
    match name {
        b"w:numId" => {
            if let Some(v) = attr_val(e, b"w:val").and_then(|v| v.trim().parse().ok()) {
                item.num_id = v;
            }
        }
        b"w:ilvl" => {
            if let Some(v) = attr_val(e, b"w:val").and_then(|v| v.trim().parse().ok()) {
                item.ilvl = v;
            }
        }
        _ => {}
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
    <w:next w:val="BaseStyle"/>
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
        assert_eq!(base.next, None);
        assert_eq!(base.run.bold, Some(true));
        /* Issue #297 — the display name, not the id. */
        assert_eq!(base.name.as_deref(), Some("Base Style"));
        assert_eq!(t.by_id["Emphasis"].name, None, "no <w:name>");

        let child = t.by_id.get("ChildStyle").unwrap();
        assert_eq!(child.based_on.as_deref(), Some("BaseStyle"));
        /* Issue #277 — `<w:next>` drives Enter-at-end. */
        assert_eq!(child.next.as_deref(), Some("BaseStyle"));
        assert_eq!(child.run.italic, Some(true));
        assert_eq!(child.para.alignment, Some(Alignment::Center));

        let emphasis = t.by_id.get("Emphasis").unwrap();
        assert_eq!(emphasis.kind, StyleKind::Character);
    }

    /// Issue #395 — `<w:pPr><w:pBdr>` on a style and on the docDefaults:
    /// physical edges as read, `nil` as a set "no border" edge, logical
    /// edges by the owner's OWN direction (`<w:bidi>` after `<w:pBdr>`),
    /// table-border children never mistaken for paragraph borders.
    #[test]
    fn parses_paragraph_borders_on_styles_and_doc_defaults() {
        let xml = br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:docDefaults><w:pPrDefault><w:pPr><w:pBdr><w:between w:val="single" w:sz="4"/></w:pBdr></w:pPr></w:pPrDefault></w:docDefaults>
  <w:style w:type="paragraph" w:styleId="Title"><w:pPr><w:pBdr><w:bottom w:val="single" w:sz="8" w:space="4" w:color="4F81BD"/></w:pBdr></w:pPr></w:style>
  <w:style w:type="paragraph" w:styleId="NoTop"><w:pPr><w:pBdr><w:top w:val="nil"/></w:pBdr></w:pPr></w:style>
  <w:style w:type="paragraph" w:styleId="Rtl"><w:pPr><w:pBdr><w:start w:val="single"/><w:end w:val="double"/></w:pBdr><w:bidi/></w:pPr></w:style>
  <w:style w:type="table" w:styleId="Grid"><w:tblPr><w:tblBorders><w:top w:val="single"/></w:tblBorders></w:tblPr></w:style>
</w:styles>"#;
        let t = parse_styles_xml(xml).expect("parse");
        let between = t.defaults.para.borders.clone().expect("defaults pBdr");
        assert!(between.inside_h.is_some() && between.top.is_none());

        let title = t.by_id["Title"].para.borders.clone().expect("Title pBdr");
        let bottom = title.bottom.expect("bottom");
        assert_eq!(bottom.size_eighth_pt, 8);
        assert_eq!(bottom.color, Some([0x4F, 0x81, 0xBD, 255]));

        let no_top = t.by_id["NoTop"].para.borders.clone().expect("NoTop pBdr");
        assert_eq!(no_top.top.map(|s| s.style), Some(engine::BorderStyle::None));

        let rtl = &t.by_id["Rtl"].para;
        let b = rtl.borders.clone().expect("Rtl pBdr");
        assert_eq!(
            b.right.map(|s| s.style),
            Some(engine::BorderStyle::Single),
            "start = right in the style's own RTL"
        );
        assert_eq!(b.left.map(|s| s.style), Some(engine::BorderStyle::Double));
        assert!(rtl.border_spelling.start && rtl.border_spelling.end);

        assert!(
            t.by_id["Grid"].para.borders.is_none(),
            "a table style's tblBorders are not paragraph borders"
        );
    }

    /// Issue #556 — `w:default` per style type, the LAST claimant of a
    /// type winning (§17.7.4.17); `0` / `false` / `off` claim nothing; a
    /// table / numbering default is not a paragraph or character one.
    #[test]
    fn the_last_default_claimant_of_each_type_wins() {
        let xml =
            br#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:style w:type="paragraph" w:default="1" w:styleId="First"/>
  <w:style w:type="paragraph" w:default="true" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
  <w:style w:type="paragraph" w:default="0" w:styleId="NotIt"/>
  <w:style w:type="paragraph" w:default="off" w:styleId="NorThis"/>
  <w:style w:type="character" w:default="1" w:styleId="DefaultParagraphFont"/>
  <w:style w:type="table" w:default="1" w:styleId="TableNormal"/>
  <w:style w:type="numbering" w:default="1" w:styleId="NoList"/>
</w:styles>"#;
        let t = parse_styles_xml(xml).expect("parse");
        assert_eq!(t.default_paragraph.as_deref(), Some("Normal"));
        assert_eq!(t.default_character.as_deref(), Some("DefaultParagraphFont"));
        assert_eq!(t.applied_paragraph_style(None), Some("Normal"));
        assert_eq!(t.applied_paragraph_style(Some("First")), Some("First"));
        assert_eq!(
            t.applied_paragraph_style(Some("Missing")),
            Some("Normal"),
            "an undefined pStyle takes the default"
        );
        assert_eq!(
            t.applied_paragraph_style(Some("DefaultParagraphFont")),
            Some("Normal"),
            "a character style is no paragraph style"
        );

        let none = parse_styles_xml(SAMPLE).expect("parse");
        assert_eq!(none.default_paragraph, None, "no claimant");
        assert_eq!(none.applied_paragraph_style(None), None);
        assert_eq!(none.applied_paragraph_style(Some("Missing")), None);
    }
}
