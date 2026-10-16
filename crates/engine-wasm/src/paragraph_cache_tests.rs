//! Issue #457 — the paragraph layout cache is weighted in estimated bytes
//! (not entries), so the paragraphs of a long document fit and a full
//! re-layout of it is warm. Plus the manual sizing / timing harnesses the
//! budget in `paragraph_cache.rs` was chosen with.

use super::*;
use crate::paragraph_cache::{PARAGRAPH_CACHE_BUDGET_BYTES, ParagraphCache, paragraph_box_bytes};
use crate::tests::test_engine_with_doc;
use std::mem::size_of;

/// A real laid-out paragraph of `text` (Liberation Sans, 400 px wide).
fn laid_box(text: &str) -> ParagraphBox {
    let engine = test_engine_with_doc(DocumentTree::from_text(text));
    let cfg = engine.layout_cfg.clone().expect("cfg");
    let doc = engine.undo.current().clone();
    let sctx = StyleContext::of(&doc);
    let fonts = engine.font_stack(&cfg.font_id);
    let para = doc
        .blocks
        .get(0)
        .and_then(engine::Block::as_paragraph)
        .expect("paragraph");
    layout_paragraph_wrapped_uncached(para, &fonts, &cfg, 1.0, 400.0, sctx, &[])
}

fn glyph_count(p: &ParagraphBox) -> usize {
    p.lines
        .iter()
        .flat_map(|l| &l.runs)
        .map(|r| r.glyphs.len())
        .sum()
}

const SENTENCE: &str = "The quick brown fox jumps over the lazy dog. ";

#[test]
fn a_box_is_charged_for_its_glyphs_lines_and_strings() {
    let short = laid_box("hello");
    let long = laid_box(&SENTENCE.repeat(12));
    let (short_bytes, long_bytes) = (paragraph_box_bytes(&short), paragraph_box_bytes(&long));

    /* The glyphs alone are a floor: every shaped glyph costs a whole
    `PositionedGlyph`. */
    assert!(
        long_bytes >= glyph_count(&long) * size_of::<layout::PositionedGlyph>(),
        "{long_bytes} bytes for {} glyphs",
        glyph_count(&long)
    );
    assert!(short_bytes >= size_of::<ParagraphBox>());
    assert!(
        long.lines.len() > 1,
        "the long paragraph must wrap onto several lines"
    );
    /* ~540 characters vs 5: the estimate scales with the content (the
    fixed cost of a box is a fifth of the five-character one). */
    assert!(
        long_bytes > short_bytes * 20,
        "{short_bytes} vs {long_bytes}"
    );

    /* A string a glyph owns is charged by its length (+ the allocation
    allowance) — and a second one on the same glyph adds up. */
    let mut owned = short.clone();
    let base = paragraph_box_bytes(&owned);
    let glyph = owned
        .lines
        .iter_mut()
        .flat_map(|l| l.runs.iter_mut())
        .flat_map(|r| r.glyphs.iter_mut())
        .next()
        .expect("a glyph");
    glyph.inline_image_rel_id = Some("rId17".to_string());
    let with_image = paragraph_box_bytes(&owned);
    assert!(with_image > base + 5, "{base} -> {with_image}");
    let glyph = owned
        .lines
        .iter_mut()
        .flat_map(|l| l.runs.iter_mut())
        .flat_map(|r| r.glyphs.iter_mut())
        .next()
        .expect("a glyph");
    glyph.inline_footnote_marker = Some("iv".to_string());
    assert!(paragraph_box_bytes(&owned) > with_image + 2);

    /* So are the field overlays and the page-break list. */
    let mut fielded = short.clone();
    let base = paragraph_box_bytes(&fielded);
    fielded.fields.push(layout::LayoutField {
        byte_range: 0..1,
        instruction: "PAGE".to_string(),
        evaluated_text: Some("12".to_string()),
    });
    assert!(paragraph_box_bytes(&fielded) > base + size_of::<layout::LayoutField>() + 6);
    let base = paragraph_box_bytes(&fielded);
    fielded.page_break_after_line = vec![0, 1, 2];
    assert!(paragraph_box_bytes(&fielded) >= base + 3 * size_of::<usize>());
}

