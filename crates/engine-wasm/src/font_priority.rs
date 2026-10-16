//! Issue #472 — the per-script default faces the shell declares
//! (`Command::SetFontPriority`) and the one place the engine builds its
//! `FontStack`.
//!
//! A run that names no font family (or one nothing resolves) takes its
//! face from the stack's per-script default. By id alone that is the first
//! covering face - and a dual-script face such as Amiri (`amiri` <
//! `liberation`) used to win Latin text in the interactive editor, with its
//! glyphs and its tall line metrics. The shell now declares which loaded
//! face each script defaults to (`ts/public/fonts.json`
//! `defaults_by_script`), and `text_pipeline::FontStack::with_priority`
//! consults that list before the id-sorted fallback.

use super::*;

impl Engine {
    /// The font stack layout resolves faces with: the loaded faces,
    /// `primary` as the fallback root, and the shell's declared per-script
    /// default faces consulted before the id-sorted fallback. EVERY stack
    /// built from the engine's font map goes through here, so the
    /// substitution report, the toolbar read-backs and layout agree on
    /// which face a bare run gets.
    pub(crate) fn font_stack(&self, primary: &str) -> FontStack {
        let mut stack = FontStack::from_faces(self.fonts.clone(), primary);
        for (script, ids) in &self.font_priority {
            stack.set_priority(*script, ids);
        }
        stack
    }

    /// `Command::SetFontPriority`: replace the declared per-script default
    /// faces and answer the EFFECTIVE lists (loaded ids that cover the
    /// script). The wire scripts the font stack does not distinguish
    /// (Greek, Hebrew, Han, ...) share `Script::Other`, their lists
    /// concatenated in declared order.
    pub(crate) fn do_set_font_priority(&mut self, priorities: Vec<bridge::FontPriority>) -> Event {
        use text_pipeline::Script as S;
        let mut table: HashMap<S, Vec<String>> = HashMap::new();
        for entry in priorities {
            let script = match entry.script {
                bridge::Script::Latin => S::Latin,
                bridge::Script::Arabic => S::Arabic,
                bridge::Script::Common => S::Common,
                _ => S::Other,
            };
            table.entry(script).or_default().extend(entry.ids);
        }
        self.font_priority = table;
        /* Which face lays out a bare run changed: drop the cached boxes
        (the same stance as `LoadFont`; the shell repaints once fonts and
        priorities are in). */
        self.layout_cache.get_mut().clear();
        self.invalidate_layout_snapshot();
        let primary = self.layout_cfg.as_ref().map_or("", |c| c.font_id.as_str());
        let stack = self.font_stack(primary);
        let priorities = [
            (S::Latin, bridge::Script::Latin),
            (S::Arabic, bridge::Script::Arabic),
            (S::Common, bridge::Script::Common),
            (S::Other, bridge::Script::Unknown),
        ]
        .into_iter()
        .filter_map(|(script, wire)| {
            let ids = stack.priority(script);
            (!ids.is_empty()).then(|| bridge::FontPriority {
                script: wire,
                ids: ids.to_vec(),
            })
        })
        .collect();
        Event::FontPriorityApplied { priorities }
    }
}
