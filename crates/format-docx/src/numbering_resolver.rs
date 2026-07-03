//! Numbering resolver (Phase 4).
//!
//! Walks a document's paragraphs in order, maintains per-`(num_id, ilvl)`
//! counter state, and computes each list paragraph's marker string by
//! substituting `%1` / `%2` / ... in the level's `<w:lvlText>` template
//! with the formatted counter at each ancestor level.
//!
//! ## Counter semantics (ECMA-376 §17.9.20)
//!
//! - First visit to a given `(num_id, ilvl)` initialises the counter at
//!   `start` (effective `<w:start>` or `<w:startOverride>` — see
//!   [`NumberingDefinitions::start_for`]).
//! - Every subsequent visit increments by 1.
//! - A visit at level `ilvl` **drops** every counter at deeper levels
//!   `> ilvl` for that `num_id`, so the next visit at a deeper level
//!   re-initialises from `start`. This matches Word's "outline reset
//!   when a parent level changes" behaviour and is what makes a
//!   `%1.%2.` template at level 1 produce `1.1`, `1.2`, `1.3`, then
//!   reset to `2.1` after a level-0 paragraph appears.
//!
//! ## `lvlText` substitution
//!
//! The template is a Unicode string where `%N` (N = 1..=9) references the
//! formatted counter at `ilvl = N - 1`. Non-`%N` characters pass through
//! verbatim — that is how bullet glyphs (`•`, `▪`, `o`, etc.) ride along
//! without any decimal-format logic. `%N` references to a level that
//! hasn't been seen yet (and has no `start`) format as `0` defensively.

use crate::parts::numbering::{NumFmt, NumberingDefinitions};
use engine::{Block, ListItem, Paragraph};
use std::collections::HashMap;

/// Render every list paragraph's marker, writing it into
/// `Paragraph::resolved_marker`. Non-list paragraphs and `Block::Table`
/// entries are skipped — Phase 5 PR 1 keeps numbering counters running
/// across intervening tables (matches Word's "list continues across
/// non-list blocks of the same numId" semantics).
pub fn resolve_markers_blocks(blocks: &mut [Block], defs: &NumberingDefinitions) {
    let mut paragraphs: Vec<&mut Paragraph> = blocks
        .iter_mut()
        .filter_map(|b| b.as_paragraph_mut())
        .collect();
    resolve_markers_inner(&mut paragraphs, defs);
}

/// Direct entry point for tests / callers that already have a
/// `[Paragraph]` slice.
pub fn resolve_markers(paragraphs: &mut [Paragraph], defs: &NumberingDefinitions) {
    let mut refs: Vec<&mut Paragraph> = paragraphs.iter_mut().collect();
    resolve_markers_inner(&mut refs, defs);
}

fn resolve_markers_inner(paragraphs: &mut [&mut Paragraph], defs: &NumberingDefinitions) {
    let mut counters: HashMap<(u32, u8), i32> = HashMap::new();
    /* Track the highest `ilvl` ever observed per `num_id` so we know which
    deeper levels to drop on a parent-level visit. */
    let mut max_seen_ilvl: HashMap<u32, u8> = HashMap::new();

    for para in paragraphs.iter_mut() {
        let Some(li) = para.list_item else { continue };
        let ListItem { num_id, ilvl } = li;

        /* Drop counters at every level deeper than `ilvl` — they restart
        on next visit. */
        if let Some(&max) = max_seen_ilvl.get(&num_id) {
            for deeper in (ilvl + 1)..=max {
                counters.remove(&(num_id, deeper));
            }
        }
        max_seen_ilvl
            .entry(num_id)
            .and_modify(|m| *m = (*m).max(ilvl))
            .or_insert(ilvl);

        /* Initialise or increment this level's counter. */
        match counters.get_mut(&(num_id, ilvl)) {
            Some(c) => *c += 1,
            None => {
                let start = defs.start_for(num_id, ilvl).unwrap_or(1);
                counters.insert((num_id, ilvl), start);
            }
        }

        para.resolved_marker = render_marker(num_id, ilvl, defs, &counters);
        /* Issue #50 — geometry rides along with the marker: the level's
        `<w:ind>` is the layout fallback when the paragraph carries no
        direct indent, mirroring `engine::numbering::resolve_markers_in_place`
        so docx-opened lists and interactively-toggled lists share the
        same marker/indent contract. */
        para.resolved_list_indent = defs.level_for(num_id, ilvl).map(|l| l.indent);
    }
}