#[test]
fn the_byte_budget_evicts_least_recently_used_first() {
    let b = laid_box(&SENTENCE.repeat(3));
    let each = paragraph_box_bytes(&b);
    /* Room for exactly 8 boxes (the per-entry limit is an eighth of the
    budget, so this is also the smallest budget that admits `b`). */
    let mut cache = ParagraphCache::with_limits(each * 8 + each / 2, 1000);
    for key in 0..20u64 {
        cache.put(key, b.clone());
        if key == 5 {
            /* Touch key 0 so it outlives its older siblings. */
            let _ = cache.get(&0);
        }
    }
    assert_eq!(cache.len(), 8, "the budget holds eight boxes");
    assert_eq!(cache.bytes(), 8 * each, "the accounting is exact");
    assert!(cache.bytes() <= cache.budget_bytes());
    for key in 12..20u64 {
        assert!(cache.contains(&key), "the newest boxes stay: {key}");
    }
    assert!(
        !cache.contains(&1) && !cache.contains(&11),
        "the oldest untouched boxes are evicted first"
    );
}

#[test]
fn accounting_stays_exact_through_replace_pop_and_clear() {
    let small = laid_box("hello");
    let big = laid_box(&SENTENCE.repeat(4));
    let (small_bytes, big_bytes) = (paragraph_box_bytes(&small), paragraph_box_bytes(&big));
    assert!(big_bytes > small_bytes);
    let mut cache = ParagraphCache::with_limits(big_bytes * 64, 1000);

    cache.put(1, small.clone());
    cache.put(2, big.clone());
    assert_eq!(cache.bytes(), small_bytes + big_bytes);
    /* Replacing a key swaps its charge. */
    cache.put(1, big.clone());
    assert_eq!(cache.len(), 2);
    assert_eq!(cache.bytes(), 2 * big_bytes);
    let popped = cache.pop(&2).expect("cached");
    assert_eq!(paragraph_box_bytes(&popped), big_bytes);
    assert_eq!(cache.bytes(), big_bytes);
    assert!(cache.pop(&2).is_none());
    cache.clear();
    assert_eq!((cache.len(), cache.bytes()), (0, 0));
    assert!(cache.is_empty());
}

#[test]
fn an_oversized_box_is_not_cached_and_drops_its_stale_entry() {
    let small = laid_box("hello");
    let big = laid_box(&SENTENCE.repeat(30));
    let (small_bytes, big_bytes) = (paragraph_box_bytes(&small), paragraph_box_bytes(&big));
    /* A budget whose eighth admits `small` and refuses `big`. */
    let budget = small_bytes * 8;
    assert!(big_bytes > budget / 8);
    let mut cache = ParagraphCache::with_limits(budget, 1000);
    cache.put(7, small);
    assert!(cache.contains(&7));
    cache.put(7, big.clone());
    assert!(
        !cache.contains(&7),
        "a stale entry must not outlive its replacement being refused"
    );
    cache.put(8, big);
    assert_eq!((cache.len(), cache.bytes()), (0, 0));
}

#[test]
fn the_entry_ceiling_still_applies_beside_the_byte_budget() {
    let b = laid_box("hello");
    let mut cache = ParagraphCache::with_limits(usize::MAX / 16, 3);
    for key in 0..10u64 {
        cache.put(key, b.clone());
    }
    assert_eq!(cache.len(), 3);
    assert_eq!(cache.bytes(), 3 * paragraph_box_bytes(&b));
    assert!(cache.contains(&9) && cache.contains(&8) && cache.contains(&7));
}

#[test]
fn the_shipped_budget_is_a_byte_budget_far_above_the_old_entry_cap() {
    /* A typical paragraph box is a few KiB, so the old 4096-entry cap
    pinned tens of MiB while the paragraphs of a 250-page document
    (5000) did not fit; the shipped cache admits many times that. */
    let b = laid_box(&SENTENCE.repeat(2));
    let each = paragraph_box_bytes(&b);
    assert!(
        PARAGRAPH_CACHE_BUDGET_BYTES / each > 4 * 4096,
        "{each} bytes"
    );
}

