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

use std::collections::HashMap;
use std::sync::Arc;
use text_pipeline::{FontStack, LoadedFont};

const LIBERATION_SANS: &[u8] = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf");
const NOTO_NASKH_ARABIC: &[u8] = include_bytes!("../../../ts/fonts/NotoNaskhArabic-Regular.ttf");

/// Build the shared fallback `FontStack`. `"liberation"` is the primary /
/// fallback-chain root; Arabic text resolves to the Noto Naskh face via
/// `FontStack::from_faces`'s coverage probe (it checks U+0628 BEH).
pub fn bundled_stack() -> FontStack {
    let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
    let liberation = LoadedFont::parse("liberation".into(), LIBERATION_SANS.to_vec())
        .expect("bundled LiberationSans-Regular.ttf must parse");
    faces.insert("liberation".to_string(), Arc::new(liberation));
    let noto_naskh = LoadedFont::parse("noto-naskh-arabic".into(), NOTO_NASKH_ARABIC.to_vec())
        .expect("bundled NotoNaskhArabic-Regular.ttf must parse");
    faces.insert("noto-naskh-arabic".to_string(), Arc::new(noto_naskh));
    FontStack::from_faces(faces, "liberation")
}

/// Issue #329 — the faces the editor's shell boots with (`ts/public/
/// fonts.json` `defaults` + `substitutes`), under their manifest ids: the
/// stack the font-substitution counter resolves each document's families
/// against, so the census says what the editor would substitute.
pub fn editor_stack() -> FontStack {
    const FACES: &[(&str, &[u8])] = &[
        (
            "amiri",
            include_bytes!("../../../ts/public/fonts/Amiri-Regular.ttf"),
        ),
        (
            "liberation",
            include_bytes!("../../../ts/public/fonts/LiberationSans-Regular.ttf"),
        ),
        (
            "noto-naskh",
            include_bytes!("../../../ts/public/fonts/NotoNaskhArabic-Regular.ttf"),
        ),
        (
            "carlito",
            include_bytes!("../../../ts/public/fonts/Carlito-Regular.ttf"),
        ),
        (
            "caladea",
            include_bytes!("../../../ts/public/fonts/Caladea-Regular.ttf"),
        ),
        (
            "liberation-serif",
            include_bytes!("../../../ts/public/fonts/LiberationSerif-Regular.ttf"),
        ),
        (
            "liberation-mono",
            include_bytes!("../../../ts/public/fonts/LiberationMono-Regular.ttf"),
        ),
        (
            "gelasio",
            include_bytes!("../../../ts/public/fonts/Gelasio-Regular.ttf"),
        ),
        (
            "selawik",
            include_bytes!("../../../ts/public/fonts/Selawik-Regular.ttf"),
        ),
    ];
    let faces: HashMap<String, Arc<LoadedFont>> = FACES
        .iter()
        .map(|(id, bytes)| {
            let face = LoadedFont::parse((*id).to_string(), bytes.to_vec())
                .expect("bundled editor face must parse");
            ((*id).to_string(), Arc::new(face))
        })
        .collect();
    FontStack::from_faces(faces, "liberation")
}

/// [`editor_stack`], parsed once per process.
pub fn editor_stack_cached() -> &'static FontStack {
    static STACK: std::sync::OnceLock<FontStack> = std::sync::OnceLock::new();
    STACK.get_or_init(editor_stack)
}
