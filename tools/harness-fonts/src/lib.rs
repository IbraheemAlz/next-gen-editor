//! The ONE font stack the native QA harnesses lay out with (issue #464).
//!
//! The browser shell shapes Latin runs with the Latin face and Arabic runs
//! with Amiri (per-script fallback in `FontStack`). The harnesses used to
//! register every shipped face under ids sorted alphabetically, so Amiri
//! (`amiri` < `liberation`) became the primary face for LATIN runs too -
//! and its tall ascent/descent inflated every line height and page count.
//!
//! `FontStack::from_faces` orders the faces that cover a script by id, and
//! Amiri covers Latin as well, so the Latin face must carry the id that
//! sorts first. The ids below encode that; do not rename them without
//! re-running the unit tests.

use std::collections::HashMap;
use std::sync::Arc;
use text_pipeline::{FontStack, LoadedFont};

/// Latin face id. Sorts before [`ARABIC_ID`]: Amiri also covers Latin, and
/// the stack prefers the lowest id within a script.
pub const LATIN_ID: &str = "liberation-sans";
/// Arabic face id (Amiri).
pub const ARABIC_ID: &str = "naskh-amiri";

const LIBERATION_SANS: &[u8] = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf");
const AMIRI: &[u8] = include_bytes!("../../../ts/fonts/Amiri-Regular.ttf");

/// Build the harness `FontStack`: Latin -> Liberation Sans, Arabic -> Amiri,
/// with Liberation Sans as the fallback-chain root.
pub fn harness_stack() -> FontStack {
    let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
    let latin = LoadedFont::parse(LATIN_ID.into(), LIBERATION_SANS.to_vec())
        .expect("bundled LiberationSans-Regular.ttf must parse");
    faces.insert(LATIN_ID.to_string(), Arc::new(latin));
    let arabic = LoadedFont::parse(ARABIC_ID.into(), AMIRI.to_vec())
        .expect("bundled Amiri-Regular.ttf must parse");
    faces.insert(ARABIC_ID.to_string(), Arc::new(arabic));
    FontStack::from_faces(faces, LATIN_ID)
}

/// Issue #329 — the editor's substitute faces (`ts/public/fonts.json`
/// `substitutes`): what the font stack's substitution table maps a
/// document's families to. Their ids sort AFTER the base faces in every
/// script they cover ([`LATIN_ID`] < `sub-*`, [`ARABIC_ID`] < `sub-*`), so
/// the per-script fallback for a run that names no family is unchanged
/// (#464); a named family reaches them through `FontStack::resolve_family`
/// by their `name`-table families (Carlito, Noto Naskh Arabic, ...).
const SUBSTITUTES: &[(&str, &[u8])] = &[
    (
        "sub-noto-naskh",
        include_bytes!("../../../ts/public/fonts/NotoNaskhArabic-Regular.ttf"),
    ),
    (
        "sub-carlito",
        include_bytes!("../../../ts/public/fonts/Carlito-Regular.ttf"),
    ),
    (
        "sub-caladea",
        include_bytes!("../../../ts/public/fonts/Caladea-Regular.ttf"),
    ),
    (
        "sub-liberation-serif",
        include_bytes!("../../../ts/public/fonts/LiberationSerif-Regular.ttf"),
    ),
    (
        "sub-liberation-mono",
        include_bytes!("../../../ts/public/fonts/LiberationMono-Regular.ttf"),
    ),
    (
        "sub-gelasio",
        include_bytes!("../../../ts/public/fonts/Gelasio-Regular.ttf"),
    ),
    (
        "sub-selawik",
        include_bytes!("../../../ts/public/fonts/Selawik-Regular.ttf"),
    ),
];

/// [`harness_stack`] plus the editor's substitute faces (issue #329): a run
/// naming no family resolves exactly as in [`harness_stack`]; a run naming
/// Calibri, Times New Roman, Simplified Arabic, ... lays out in the face the
/// editor substitutes.
pub fn harness_stack_with_substitutes() -> FontStack {
    let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
    for (id, bytes) in [(LATIN_ID, LIBERATION_SANS), (ARABIC_ID, AMIRI)]
        .into_iter()
        .chain(SUBSTITUTES.iter().copied())
    {
        let face = LoadedFont::parse(id.to_string(), bytes.to_vec())
            .unwrap_or_else(|e| panic!("bundled face `{id}` must parse: {e:?}"));
        faces.insert(id.to_string(), Arc::new(face));
    }
    FontStack::from_faces(faces, LATIN_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use text_pipeline::Script;

    /// Issue #329 — the substitutes never change the unnamed fallback, and a
    /// named family reaches its substitute.
    #[test]
    fn substitutes_keep_the_fallback_and_serve_named_families() {
        let stack = harness_stack_with_substitutes();
        let id = |script, family: Option<&str>| {
            stack
                .resolve(script, family, false, false)
                .map(|(id, _, _)| id.clone())
                .expect("a face")
        };
        assert_eq!(id(Script::Latin, None), LATIN_ID);
        assert_eq!(id(Script::Arabic, None), ARABIC_ID);
        assert_eq!(id(Script::Latin, Some("calibri")), "sub-carlito");
        assert_eq!(
            id(Script::Arabic, Some("simplified-arabic")),
            "sub-noto-naskh"
        );
        assert_eq!(id(Script::Latin, Some("arial")), LATIN_ID);
    }

    #[test]
    fn latin_run_resolves_to_the_latin_face() {
        let stack = harness_stack();
        let (id, _, _) = stack
            .resolve(Script::Latin, None, false, false)
            .expect("a face");
        assert_eq!(id, LATIN_ID);
    }

    #[test]
    fn arabic_run_resolves_to_amiri() {
        let stack = harness_stack();
        let (id, face, _) = stack
            .resolve(Script::Arabic, None, false, false)
            .expect("a face");
        assert_eq!(id, ARABIC_ID);
        assert!(face.covers('\u{0628}'));
    }
}