/// The issue-#457 shape in miniature: more distinct paragraphs than the old
/// 4096-entry cap, laid out in full twice. The second pass must be
/// entirely warm — no miss, no new entry — and byte-identical geometry.
#[test]
fn a_full_relayout_of_more_paragraphs_than_the_old_cap_is_warm() {
    const PARAGRAPHS: usize = 4600;
    let doc = DocumentTree::from_paragraphs((0..PARAGRAPHS).map(|i| format!("Paragraph {i:05}.")));
    let engine = test_engine_with_doc(doc);

    let (cold_pages, _, _, _) = engine.build_pages(2.0, false, None).expect("cold build");
    let after_cold = {
        let cache = engine.layout_cache.borrow();
        assert!(
            cache.len() >= PARAGRAPHS,
            "every paragraph is cached ({} of {PARAGRAPHS})",
            cache.len()
        );
        assert!(cache.bytes() > 0 && cache.bytes() <= cache.budget_bytes());
        (cache.len(), cache.bytes(), cache.stats())
    };

    let (warm_pages, _, _, _) = engine.build_pages(2.0, false, None).expect("warm build");
    let cache = engine.layout_cache.borrow();
    let stats = cache.stats();
    assert_eq!(
        stats.misses, after_cold.2.misses,
        "the warm pass must not miss ({} -> {})",
        after_cold.2.misses, stats.misses
    );
    assert!(stats.hits >= after_cold.2.hits + PARAGRAPHS as u64);
    assert_eq!((cache.len(), cache.bytes()), (after_cold.0, after_cold.1));
    assert_eq!(
        layout::geometry_fingerprint(&cold_pages),
        layout::geometry_fingerprint(&warm_pages),
        "a cached paragraph lays out exactly as a fresh one"
    );
}

/* ====================================================================
Manual harnesses (issue #457). Not correctness gates:

    cargo test -p engine-wasm --release profile_paragraph_cache -- \
        --ignored --nocapture
==================================================================== */

/// An engine over `doc` with the interactive editor's boot faces — Liberation
/// Sans for Latin, Amiri then Noto Naskh Arabic for Arabic, declared the way
/// `ts/public/fonts.json` `defaults_by_script` does — so the fixtures'
/// mixed Latin / Arabic text shapes with the glyphs the product shapes.
fn shell_font_engine(doc: DocumentTree) -> Engine {
    let mut engine = test_engine_with_doc(doc);
    engine.fonts.clear();
    for (id, bytes) in [
        (
            "liberation",
            include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").as_slice(),
        ),
        (
            "amiri",
            include_bytes!("../../../ts/fonts/Amiri-Regular.ttf").as_slice(),
        ),
        (
            "noto-naskh",
            include_bytes!("../../../ts/fonts/NotoNaskhArabic-Regular.ttf").as_slice(),
        ),
    ] {
        let font = LoadedFont::parse(id.to_string(), bytes.to_vec()).expect("parse face");
        engine.fonts.insert(id.to_string(), Arc::new(font));
    }
    engine.font_priority = HashMap::from([
        (text_pipeline::Script::Latin, vec!["liberation".to_string()]),
        (
            text_pipeline::Script::Arabic,
            vec!["amiri".to_string(), "noto-naskh".to_string()],
        ),
    ]);
    if let Some(cfg) = engine.layout_cfg.as_mut() {
        cfg.font_id = "liberation".to_string();
    }
    engine
}

/// The pre-#476 perf-fixture filler (`tools/perf-fixtures`): mixed Latin and
/// Arabic, ~110 characters a paragraph.
const FIXTURE_FILLER: &str = "The quick brown fox jumps over the lazy dog. \
    مرحبا بالعالم، هذا نص حشو لقياس استهلاك الذاكرة.";

/// A harness document: a committed `tests/perf/<label>.docx` fixture, or
/// `dense-<n>` — `n` filler paragraphs, the shape the `250p` / `500p`
/// fixtures had before issue #476 re-calibrated them to their nominal page
/// counts (20 paragraphs a page: 5 000 and 10 000 paragraphs). Since #476
/// the committed `500p` is 3 000 paragraphs and fits even the old 4096-entry
/// cache; a real 500-page document (a dozen paragraphs a page and up) does
/// not, so the dense documents keep that scenario measurable.
fn perf_fixture(label: &str) -> DocumentTree {
    if let Some(n) = label.strip_prefix("dense-") {
        let n: usize = n.parse().expect("dense-<paragraph count>");
        return DocumentTree::from_paragraphs(
            (0..n).map(|i| format!("Paragraph {i:05}. {FIXTURE_FILLER}")),
        );
    }
    let path = format!(
        "{}/../../tests/perf/{label}.docx",
        env!("CARGO_MANIFEST_DIR")
    );
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    format_docx::read_docx(&bytes)
        .unwrap_or_else(|e| panic!("parse {path}: {e:?}"))
        .document
}

