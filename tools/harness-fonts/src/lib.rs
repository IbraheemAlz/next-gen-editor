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

#[cfg(test)]
mod tests {
    use super::*;
    use text_pipeline::Script;

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
