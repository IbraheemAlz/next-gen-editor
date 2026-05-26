//! `engine::numbering` — Sprint 13 (#11 → #12).
//!
//! In-memory mirror of OOXML's tripartite numbering graph
//! (`<w:numbering>` ↦ `<w:abstractNum>` ↦ `<w:lvl>`):
//!
//! ```text
//! Paragraph<w:pPr><w:numPr><w:numId N>  ──> NumInstance.num_id = N
//!                                              │
//!                                              ▼
//!                              NumInstance.abstract_num_id = A
//!                                              │
//!                                              ▼
//!                                  AbstractNum.levels[ilvl]
//! ```
//!
//! Sprint 13 owns the **synthesis** path: when the UI calls
//! [`NumberingDefinitions::synth_list_definition`], we either reuse a
//! pre-existing matching template or append a fresh stock hierarchy.
//! The "reuse first" rule is the load-bearing **idempotency mandate** —
//! repeated `Bullet ↔ Number ↔ Bullet` toggles must NEVER inflate
//! `numbering.xml`.
//!
//! Reader-side parsing lives in `crates/format-docx/src/parts/numbering.rs`;
//! that crate converts its `NumberingDefinitions` into this engine
//! mirror at archive-open time. The format-docx counterpart is the
//! source of truth for parse semantics; this module is the source of
//! truth for editing + synthesis.

use crate::Indent;
use std::collections::HashMap;

/// `<w:numFmt w:val>` — number/marker format. Engine ships the common
/// formats; `Other` preserves the source token so a round-trip stays
/// lossless.
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

/// One `<w:lvl>` entry inside an `<w:abstractNum>`.
#[derive(Debug, Clone)]
pub struct LvlDef {
    pub ilvl: u8,
    pub start: i32,
    pub num_fmt: NumFmt,
    /// `<w:lvlText>` — marker template with `%1`..`%9` counter refs.
    pub lvl_text: String,
    pub lvl_restart: Option<u8>,
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

#[derive(Debug, Clone)]
pub struct LvlOverride {
    pub ilvl: u8,
    pub start_override: Option<i32>,
    pub lvl: Option<LvlDef>,
}

/// `<w:num w:numId>` — paragraph-referenced instance binding a numId
/// to an abstractNumId (plus optional per-level overrides).
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

/// Sprint 13 (#12) — discriminator used by
/// [`NumberingDefinitions::synth_list_definition`] to pick which stock
/// hierarchy to reuse or append. `Off` is handled by the caller; this
/// enum covers only the two synthesisable kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListSynthesisKind {
    Bullet,
    Number,
}

#[derive(Debug, Clone, Default)]
pub struct NumberingDefinitions {
    pub abstract_nums: HashMap<u32, AbstractNum>,
    pub num_instances: HashMap<u32, NumInstance>,
    /// Sprint 13 (#12) — flips to `true` on the first
    /// synth_list_definition call that materially mutates the store
    /// (a fresh AbstractNum or Num appended). When `false` the
    /// writer keeps the OPC passthrough byte-identical;
    /// `synth_list_definition` calls that reuse an existing template
    /// leave this `false`.
    pub dirty: bool,
}

impl NumberingDefinitions {
    /// Resolve `(num_id, ilvl)` → effective `LvlDef`. Override wins
    /// over the abstract definition.
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

    /// `<w:start>` for `(num_id, ilvl)`, with `<w:startOverride>`
    /// taking priority.
    pub fn start_for(&self, num_id: u32, ilvl: u8) -> Option<i32> {
        let num = self.num_instances.get(&num_id)?;
        if let Some(ovr) = num.lvl_override(ilvl)
            && let Some(s) = ovr.start_override
        {
            return Some(s);
        }
        self.level_for(num_id, ilvl).map(|l| l.start)
    }

