//! Style cascade resolver (Phase 3 / OOXML §17.7.2).
//!
//! Folds the paragraph + run cascade described by [`crate::parts::styles`]
//! into the engine's flat `ParaProperties` / `SpanStyle`. The reader applies
//! this once per `<w:p>` and `<w:r>`; the engine never sees a `pStyle` /
//! `rStyle` reference.
//!
//! ## Cascade order (root-first)
//!
//! 1. `<w:docDefaults>/<w:pPrDefault>` / `<w:rPrDefault>`.
//! 2. The paragraph style chain — walked from the most-distant ancestor
//!    toward the leaf via repeated `<w:basedOn>` hops, then folded
//!    leaf-last so the child overrides the parent.
//! 3. (Runs only) The character style chain, applied on top of the
//!    paragraph style's run formatting.
//! 4. Direct `<w:pPr>` (paragraph) / `<w:rPr>` (run).
//!
//! ## Cycle protection
//!
//! `<w:basedOn>` chains are capped at [`MAX_CHAIN`] entries and tracked with
//! a visited-id set: a self-loop or any cycle aborts at the first repeat,
//! and depth overflow is clamped silently. Malicious or corrupted
//! stylesheets cannot loop the parser.

use crate::parts::styles::{StyleDef, StyleKind, StyleTable};
use engine::{ParaProperties, SpanStyle};
use std::collections::HashSet;

/// Cap on `<w:basedOn>` chain depth. Matches the ECMA-376 §17.7.4.5
/// implementation guidance ("no more than 10 levels"). Anything beyond is
/// almost certainly a malformed stylesheet; we silently clamp.
pub const MAX_CHAIN: usize = 10;

pub struct StyleResolver<'a> {
    table: &'a StyleTable,
}

impl<'a> StyleResolver<'a> {
    pub fn new(table: &'a StyleTable) -> Self {
        Self { table }
    }

    /// Resolve the **paragraph** cascade for a `<w:p>` carrying optional
    /// `pStyle` / direct `<w:pPr>`. Returns:
    ///
    /// - the effective `ParaProperties` (doc defaults + para-style chain +
    ///   direct `<w:pPr>`),
    /// - the **baseline** `SpanStyle` runs inherit from this paragraph (doc
    ///   defaults + para-style's `<w:rPr>` chain). Pass it into
    ///   [`Self::resolve_run`] for every `<w:r>` inside the paragraph.
    pub fn resolve_paragraph(
        &self,
        p_style: Option<&str>,
        direct_ppr: ParaProperties,
        direct_rpr: SpanStyle,
    ) -> (ParaProperties, SpanStyle) {
        let mut para = self.table.defaults.para.clone();
        let mut run = self.table.defaults.run;
        if let Some(id) = p_style {
            let chain = self.collect_chain(id, StyleKind::Paragraph);
            /* Chain is leaf-first; reverse so the most distant ancestor
            applies first and the leaf overrides last. */
            for s in chain.iter().rev() {
                para = para.merged_with(s.para.clone());
                run = run.merged_with(s.run);
            }
        }
        /* Direct `<w:pPr>` is the highest specificity for paragraph
        properties; the paragraph-mark `<w:pPr>/<w:rPr>` (direct_rpr) drops
        onto the inherited run baseline. */
        para = para.merged_with(direct_ppr);
        run = run.merged_with(direct_rpr);
        (para, run)
    }

    /// Resolve the **run** cascade for a `<w:r>` inside a paragraph.
    ///
    /// `baseline` is the second tuple value from
    /// [`Self::resolve_paragraph`]. `r_style` is the optional `<w:rStyle>`
    /// reference; `direct_rpr` is the `<w:rPr>` directly on this run.
    pub fn resolve_run(
        &self,
        baseline: SpanStyle,
        r_style: Option<&str>,
        direct_rpr: SpanStyle,
    ) -> SpanStyle {
        let mut run = baseline;
        if let Some(id) = r_style {
            let chain = self.collect_chain(id, StyleKind::Character);
            for s in chain.iter().rev() {
                run = run.merged_with(s.run);
            }
        }
        run.merged_with(direct_rpr)
    }