/// Format a single `(num_id, ilvl)` paragraph's marker. Public so unit
/// tests can drive it directly without building a full `DocumentTree`.
pub fn render_marker(
    num_id: u32,
    ilvl: u8,
    defs: &NumberingDefinitions,
    counters: &HashMap<(u32, u8), i32>,
) -> Option<String> {
    let lvl = defs.level_for(num_id, ilvl)?;
    /* Bullet / none never substitute — the template is the marker
    literal (often the bullet glyph itself). */
    if matches!(lvl.num_fmt, NumFmt::Bullet | NumFmt::None) {
        return Some(lvl.lvl_text.clone());
    }
    Some(substitute(&lvl.lvl_text, |ref_ilvl| {
        let n = counters.get(&(num_id, ref_ilvl)).copied().unwrap_or(0);
        let fmt = defs
            .level_for(num_id, ref_ilvl)
            .map(|l| &l.num_fmt)
            .unwrap_or(&NumFmt::Decimal);
        format_counter(n, fmt)
    }))
}

/// Walk `template` substituting every `%N` (N = 1..=9) with the result of
/// `fmt_at(N - 1)`. `%` followed by a non-digit passes through verbatim;
/// trailing `%` at EOS also passes through.
fn substitute(template: &str, mut fmt_at: impl FnMut(u8) -> String) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%'
            && let Some(&n) = chars.peek()
            && let Some(d) = n.to_digit(10)
            && (1..=9).contains(&d)
        {
            chars.next();
            out.push_str(&fmt_at((d - 1) as u8));
        } else {
            out.push(c);
        }
    }
    out
}

/// Format a counter according to `numFmt`. `Other(_)` falls back to
/// decimal — round-trip preserves the original `lvl_text` regardless,
/// but the rendered marker stays sensible.
pub fn format_counter(n: i32, fmt: &NumFmt) -> String {
    match fmt {
        NumFmt::Decimal | NumFmt::Other(_) => n.to_string(),
        NumFmt::DecimalZero => format!("{n:02}"),
        NumFmt::LowerLetter => letter(n, false),
        NumFmt::UpperLetter => letter(n, true),
        NumFmt::LowerRoman => roman(n, false),
        NumFmt::UpperRoman => roman(n, true),
        NumFmt::Bullet | NumFmt::None => String::new(),
    }
}

/// Alphabetic counter: 1→A, 26→Z, 27→AA, 52→AZ, 53→BA. Word emits
/// the next-cycle uppercase repeated-letter form (1 → A, 27 → AA, not AB),
/// so this is *not* a base-26 conversion — repeated letters are an
/// off-by-one cycle.
fn letter(n: i32, upper: bool) -> String {
    if n < 1 {
        return n.to_string();
    }
    let base = if upper { b'A' } else { b'a' };
    /* Word's lowerLetter/upperLetter format. Per ECMA-376 §17.18.59,
    repeat the same letter `(n - 1) / 26 + 1` times. */
    let cycle = ((n - 1) / 26) as usize + 1;
    let ch = base + ((n - 1) % 26) as u8;
    std::iter::repeat_n(ch as char, cycle).collect()
}

