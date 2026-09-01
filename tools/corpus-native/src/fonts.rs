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