    /// Sprint 13 (#12) — locate (or append) a numId whose binding
    /// matches the standard stock hierarchy for `kind`.
    ///
    /// ### Idempotency mandate
    ///
    /// Repeated Bullet↔Number↔Bullet toggles must NEVER inflate
    /// `numbering.xml`. The algorithm is:
    ///
    /// 1. Scan every existing `NumInstance` whose target `AbstractNum`
    ///    matches the stock template for `kind` (level 0's `numFmt`
    ///    is `Bullet` for bullet lists, `Decimal` for number lists).
    ///    First match wins → return its `num_id`.
    /// 2. Else, scan every existing `AbstractNum` for a matching
    ///    stock template. If found, mint only a fresh `Num` pointing
    ///    at it (no new AbstractNum).
    /// 3. Else, mint a fresh `AbstractNum` AND a fresh `Num`.
    ///
    /// Returns the resulting `num_id`. Sets `self.dirty = true` only
    /// when (2) or (3) materially adds new entries.
    pub fn synth_list_definition(&mut self, kind: ListSynthesisKind) -> u32 {
        /* Step 1 — look for a NumInstance whose abstract matches. */
        for (num_id, num) in &self.num_instances {
            if let Some(abs) = self.abstract_nums.get(&num.abstract_num_id)
                && abstract_matches_kind(abs, kind)
            {
                return *num_id;
            }
        }
        /* Step 2 — abstract exists, but no NumInstance references it.
        Mint a Num. */
        if let Some(abs_id) = self
            .abstract_nums
            .iter()
            .find(|(_, a)| abstract_matches_kind(a, kind))
            .map(|(id, _)| *id)
        {
            let new_num_id = self.next_num_id();
            self.num_instances.insert(
                new_num_id,
                NumInstance {
                    num_id: new_num_id,
                    abstract_num_id: abs_id,
                    overrides: Vec::new(),
                },
            );
            self.dirty = true;
            return new_num_id;
        }
        /* Step 3 — mint both. */
        let new_abs_id = self.next_abstract_id();
        let new_abs = AbstractNum {
            id: new_abs_id,
            levels: stock_levels_for(kind),
        };
        self.abstract_nums.insert(new_abs_id, new_abs);
        let new_num_id = self.next_num_id();
        self.num_instances.insert(
            new_num_id,
            NumInstance {
                num_id: new_num_id,
                abstract_num_id: new_abs_id,
                overrides: Vec::new(),
            },
        );
        self.dirty = true;
        new_num_id
    }

    /// Smallest `u32` not present in `self.num_instances` (1-based —
    /// Word never uses `numId 0`).
    fn next_num_id(&self) -> u32 {
        let mut id = 1u32;
        while self.num_instances.contains_key(&id) {
            id = id.saturating_add(1);
        }
        id
    }

    /// Smallest `u32` not present in `self.abstract_nums` (0-based —
    /// Word's stock numbering.xml starts abstractNumId at 0).
    fn next_abstract_id(&self) -> u32 {
        let mut id = 0u32;
        while self.abstract_nums.contains_key(&id) {
            id = id.saturating_add(1);
        }
        id
    }
}

/// True when `abs` looks like the stock Bullet (level 0 = `Bullet`) or
/// the stock Number (level 0 = `Decimal`) template. Sprint 13 keeps
/// this match crude — any prior synth-or-imported abstract whose
/// level-0 numFmt is the right shape counts as a reuse candidate.
/// Heading-numbering, multi-style numFmts, and per-level overrides
/// still hit the "fresh AbstractNum" path.
fn abstract_matches_kind(abs: &AbstractNum, kind: ListSynthesisKind) -> bool {
    let Some(lvl0) = abs.level(0) else {
        return false;
    };
    match kind {
        ListSynthesisKind::Bullet => matches!(lvl0.num_fmt, NumFmt::Bullet),
        ListSynthesisKind::Number => matches!(lvl0.num_fmt, NumFmt::Decimal),
    }
}

/// Word's stock 9-level bullet / decimal hierarchies. Marker glyphs,
/// indents (twips), and `lvl_text` templates match the values
/// Word.exe stamps on a freshly-clicked "Bullet" / "Number" toolbar
/// button.
fn stock_levels_for(kind: ListSynthesisKind) -> Vec<LvlDef> {
    match kind {
        ListSynthesisKind::Bullet => stock_bullet_levels(),
        ListSynthesisKind::Number => stock_number_levels(),
    }
}

/// Word's stock bullet glyphs cycled across nine levels:
/// `• ◦ ▪ • ◦ ▪ • ◦ ▪`. Word actually uses Symbol-font codepoints; we
/// substitute Unicode equivalents so the engine renders without a
/// fancy private-use lookup.
fn stock_bullet_levels() -> Vec<LvlDef> {
    const GLYPHS: [&str; 9] = ["•", "◦", "▪", "•", "◦", "▪", "•", "◦", "▪"];
    (0..9u8)
        .map(|i| LvlDef {
            ilvl: i,
            start: 1,
            num_fmt: NumFmt::Bullet,
            lvl_text: GLYPHS[i as usize].to_string(),
            lvl_restart: None,
            /* Each level deepens by 360 twips (0.25 inch). */
            indent: Indent {
                start_twips: 720 + 360 * i32::from(i),
                end_twips: 0,
                first_line_twips: 0,
                hanging_twips: 360,
            },
        })
        .collect()
}