/// Roman numeral 1..=3999. Out-of-range falls back to decimal.
fn roman(mut n: i32, upper: bool) -> String {
    if !(1..=3999).contains(&n) {
        return n.to_string();
    }
    const UPPER: [(i32, &str); 13] = [
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
    for (v, s) in UPPER {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    if upper { out } else { out.to_ascii_lowercase() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::numbering::{AbstractNum, LvlDef, NumInstance};

    fn lvl(ilvl: u8, fmt: NumFmt, text: &str, start: i32) -> LvlDef {
        LvlDef {
            ilvl,
            start,
            num_fmt: fmt,
            lvl_text: text.into(),
            ..LvlDef::default()
        }
    }

    fn defs_with(abstracts: Vec<AbstractNum>, nums: Vec<NumInstance>) -> NumberingDefinitions {
        let mut d = NumberingDefinitions::default();
        for a in abstracts {
            d.abstract_nums.insert(a.id, a);
        }
        for n in nums {
            d.num_instances.insert(n.num_id, n);
        }
        d
    }

    #[test]
    fn decimal_counter_increments() {
        let d = defs_with(
            vec![AbstractNum {
                id: 0,
                levels: vec![lvl(0, NumFmt::Decimal, "%1.", 1)],
            }],
            vec![NumInstance {
                num_id: 1,
                abstract_num_id: 0,
                overrides: vec![],
            }],
        );
        let mut paras: Vec<Paragraph> = (0..3)
            .map(|_| Paragraph {
                list_item: Some(ListItem { num_id: 1, ilvl: 0 }),
                ..Default::default()
            })
            .collect();
        resolve_markers(&mut paras, &d);
        assert_eq!(paras[0].resolved_marker.as_deref(), Some("1."));
        assert_eq!(paras[1].resolved_marker.as_deref(), Some("2."));
        assert_eq!(paras[2].resolved_marker.as_deref(), Some("3."));
    }

    #[test]
    fn deeper_level_resets_when_parent_increments() {
        let d = defs_with(
            vec![AbstractNum {
                id: 0,
                levels: vec![
                    lvl(0, NumFmt::Decimal, "%1.", 1),
                    lvl(1, NumFmt::Decimal, "%1.%2.", 1),
                ],
            }],
            vec![NumInstance {
                num_id: 1,
                abstract_num_id: 0,
                overrides: vec![],
            }],
        );
        let mk = |ilvl: u8| Paragraph {
            list_item: Some(ListItem { num_id: 1, ilvl }),
            ..Default::default()
        };
        let mut paras = vec![mk(0), mk(1), mk(1), mk(0), mk(1)];
        resolve_markers(&mut paras, &d);
        let got: Vec<_> = paras.iter().map(|p| p.resolved_marker.as_deref()).collect();
        assert_eq!(
            got,
            vec![
                Some("1."),
                Some("1.1."),
                Some("1.2."),
                Some("2."),
                Some("2.1.")
            ]
        );
    }

    #[test]
    fn bullet_uses_literal_lvl_text() {
        let d = defs_with(
            vec![AbstractNum {
                id: 0,
                levels: vec![lvl(0, NumFmt::Bullet, "*", 1)],
            }],
            vec![NumInstance {
                num_id: 1,
                abstract_num_id: 0,
                overrides: vec![],
            }],
        );
        let mut paras: Vec<Paragraph> = (0..3)
            .map(|_| Paragraph {
                list_item: Some(ListItem { num_id: 1, ilvl: 0 }),
                ..Default::default()
            })
            .collect();
        resolve_markers(&mut paras, &d);
        assert!(
            paras
                .iter()
                .all(|p| p.resolved_marker.as_deref() == Some("*"))
        );
    }

    #[test]
    fn lower_letter_cycles_repeated_letters() {
        assert_eq!(format_counter(1, &NumFmt::LowerLetter), "a");
        assert_eq!(format_counter(26, &NumFmt::LowerLetter), "z");
        assert_eq!(format_counter(27, &NumFmt::LowerLetter), "aa");
        assert_eq!(format_counter(52, &NumFmt::LowerLetter), "zz");
        assert_eq!(format_counter(53, &NumFmt::LowerLetter), "aaa");
    }

    #[test]
    fn roman_basic_values() {
        assert_eq!(format_counter(4, &NumFmt::UpperRoman), "IV");
        assert_eq!(format_counter(9, &NumFmt::UpperRoman), "IX");
        assert_eq!(format_counter(1990, &NumFmt::UpperRoman), "MCMXC");
        assert_eq!(format_counter(7, &NumFmt::LowerRoman), "vii");
    }

    #[test]
    fn percent_followed_by_non_digit_passes_through() {
        let out = substitute("100%", |_| "X".into());
        assert_eq!(out, "100%");
    }

    #[test]
    fn percent_at_end_passes_through() {
        let out = substitute("trail%", |_| "X".into());
        assert_eq!(out, "trail%");
    }

    #[test]
    fn start_override_seeds_counter() {
        use crate::parts::numbering::LvlOverride;
        let d = defs_with(
            vec![AbstractNum {
                id: 0,
                levels: vec![lvl(0, NumFmt::Decimal, "%1)", 1)],
            }],
            vec![NumInstance {
                num_id: 1,
                abstract_num_id: 0,
                overrides: vec![LvlOverride {
                    ilvl: 0,
                    start_override: Some(7),
                    lvl: None,
                }],
            }],
        );
        let mut paras: Vec<Paragraph> = (0..2)
            .map(|_| Paragraph {
                list_item: Some(ListItem { num_id: 1, ilvl: 0 }),
                ..Default::default()
            })
            .collect();
        resolve_markers(&mut paras, &d);
        assert_eq!(paras[0].resolved_marker.as_deref(), Some("7)"));
        assert_eq!(paras[1].resolved_marker.as_deref(), Some("8)"));
    }

    #[test]
    fn dangling_num_id_drops_marker_silently() {
        let d = NumberingDefinitions::default();
        let mut paras = vec![Paragraph {
            list_item: Some(ListItem {
                num_id: 999,
                ilvl: 0,
            }),
            ..Default::default()
        }];
        resolve_markers(&mut paras, &d);
        assert_eq!(paras[0].resolved_marker, None);
    }
}
