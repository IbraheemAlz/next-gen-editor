//! Bundled fallback font stack.
//!
//! This is a *stress-test* rasterizer, not a typography check (issue #88's
//! "Out of scope: Visual correctness judgment" — that's the differential
//! oracle epic). Real documents reference arbitrary installed fonts we don't
//! have; we don't need to match them, only to shape + lay out + PDF-export
//! whatever script the text actually is without crashing. Three faces
//! already vendored under `ts/fonts/` (used by
//! `crates/format-pdf/examples/export_profiles.rs` the same way) cover
//! Latin + Arabic, which is this project's two working scripts.

use text_pipeline::FontStack;

/// Build the shared fallback `FontStack` - the same one the browser shell
/// effectively uses (Latin -> Liberation Sans, Arabic -> Amiri), via the
/// shared `harness-fonts` helper (issue #464).
pub fn bundled_stack() -> FontStack {
    harness_fonts::harness_stack()
}

/// Issue #329 — the harness stack plus the editor's substitute faces
/// (`ts/public/fonts.json` `substitutes`, shared through `harness-fonts`):
/// the stack the font-substitution counter resolves each document's
/// families against, so the census says what the editor would substitute.
pub fn editor_stack() -> FontStack {
    harness_fonts::harness_stack_with_substitutes()
}

/// [`editor_stack`], parsed once per process.
pub fn editor_stack_cached() -> &'static FontStack {
    static STACK: std::sync::OnceLock<FontStack> = std::sync::OnceLock::new();
    STACK.get_or_init(editor_stack)
}

/// Issue #476 - give a canvas-less production engine the editor's boot fonts.
///
/// `Engine::new_headless` holds one Liberation Sans face (`fuzz-latin`), so
/// the production-layout stage laid every document out in it: a Calibri
/// document measured in Liberation Sans (wider) and an Arabic run in Latin
/// glyphs, and the page totals did not describe what the editor shows. This
/// loads the same faces the shell loads (`harness_fonts::editor_faces`, the
/// `fonts.json` `defaults` + `substitutes`) through the real
/// `Command::LoadFont`, then declares the shell's per-script defaults
/// (`Command::SetFontPriority`, `harness_fonts::DEFAULTS_BY_SCRIPT`), the
/// same two steps the shell's boot sequence runs. Each face is parsed (and,
/// by `LoadedFont::parse`, leaked) once per engine - fine for the
/// one-document worker process this runs in.
pub fn register_editor_fonts(engine: &mut engine_wasm::Engine) -> Result<(), String> {
    use bridge::{Command, Event, FontPriority, Script as Wire};
    use text_pipeline::Script;

    for (id, bytes) in harness_fonts::editor_faces() {
        let loaded = engine.apply_sync(Command::LoadFont {
            id: id.to_string(),
            bytes: bytes.to_vec(),
        });
        if !matches!(loaded, Event::FontLoaded { .. }) {
            return Err(format!("LoadFont `{id}` answered {loaded:?}"));
        }
    }
    let priorities: Vec<FontPriority> = harness_fonts::DEFAULTS_BY_SCRIPT
        .iter()
        .filter_map(|(script, ids)| {
            let script = match script {
                Script::Latin => Wire::Latin,
                Script::Arabic => Wire::Arabic,
                /* Issue #490 - the explicit script-neutral default. */
                Script::Common => Wire::Common,
                Script::Other => return None,
            };
            Some(FontPriority {
                script,
                ids: ids.iter().map(|id| id.to_string()).collect(),
            })
        })
        .collect();
    match engine.apply_sync(Command::SetFontPriority { priorities }) {
        Event::FontPriorityApplied { .. } => Ok(()),
        other => Err(format!("SetFontPriority answered {other:?}")),
    }
}