/// Word's stock decimal/letter/roman hierarchy: 1., a., i., 1., a.,
/// i., 1., a., i. Cycle matches Word's default ordered-list
/// outline.
fn stock_number_levels() -> Vec<LvlDef> {
    const FORMATS: [NumFmt; 9] = [
        NumFmt::Decimal,
        NumFmt::LowerLetter,
        NumFmt::LowerRoman,
        NumFmt::Decimal,
        NumFmt::LowerLetter,
        NumFmt::LowerRoman,
        NumFmt::Decimal,
        NumFmt::LowerLetter,
        NumFmt::LowerRoman,
    ];
    /* `lvl_text` per level — `%1.`, `%2.`, `%3.`, … with each
    placeholder pointing at its own level's counter. */
    (0..9u8)
        .map(|i| LvlDef {
            ilvl: i,
            start: 1,
            num_fmt: FORMATS[i as usize].clone(),
            lvl_text: format!("%{}.", i + 1),
            lvl_restart: None,
            indent: Indent {
                start_twips: 720 + 360 * i32::from(i),
                end_twips: 0,
                first_line_twips: 0,
                hanging_twips: 360,
            },
        })
        .collect()
}

/* ---------------------------------------------------------------- */
/* Marker resolution                                                */
/* ---------------------------------------------------------------- */

/// Render the marker string for one list paragraph given its
/// `(num_id, ilvl)` and the running counter state. Walks the level
/// template (`%1`/`%2`/...) substituting each placeholder with the
/// formatted counter at that ancestor level. Non-`%N` characters
/// (bullet glyphs, separators) pass through verbatim.
pub fn render_marker(
    defs: &NumberingDefinitions,
    num_id: u32,
    ilvl: u8,
    counters: &HashMap<(u32, u8), i32>,
) -> Option<String> {
    let lvl = defs.level_for(num_id, ilvl)?;
    let mut out = String::with_capacity(lvl.lvl_text.len() + 4);
    let mut chars = lvl.lvl_text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(&nxt) = chars.peek() else {
            out.push('%');
            continue;
        };
        let Some(n) = nxt.to_digit(10) else {
            out.push('%');
            continue;
        };
        if !(1..=9).contains(&n) {
            out.push('%');
            continue;
        }
        chars.next();
        let target_ilvl = (n - 1) as u8;
        let v = counters.get(&(num_id, target_ilvl)).copied().unwrap_or(0);
        let fmt = defs
            .level_for(num_id, target_ilvl)
            .map(|l| l.num_fmt.clone())
            .unwrap_or(NumFmt::Decimal);
        out.push_str(&format_counter(v, &fmt));
    }
    Some(out)
}

fn format_counter(n: i32, fmt: &NumFmt) -> String {
    match fmt {
        NumFmt::Decimal => n.to_string(),
        NumFmt::DecimalZero => format!("{n:02}"),
        NumFmt::LowerLetter => letter(n, false),
        NumFmt::UpperLetter => letter(n, true),
        NumFmt::LowerRoman => roman(n).to_lowercase(),
        NumFmt::UpperRoman => roman(n),
        NumFmt::Bullet => String::new(),
        NumFmt::None => String::new(),
        NumFmt::Other(_) => n.to_string(),
    }
}

fn letter(n: i32, upper: bool) -> String {
    if n <= 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut k = n;
    let base = if upper { b'A' } else { b'a' };
    while k > 0 {
        let r = (k - 1) % 26;
        out.insert(0, char::from(base + r as u8));
        k = (k - 1) / 26;
    }
    out
}