/// What the budget in `paragraph_cache.rs` was chosen from: each perf
/// fixture laid out in full, the cached paragraph count and estimated
/// bytes (native pointer width — wasm32's boxes are smaller, so this is
/// the upper bound), and the cache's own budget beside it.
#[test]
#[ignore = "manual sizing harness, not a correctness gate"]
fn profile_paragraph_cache_sizing() {
    for label in ["50p", "100p", "250p", "500p", "dense-5000", "dense-10000"] {
        let engine = shell_font_engine(perf_fixture(label));
        let (pages, _, _, _) = engine.build_pages(2.0, false, None).expect("full layout");
        let cache = engine.layout_cache.borrow();
        let glyphs: usize = pages
            .iter()
            .flat_map(|p| &p.blocks)
            .filter_map(|b| match b {
                LayoutBlock::Paragraph(p) => Some(glyph_count(p)),
                _ => None,
            })
            .sum();
        eprintln!(
            "{label:>11}: {:>5} pages, {:>6} cached paragraphs, {:>9} glyphs, {:>7.1} MiB estimated \
             ({:.0} B/paragraph, {} B/glyph, size_of PositionedGlyph = {}), budget {} MiB",
            pages.len(),
            cache.len(),
            glyphs,
            cache.bytes() as f64 / (1 << 20) as f64,
            cache.bytes() as f64 / cache.len().max(1) as f64,
            cache.bytes() / glyphs.max(1),
            size_of::<layout::PositionedGlyph>(),
            cache.budget_bytes() >> 20,
        );
    }
}

/// Warm vs cold FULL re-layout (`build_pages(.., None)`: what PDF export,
/// `ExpandLayout` to the end and the save restamp run) on the perf
/// fixtures, with the pre-#457 cache (4096 entries, no byte bound) beside
/// the shipped one. Cold = a fresh cache; warm = the same engine again.
/// Each is the best of `ROUNDS` so a noisy machine does not decide it; the
/// load average is printed so the reader can discount a loaded run.
#[test]
#[ignore = "manual timing harness, not a correctness gate"]
fn profile_paragraph_cache_warm_vs_cold() {
    use std::time::Instant;
    const ROUNDS: usize = 2;
    let load = || {
        std::fs::read_to_string("/proc/loadavg")
            .unwrap_or_default()
            .trim()
            .to_string()
    };
    eprintln!("loadavg at start: {}", load());
    for label in ["500p", "dense-5000", "dense-10000"] {
        let engine = shell_font_engine(perf_fixture(label));
        for (name, fresh) in [
            (
                "before (4096 entries)",
                (|| ParagraphCache::with_limits(usize::MAX / 16, 4096)) as fn() -> ParagraphCache,
            ),
            ("after  (byte budget)", ParagraphCache::new),
        ] {
            let mut cold = Vec::new();
            let mut warm = Vec::new();
            for _ in 0..ROUNDS {
                engine.layout_cache.borrow_mut().paragraphs = fresh();
                let t = Instant::now();
                let (pages, _, _, _) = engine.build_pages(2.0, false, None).expect("cold layout");
                cold.push(t.elapsed().as_secs_f64() * 1000.0);
                let mid = engine.layout_cache.borrow().stats();

                let t = Instant::now();
                let (again, _, _, _) = engine.build_pages(2.0, false, None).expect("warm layout");
                warm.push(t.elapsed().as_secs_f64() * 1000.0);
                let after = engine.layout_cache.borrow().stats();
                assert_eq!(pages.len(), again.len());
                let cache = engine.layout_cache.borrow();
                eprintln!(
                    "{label} {name}: cold {:>8.1} ms   warm {:>8.1} ms ({} misses, {} hits)   \
                     cache {} entries / {:.1} MiB",
                    cold.last().unwrap(),
                    warm.last().unwrap(),
                    after.misses - mid.misses,
                    after.hits - mid.hits,
                    cache.len(),
                    cache.bytes() as f64 / (1 << 20) as f64,
                );
            }
            let best = |v: &[f64]| v.iter().copied().fold(f64::INFINITY, f64::min);
            eprintln!(
                "{label} {name}: best cold {:.1} ms, best warm {:.1} ms -> warm/cold = {:.0} %",
                best(&cold),
                best(&warm),
                100.0 * best(&warm) / best(&cold)
            );
        }
    }
    eprintln!("loadavg at end: {}", load());
}