    /// Walk the `<w:basedOn>` chain leaf → root. Stops on cycle (visited),
    /// missing parent, kind mismatch, or [`MAX_CHAIN`] depth. Returns
    /// **leaf-first**; callers reverse for cascade application.
    fn collect_chain(&self, leaf_id: &str, kind: StyleKind) -> Vec<&StyleDef> {
        let mut chain: Vec<&StyleDef> = Vec::new();
        let mut visited: HashSet<&str> = HashSet::new();
        let mut current: Option<&str> = Some(leaf_id);
        while let Some(id) = current {
            if chain.len() >= MAX_CHAIN {
                break;
            }
            if !visited.insert(id) {
                /* Cycle. The chain so far is still safe to apply because
                the visited entry has already contributed once. */
                break;
            }
            let Some(def) = self.table.by_id.get(id) else {
                /* Dangling parent reference — Word tolerates these. */
                break;
            };
            /* Kind mismatch (e.g. paragraph style basedOn a table style) is
            illegal per spec; defensively stop the walk. */
            if def.kind != kind {
                break;
            }
            chain.push(def);
            current = def.based_on.as_deref();
        }
        chain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parts::styles::{DocDefaults, StyleDef, StyleKind, StyleTable};
    use engine::{Alignment, SpanStyle};

    fn pstyle(id: &str, based_on: Option<&str>, run: SpanStyle) -> StyleDef {
        StyleDef {
            id: id.into(),
            kind: StyleKind::Paragraph,
            based_on: based_on.map(String::from),
            para: ParaProperties::default(),
            run,
        }
    }

    fn table_with(styles: Vec<StyleDef>, defaults: DocDefaults) -> StyleTable {
        let mut t = StyleTable {
            defaults,
            ..Default::default()
        };
        for s in styles {
            t.by_id.insert(s.id.clone(), s);
        }
        t
    }

    #[test]
    fn doc_defaults_apply_when_no_style_referenced() {
        let mut defaults = DocDefaults::default();
        defaults.para.alignment = Some(Alignment::Center);
        let t = table_with(vec![], defaults);
        let r = StyleResolver::new(&t);
        let (p, _) = r.resolve_paragraph(None, ParaProperties::default(), SpanStyle::default());
        assert_eq!(p.alignment, Some(Alignment::Center));
    }

    #[test]
    fn based_on_chain_folds_root_first() {
        /* Base sets bold; Child sets italic basedOn Base. Resolved style
        carries both, with Child winning on any conflict. */
        let base = pstyle(
            "Base",
            None,
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        let child = pstyle(
            "Child",
            Some("Base"),
            SpanStyle {
                italic: Some(true),
                ..Default::default()
            },
        );
        let t = table_with(vec![base, child], DocDefaults::default());
        let r = StyleResolver::new(&t);
        let (_, run) = r.resolve_paragraph(
            Some("Child"),
            ParaProperties::default(),
            SpanStyle::default(),
        );
        assert_eq!(run.bold, Some(true));
        assert_eq!(run.italic, Some(true));
    }

    #[test]
    fn child_overrides_parent_on_conflict() {
        let base = pstyle(
            "Base",
            None,
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        let child = pstyle(
            "Child",
            Some("Base"),
            SpanStyle {
                bold: Some(false),
                ..Default::default()
            },
        );
        let t = table_with(vec![base, child], DocDefaults::default());
        let r = StyleResolver::new(&t);
        let (_, run) = r.resolve_paragraph(
            Some("Child"),
            ParaProperties::default(),
            SpanStyle::default(),
        );
        assert_eq!(run.bold, Some(false));
    }

    #[test]
    fn cycle_does_not_loop_forever() {
        /* A → B → A. Both contribute once; resolver stops on the second
        visit. */
        let a = pstyle(
            "A",
            Some("B"),
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        let b = pstyle(
            "B",
            Some("A"),
            SpanStyle {
                italic: Some(true),
                ..Default::default()
            },
        );
        let t = table_with(vec![a, b], DocDefaults::default());
        let r = StyleResolver::new(&t);
        let (_, run) =
            r.resolve_paragraph(Some("A"), ParaProperties::default(), SpanStyle::default());
        assert_eq!(run.bold, Some(true));
        assert_eq!(run.italic, Some(true));
    }

    #[test]
    fn self_cycle_is_safe() {
        let self_ref = pstyle(
            "X",
            Some("X"),
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        let t = table_with(vec![self_ref], DocDefaults::default());
        let r = StyleResolver::new(&t);
        let (_, run) =
            r.resolve_paragraph(Some("X"), ParaProperties::default(), SpanStyle::default());
        assert_eq!(run.bold, Some(true));
    }

    #[test]
    fn dangling_parent_reference_is_tolerated() {
        let leaf = pstyle(
            "Leaf",
            Some("Missing"),
            SpanStyle {
                italic: Some(true),
                ..Default::default()
            },
        );
        let t = table_with(vec![leaf], DocDefaults::default());
        let r = StyleResolver::new(&t);
        let (_, run) = r.resolve_paragraph(
            Some("Leaf"),
            ParaProperties::default(),
            SpanStyle::default(),
        );
        assert_eq!(run.italic, Some(true));
    }

    #[test]
    fn direct_rpr_wins_over_style() {
        let style = pstyle(
            "S",
            None,
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        let t = table_with(vec![style], DocDefaults::default());
        let r = StyleResolver::new(&t);
        let direct = SpanStyle {
            bold: Some(false),
            ..Default::default()
        };
        let (_, baseline) =
            r.resolve_paragraph(Some("S"), ParaProperties::default(), SpanStyle::default());
        let resolved = r.resolve_run(baseline, None, direct);
        assert_eq!(resolved.bold, Some(false));
    }

    #[test]
    fn character_style_overlays_paragraph_baseline() {
        let pstyle_def = pstyle(
            "P",
            None,
            SpanStyle {
                bold: Some(true),
                ..Default::default()
            },
        );
        let cstyle = StyleDef {
            id: "Emph".into(),
            kind: StyleKind::Character,
            based_on: None,
            para: ParaProperties::default(),
            run: SpanStyle {
                italic: Some(true),
                ..Default::default()
            },
        };
        let t = table_with(vec![pstyle_def, cstyle], DocDefaults::default());
        let r = StyleResolver::new(&t);
        let (_, baseline) =
            r.resolve_paragraph(Some("P"), ParaProperties::default(), SpanStyle::default());
        let resolved = r.resolve_run(baseline, Some("Emph"), SpanStyle::default());
        assert_eq!(resolved.bold, Some(true));
        assert_eq!(resolved.italic, Some(true));
    }

    #[test]
    fn chain_depth_capped_at_max() {
        let mut styles = Vec::new();
        for i in 0..30 {
            let parent = if i + 1 < 30 {
                Some(format!("S{}", i + 1))
            } else {
                None
            };
            styles.push(StyleDef {
                id: format!("S{i}"),
                kind: StyleKind::Paragraph,
                based_on: parent,
                para: ParaProperties::default(),
                run: SpanStyle::default(),
            });
        }
        let t = table_with(styles, DocDefaults::default());
        let r = StyleResolver::new(&t);
        let chain = r.collect_chain("S0", StyleKind::Paragraph);
        assert_eq!(chain.len(), MAX_CHAIN);
    }
}
