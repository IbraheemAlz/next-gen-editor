//! The ONE font stack the native QA harnesses lay out with (issues #464,
//! #472).
//!
//! The browser shell shapes Latin runs that name no family with Liberation
//! Sans and Arabic runs with Amiri. The harnesses used to get that by
//! naming: `FontStack::from_faces` orders the faces covering a script by
//! id, and Amiri covers Latin too, so the Latin face had to carry an id
//! that sorted before Amiri's (and the substitutes a `sub-` prefix that
//! sorted after both). Issue #472 replaced the trick with the explicit
//! per-script priority (`FontStack::with_priority`), declared from the
//! SAME table the shell sends its engine (`ts/public/fonts.json`
//! `defaults_by_script`, pinned by `matches_the_shell_manifest`). The faces
//! are registered under their manifest ids, so a harness run and the
//! editor name every face alike and nothing depends on how an id sorts.

use std::collections::HashMap;
use std::sync::Arc;
use text_pipeline::{FontStack, LoadedFont, Script};

/// Latin default face id (`fonts.json` `liberation`, Liberation Sans).
pub const LATIN_ID: &str = "liberation";
/// Arabic default face id (`fonts.json` `amiri`, Amiri).
pub const ARABIC_ID: &str = "amiri";

/// The per-script defaults, in preference order - `fonts.json`
/// `defaults_by_script` (`latin`, `arabic`). An id the stack does not hold
/// is skipped, so [`harness_stack`] (no Noto Naskh) and
/// [`harness_stack_with_substitutes`] share the one table.
pub const DEFAULTS_BY_SCRIPT: &[(Script, &[&str])] = &[
    (Script::Latin, &[LATIN_ID]),
    (Script::Arabic, &[ARABIC_ID, "noto-naskh"]),
];

const LIBERATION_SANS: &[u8] = include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf");
const AMIRI: &[u8] = include_bytes!("../../../ts/fonts/Amiri-Regular.ttf");

/// Parse `faces` and build the stack: [`LATIN_ID`] is the fallback root and
/// [`DEFAULTS_BY_SCRIPT`] the per-script priority.
fn stack_of<'a>(faces: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> FontStack {
    let mut map: HashMap<String, Arc<LoadedFont>> = HashMap::new();
    for (id, bytes) in faces {
        let face = LoadedFont::parse(id.to_string(), bytes.to_vec())
            .unwrap_or_else(|e| panic!("bundled face `{id}` must parse: {e:?}"));
        map.insert(id.to_string(), Arc::new(face));
    }
    let mut stack = FontStack::from_faces(map, LATIN_ID);
    for (script, ids) in DEFAULTS_BY_SCRIPT {
        stack.set_priority(*script, ids);
    }
    stack
}

/// Build the harness `FontStack`: Latin -> Liberation Sans, Arabic -> Amiri,
/// with Liberation Sans as the fallback-chain root.
pub fn harness_stack() -> FontStack {
    stack_of([(LATIN_ID, LIBERATION_SANS), (ARABIC_ID, AMIRI)])
}