/* ====================================================================
Issue #500 (item 3) — the layout caches are observable on `EngineStats`.
==================================================================== */

/// `Command::RequestStats` through the real dispatcher.
#[cfg(feature = "fuzz-native")]
fn request_stats(engine: &mut Engine) -> bridge::EngineStats {
    match engine.apply_sync(Command::RequestStats) {
        Event::Stats(stats) => stats,
        other => panic!("expected Stats, got {other:?}"),
    }
}

/// Open a document of many paragraphs and a table, repaint it twice, and
/// read the caches off `EngineStats`: the first paint filled them (misses,
/// bytes, entries), the second is served from them (hits grow, nothing
/// new is laid out) and the bytes stay inside the paragraph budget.
#[cfg(feature = "fuzz-native")]
#[test]
fn engine_stats_report_the_layout_caches_and_a_repaint_hits() {
    let mut body = String::new();
    for i in 0..40 {
        body.push_str(&format!(
            "<w:p><w:r><w:t>Paragraph {i:03}: the quick brown fox jumps over the lazy dog.</w:t></w:r></w:p>"
        ));
    }
    body.push_str(
        "<w:tbl><w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/></w:tblPr>\
         <w:tblGrid><w:gridCol w:w=\"2000\"/><w:gridCol w:w=\"2000\"/></w:tblGrid>\
         <w:tr><w:tc><w:p><w:r><w:t>cell a</w:t></w:r></w:p></w:tc>\
         <w:tc><w:p><w:r><w:t>cell b</w:t></w:r></w:p></w:tc></w:tr></w:tbl>",
    );
    let mut engine = Engine::new_headless(DocumentTree::from_text("seed"));
    let opened = engine.apply_sync(Command::OpenDocument {
        bytes: format_docx::test_fixtures::docx_with_body(&body),
        format: DocFormat::Docx,
        name: None,
        limits: None,
        defaults: None,
        password: None,
    });
    assert!(
        matches!(opened, Event::DocumentLoaded { .. }),
        "expected DocumentLoaded, got {opened:?}"
    );

    engine.build_pages(1.0, false, None).expect("first paint");
    let first = request_stats(&mut engine);
    assert!(first.paragraph_cache_entries >= 40, "{first:?}");
    assert!(first.paragraph_cache_bytes > 0, "{first:?}");
    assert!(first.paragraph_cache_misses >= 40, "{first:?}");
    assert!(first.table_cache_entries >= 1, "{first:?}");
    assert!(first.table_cache_bytes > 0, "{first:?}");
    assert!(first.table_cache_misses >= 1, "{first:?}");

    engine.build_pages(1.0, false, None).expect("second paint");
    let second = request_stats(&mut engine);
    assert!(
        second.paragraph_cache_hits > first.paragraph_cache_hits,
        "{first:?} -> {second:?}"
    );
    assert_eq!(
        second.paragraph_cache_misses, first.paragraph_cache_misses,
        "the repaint must not lay a paragraph out again"
    );
    assert!(
        second.table_cache_hits > first.table_cache_hits,
        "{first:?} -> {second:?}"
    );
    assert_eq!(second.table_cache_misses, first.table_cache_misses);
    assert_eq!(
        (second.paragraph_cache_entries, second.paragraph_cache_bytes),
        (first.paragraph_cache_entries, first.paragraph_cache_bytes),
        "a warm repaint adds nothing"
    );
    assert!(second.paragraph_cache_bytes <= PARAGRAPH_CACHE_BUDGET_BYTES as u64);
    assert!(second.table_cache_bytes > 0);

    /* The counters outlive a clear; the contents do not. */
    engine.layout_cache.get_mut().clear();
    let cleared = request_stats(&mut engine);
    assert_eq!(
        (
            cleared.paragraph_cache_entries,
            cleared.paragraph_cache_bytes
        ),
        (0, 0)
    );
    assert_eq!(
        (cleared.table_cache_entries, cleared.table_cache_bytes),
        (0, 0)
    );
    assert_eq!(cleared.paragraph_cache_hits, second.paragraph_cache_hits);
}
