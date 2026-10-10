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