fn roman(n: i32) -> String {
    let n = n.clamp(1, 3999);
    const TBL: [(i32, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    let mut k = n;
    for (val, sym) in TBL {
        while k >= val {
            out.push_str(sym);
            k -= val;
        }
    }
    out
}

/* ---------------------------------------------------------------- */
/* Whole-document marker resolver                                   */
/* ---------------------------------------------------------------- */

/// Sprint 13 (#12) — render `Paragraph::resolved_marker` for every
/// list paragraph in `paragraphs`, honouring outline-reset semantics
/// (visiting level `N` drops all counters at level `> N` for the
/// same `num_id`). Mirrors `format-docx::numbering_resolver` but
/// runs on the in-memory engine model so post-ToggleList mutations
/// refresh markers without re-traversing `other_entries`.
pub fn resolve_markers_in_place(
    paragraphs: &mut [&mut crate::Paragraph],
    defs: &NumberingDefinitions,
) {
    let mut counters: HashMap<(u32, u8), i32> = HashMap::new();
    let mut max_seen_ilvl: HashMap<u32, u8> = HashMap::new();
    for para in paragraphs.iter_mut() {
        let Some(li) = para.list_item else {
            para.resolved_marker = None;
            continue;
        };
        let crate::ListItem { num_id, ilvl } = li;
        if let Some(&max) = max_seen_ilvl.get(&num_id) {
            for deeper in (ilvl + 1)..=max {
                counters.remove(&(num_id, deeper));
            }
        }
        max_seen_ilvl
            .entry(num_id)
            .and_modify(|m| *m = (*m).max(ilvl))
            .or_insert(ilvl);
        match counters.get_mut(&(num_id, ilvl)) {
            Some(c) => *c += 1,
            None => {
                let start = defs.start_for(num_id, ilvl).unwrap_or(1);
                counters.insert((num_id, ilvl), start);
            }
        }
        para.resolved_marker = render_marker(defs, num_id, ilvl, &counters);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_bullet_creates_then_reuses() {
        let mut defs = NumberingDefinitions::default();
        let n1 = defs.synth_list_definition(ListSynthesisKind::Bullet);
        assert!(defs.dirty);
        assert_eq!(defs.abstract_nums.len(), 1);
        assert_eq!(defs.num_instances.len(), 1);
        defs.dirty = false; // reset; subsequent reuse must not flip it
        let n2 = defs.synth_list_definition(ListSynthesisKind::Bullet);
        assert_eq!(n1, n2, "second bullet call must REUSE the same numId");
        assert!(!defs.dirty, "reuse must not mark numbering dirty");
        assert_eq!(defs.abstract_nums.len(), 1);
        assert_eq!(defs.num_instances.len(), 1);
    }

    #[test]
    fn synth_number_creates_then_reuses_independently_of_bullet() {
        let mut defs = NumberingDefinitions::default();
        let bullet_id = defs.synth_list_definition(ListSynthesisKind::Bullet);
        let number_id = defs.synth_list_definition(ListSynthesisKind::Number);
        assert_ne!(bullet_id, number_id);
        assert_eq!(defs.abstract_nums.len(), 2);
        assert_eq!(defs.num_instances.len(), 2);
        defs.dirty = false;
        let n2 = defs.synth_list_definition(ListSynthesisKind::Number);
        assert_eq!(number_id, n2);
        assert!(!defs.dirty);
    }

    #[test]
    fn repeated_toggles_never_inflate() {
        let mut defs = NumberingDefinitions::default();
        for _ in 0..20 {
            defs.synth_list_definition(ListSynthesisKind::Bullet);
            defs.synth_list_definition(ListSynthesisKind::Number);
        }
        assert_eq!(defs.abstract_nums.len(), 2);
        assert_eq!(defs.num_instances.len(), 2);
    }

    #[test]
    fn stock_bullet_level_0_glyph() {
        let levels = stock_bullet_levels();
        assert_eq!(levels.len(), 9);
        assert_eq!(levels[0].num_fmt, NumFmt::Bullet);
        assert_eq!(levels[0].lvl_text, "•");
    }

    #[test]
    fn render_marker_decimal_substitutes() {
        let mut defs = NumberingDefinitions::default();
        defs.synth_list_definition(ListSynthesisKind::Number);
        let mut counters: HashMap<(u32, u8), i32> = HashMap::new();
        counters.insert((1, 0), 7);
        let m = render_marker(&defs, 1, 0, &counters).unwrap();
        assert_eq!(m, "7.");
    }

    #[test]
    fn render_marker_bullet_yields_glyph() {
        let mut defs = NumberingDefinitions::default();
        defs.synth_list_definition(ListSynthesisKind::Bullet);
        let counters: HashMap<(u32, u8), i32> = HashMap::new();
        let m = render_marker(&defs, 1, 0, &counters).unwrap();
        assert_eq!(m, "•");
    }
}