/// Issue #329 — the editor's substitute faces (`ts/public/fonts.json`
/// `substitutes`), under their manifest ids: what the font stack's
/// substitution table maps a document's families to. A run naming no family
/// resolves through [`DEFAULTS_BY_SCRIPT`] exactly as in [`harness_stack`]; a
/// named family reaches a substitute through `FontStack::resolve_family`
/// by its `name`-table family (Carlito, Noto Naskh Arabic, ...).
const SUBSTITUTES: &[(&str, &[u8])] = &[
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

/// [`harness_stack`] plus the editor's substitute faces (issue #329): a run
/// naming no family resolves exactly as in [`harness_stack`]; a run naming
/// Calibri, Times New Roman, Simplified Arabic, ... lays out in the face the
/// editor substitutes.
pub fn harness_stack_with_substitutes() -> FontStack {
    stack_of(editor_faces())
}

/// Issue #476 - every face the shell loads at boot (`fonts.json` `defaults`
/// + `substitutes`) as `(manifest id, TTF bytes)`: the two defaults, then the
/// substitutes. [`harness_stack_with_substitutes`] is built from exactly this
/// list; a harness that drives a real `Engine` (`corpus-native`'s production
/// layout) registers the same faces with `Command::LoadFont` and then sends
/// [`DEFAULTS_BY_SCRIPT`] as `Command::SetFontPriority`, so it lays out with
/// the stack the editor boots with instead of a lone Liberation Sans.
pub fn editor_faces() -> Vec<(&'static str, &'static [u8])> {
    [(LATIN_ID, LIBERATION_SANS), (ARABIC_ID, AMIRI)]
        .into_iter()
        .chain(SUBSTITUTES.iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_of(stack: &FontStack, script: Script, family: Option<&str>) -> String {
        stack
            .resolve(script, family, false, false)
            .map(|(id, _, _)| id.clone())
            .expect("a face")
    }

    /// Issue #329 — the substitutes never change the unnamed fallback, and a
    /// named family reaches its substitute.
    #[test]
    fn substitutes_keep_the_fallback_and_serve_named_families() {
        let stack = harness_stack_with_substitutes();
        assert_eq!(id_of(&stack, Script::Latin, None), LATIN_ID);
        assert_eq!(id_of(&stack, Script::Arabic, None), ARABIC_ID);
        assert_eq!(id_of(&stack, Script::Latin, Some("calibri")), "carlito");
        assert_eq!(
            id_of(&stack, Script::Arabic, Some("simplified-arabic")),
            "noto-naskh"
        );
        assert_eq!(id_of(&stack, Script::Latin, Some("arial")), LATIN_ID);
    }

    /// Issue #476 - the face list a harness hands an engine is the one the
    /// substitute stack is built from, and it is the shell's boot set.
    #[test]
    fn editor_faces_are_the_shells_boot_set() {
        let ids: Vec<&str> = editor_faces().iter().map(|(id, _)| *id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "an id is registered twice");
        assert_eq!(&ids[..2], &[LATIN_ID, ARABIC_ID]);
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../../ts/public/fonts.json")).unwrap();
        let mut shipped: Vec<String> = manifest["defaults"]
            .as_array()
            .unwrap()
            .iter()
            .chain(manifest["substitutes"].as_array().unwrap())
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        shipped.sort();
        shipped.dedup();
        let mut mine: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        mine.sort();
        assert_eq!(mine, shipped, "harness faces drifted from fonts.json");
    }

    #[test]
    fn latin_run_resolves_to_the_latin_face() {
        assert_eq!(id_of(&harness_stack(), Script::Latin, None), LATIN_ID);
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

    /// Issue #472 - no id trick: Amiri's id sorts BEFORE Liberation Sans's
    /// (`amiri` < `liberation`) and Amiri covers Latin, so by id alone it
    /// would win Latin text. The priority keeps Latin on Liberation Sans
    /// in every harness stack, and without the declared table the stack
    /// really would pick Amiri (the test would otherwise pass vacuously).
    #[test]
    fn latin_default_does_not_depend_on_id_order() {
        assert!(ARABIC_ID < LATIN_ID);
        for stack in [harness_stack(), harness_stack_with_substitutes()] {
            assert_eq!(id_of(&stack, Script::Latin, None), LATIN_ID);
        }
        let mut bare = HashMap::new();
        for (id, bytes) in [(LATIN_ID, LIBERATION_SANS), (ARABIC_ID, AMIRI)] {
            let face = LoadedFont::parse(id.to_string(), bytes.to_vec()).expect("parse");
            bare.insert(id.to_string(), Arc::new(face));
        }
        let bare = FontStack::from_faces(bare, LATIN_ID);
        assert_eq!(id_of(&bare, Script::Latin, None), ARABIC_ID);
    }

    /// The harnesses and the shell agree: [`DEFAULTS_BY_SCRIPT`] is
    /// `ts/public/fonts.json` `defaults_by_script`, and the ids the harness
    /// registers are manifest ids.
    #[test]
    fn matches_the_shell_manifest() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../../ts/public/fonts.json"))
                .expect("fonts.json parses");
        let by_script = manifest["defaults_by_script"]
            .as_object()
            .expect("fonts.json has defaults_by_script");
        let declared = |key: &str| -> Vec<String> {
            match &by_script[key] {
                serde_json::Value::String(id) => vec![id.clone()],
                serde_json::Value::Array(ids) => ids
                    .iter()
                    .map(|v| v.as_str().expect("id string").to_string())
                    .collect(),
                other => panic!("defaults_by_script.{key}: {other}"),
            }
        };
        for (script, key) in [(Script::Latin, "latin"), (Script::Arabic, "arabic")] {
            let ours: Vec<String> = DEFAULTS_BY_SCRIPT
                .iter()
                .find(|(s, _)| *s == script)
                .map(|(_, ids)| ids.iter().map(|s| (*s).to_string()).collect())
                .unwrap_or_default();
            assert_eq!(ours, declared(key), "defaults_by_script.{key}");
        }
        assert_eq!(by_script.len(), DEFAULTS_BY_SCRIPT.len());
        let ids: Vec<&str> = manifest["fonts"]
            .as_array()
            .expect("fonts")
            .iter()
            .map(|f| f["id"].as_str().expect("id"))
            .collect();
        for id in [LATIN_ID, ARABIC_ID]
            .into_iter()
            .chain(SUBSTITUTES.iter().map(|(id, _)| *id))
        {
            assert!(ids.contains(&id), "`{id}` is a fonts.json id");
        }
    }
}
