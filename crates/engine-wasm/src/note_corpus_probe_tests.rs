//! Issues #317 / #181 / #141 — the whole-corpus note-band probe.
//!
//! An `#[ignore]`d, env-driven driver (never part of `cargo test`): lays
//! out ONE real `.docx` (`NGE_PROBE_FILE`) through the production
//! `build_pages` pipeline (the native `test_engine_with_doc` engine, one
//! test font) and prints one tab-separated `PROBE` line of note-band
//! facts — page count, `geometry_fingerprint`, footnote / endnote band
//! entries, endnote references, floats overlapping a footnote band, the
//! degradation notes, and a viewport-culled band sweep checked against
//! the full layout with `layout::verify_prefix_open` (the `ExpandLayout`
//! fast path's verifier).
//!
//! One document per process so a stack overflow or a hang in one corpus
//! file (`tools/corpus-native`'s rationale) cannot take the batch down —
//! the shell loop owns the timeout. Diff the lines of two checkouts to
//! prove a fingerprint-moving change is confined to the documents it
//! should touch:
//!
//! ```text
//! NGE_PROBE_FILE=/data/corpus/files/x/endnotes.docx \
//!   target/release/deps/engine_wasm-<hash> --ignored --exact \
//!   note_corpus_probe_tests::probe_one_document --nocapture
//! ```

use super::*;

#[test]
#[ignore = "corpus probe — needs NGE_PROBE_FILE"]
fn probe_one_document() {
    let Ok(path) = std::env::var("NGE_PROBE_FILE") else {
        return;
    };
    let name = path.clone();
    let bytes = std::fs::read(&path).expect("read probe file");
    let Ok(archive) = format_docx::read_docx(&bytes) else {
        println!("PROBE\t{name}\tunreadable");
        return;
    };
    let doc = archive.document;
    let refs = doc.note_references();
    let count_refs = |kind: engine::NoteKind| {
        let mut seen = std::collections::HashSet::new();
        refs.iter()
            .filter(|r| r.anchor.kind == kind && seen.insert(r.anchor))
            .count()
    };
    let (fn_refs, en_refs) = (
        count_refs(engine::NoteKind::Footnote),
        count_refs(engine::NoteKind::Endnote),
    );
    let engine = tests::test_engine_with_doc(doc);
    let (full, _, _, info) = engine.build_pages(1.0, false, None).expect("full layout");
    let heads = |band: fn(&PageBox) -> &layout::NoteBand| -> (usize, usize) {
        let all: Vec<&layout::FootnoteEntry> =
            full.iter().flat_map(|p| band(p).entries.iter()).collect();
        (
            all.iter().filter(|e| !e.continued_from_previous).count(),
            all.len(),
        )
    };
    let (fn_heads, fn_entries) = heads(|p| &p.footnotes);
    let (en_heads, en_entries) = heads(|p| &p.endnotes);
    let en_in_footnote_band = full
        .iter()
        .flat_map(|p| p.footnotes.entries.iter())
        .filter(|e| e.kind == engine::NoteKind::Endnote)
        .count();
    let floats: usize = full.iter().map(|p| p.floats.len()).sum();
    let floats_over_band: usize = full
        .iter()
        .map(|p| {
            if p.footnotes.is_empty() {
                return 0;
            }
            let limit = p.footnotes.y - layout::FOOTNOTE_SEPARATOR_HEIGHT_PT;
            p.floats
                .iter()
                .filter(|f| {
                    !f.behind_doc
                        && !f.hidden
                        && matches!(f.anchor, layout::FloatAnchorRef::Body { .. })
                        && f.origin.y + f.size.height > limit + 0.01
                        && f.origin.y < p.footnotes.y + p.footnotes.content_height()
                })
                .count()
        })
        .sum();
    let degraded: Vec<String> = info
        .degradations
        .iter()
        .map(|d| format!("{:?}@{}", d.reason, d.page.unwrap_or(u32::MAX)))
        .collect();
    /* The culled-band sweep — only where notes are in play (it is the
    probe's expensive half). */
    let (mut bands, mut mismatches, mut opened) = (0usize, 0usize, 0usize);
    if fn_refs + en_refs > 0 && full.len() > 1 {
        let gap = render::scene::PAGE_GAP_PT;
        let total: f32 = full.iter().map(|p| p.size.height + gap).sum();
        let runway = lazy_runway(engine.lazy_layout.viewport_h, 1.0);
        let steps = (full.len() * 3).min(36);
        for k in 1..steps {
            let target = total * k as f32 / steps as f32 - runway;
            let (band, _, _, binfo) = engine
                .build_pages(1.0, false, Some(target))
                .expect("band layout");
            if binfo.is_full_layout {
                break;
            }
            bands += 1;
            opened += usize::from(binfo.open_from_page.is_some());
            if layout::verify_prefix_open(&band, &full, binfo.open_from_page).is_err() {
                mismatches += 1;
            }
        }
    }
    println!(
        "PROBE\t{name}\tok\tpages={}\tfp={:#018x}\tfn_refs={fn_refs}\tfn_heads={fn_heads}\t\
         fn_entries={fn_entries}\ten_refs={en_refs}\ten_heads={en_heads}\ten_entries={en_entries}\t\
         en_in_fn_band={en_in_footnote_band}\tfloats={floats}\tfloats_over_band={floats_over_band}\t\
         bands={bands}\tband_open={opened}\tband_mismatch={mismatches}\tdegraded={}",
        full.len(),
        layout::geometry_fingerprint(&full),
        degraded.join(","),
    );
}
