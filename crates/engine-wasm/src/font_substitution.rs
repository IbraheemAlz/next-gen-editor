//! Issue #329 — which font substitutions the open document's layout makes.
//!
//! The font stack substitutes a family the document names but the engine
//! does not have (`text_pipeline::FontStack::resolve_family`, the
//! `SUBSTITUTIONS` table). This module reports those substitutions to the
//! shell — `Event::DocumentLoaded.substituted` / `Event::FontLoaded.
//! substituted`, the Dev HUD's list and the telemetry count — by walking
//! the document's style spans exactly the way layout resolves them: every
//! shaped piece (script class × span) asks the stack for the span's family
//! in that piece's script slot, and a `FamilyMatch::Substituted` answer is
//! one (family, slot) entry.

use super::*;
use std::collections::BTreeMap;
use text_pipeline::{FamilyMatch, ScriptClass, Substitution, family_key, segment_by_script_class};

impl Engine {
    /// Issue #329 — the substitutions layout makes for the current
    /// document with the faces loaded now, sorted by (slot, family).
    /// Empty while no face is loaded.
    pub(crate) fn font_substitutions(&self) -> Vec<bridge::FontSubstitution> {
        if self.fonts.is_empty() {
            return Vec::new();
        }
        let primary = self.layout_cfg.as_ref().map_or("", |c| c.font_id.as_str());
        let stack = FontStack::from_faces(self.fonts.clone(), primary);
        document_font_substitutions(self.undo.current(), &stack)
    }
}

/// Issue #329 — the (family, script slot) substitutions `stack` makes for
/// the text of `doc`: the body (table cells included, nested tables down
/// to [`MAX_TABLE_LAYOUT_DEPTH`]), every header / footer story and every
/// footnote / endnote story. Text-box stories are not walked.
pub(crate) fn document_font_substitutions(
    doc: &DocumentTree,
    stack: &FontStack,
) -> Vec<bridge::FontSubstitution> {
    let sctx = StyleContext::of(doc);
    let mut found: BTreeMap<(u8, String), bridge::FontSubstitution> = BTreeMap::new();
    let mut visit = |para: &engine::Paragraph| {
        for span in build_style_spans(para, sctx, 11.0, [0, 0, 0, 255], 1.0) {
            let text = para
                .text
                .get(span.start as usize..span.end as usize)
                .unwrap_or("");
            for (_, script, complex) in segment_by_script_class(text) {
                let face = span.face_for(complex);
                let Some(family) = face.font_family else {
                    continue;
                };
                let Some(r) = stack.resolve_family(family, script, face.bold, face.italic) else {
                    continue;
                };
                let FamilyMatch::Substituted(row) = r.matched else {
                    continue;
                };
                let slot_order = matches!(row.class, ScriptClass::ComplexScript) as u8;
                found
                    .entry((slot_order, row.family.to_string()))
                    .or_insert_with(|| substitution_entry(row, r.id, r.face));
            }
        }
    };
    visit_blocks(doc.blocks.iter(), 0, &mut visit);
    let mut stories: Vec<(&String, &Vec<engine::Block>)> =
        doc.headers.iter().chain(doc.footers.iter()).collect();
    stories.sort_by(|a, b| a.0.cmp(b.0));
    for (_, blocks) in stories {
        visit_blocks(blocks.iter(), 0, &mut visit);
    }
    for story in doc
        .footnote_stories
        .values()
        .chain(doc.endnote_stories.values())
    {
        visit_blocks(story.body.iter(), 0, &mut visit);
    }
    found.into_values().collect()
}

/// The wire entry for a substitution `row` served by the loaded face `id`.
fn substitution_entry(row: &Substitution, id: &str, face: &LoadedFont) -> bridge::FontSubstitution {
    /* The row's own spelling of the substitute that matched (by id or by
    the face's `name`-table family), else the face's first family name. */
    let id_key = family_key(id);
    let substitute = row
        .substitutes
        .iter()
        .find(|name| {
            let key = family_key(name);
            key == id_key || face.family_names().iter().any(|n| family_key(n) == key)
        })
        .map(|name| name.to_string())
        .or_else(|| face.family_names().first().cloned())
        .unwrap_or_else(|| id.to_string());
    bridge::FontSubstitution {
        family: row.family.to_string(),
        slot: match row.class {
            ScriptClass::Latin => bridge::FontSlot::Latin,
            ScriptClass::ComplexScript => bridge::FontSlot::ComplexScript,
        },
        substitute,
        substitute_id: id.to_string(),
        metric_compatible: row.metric_compatible,
    }
}

/// Depth-first visit of every paragraph in `blocks`, descending into table
/// cells (bounded like table layout, issue #318).
fn visit_blocks<'a>(
    blocks: impl Iterator<Item = &'a engine::Block>,
    depth: u32,
    f: &mut impl FnMut(&engine::Paragraph),
) {
    for block in blocks {
        match block {
            engine::Block::Paragraph(p) => f(p),
            engine::Block::Table(t) if depth < MAX_TABLE_LAYOUT_DEPTH => {
                for row in &t.rows {
                    for cell in &row.cells {
                        visit_blocks(cell.blocks.iter(), depth + 1, f);
                    }
                }
            }
            engine::Block::Table(_) => {}
        }
    }
}
