//! Issue #497 — the table-heavy body-edit repaint, driven natively through
//! the real dispatcher exactly as `tools/perf/run.mjs` drives it in the
//! browser: the editor's boot faces + `SET_FONT_PRIORITY`, `LOAD_DOCX` of
//! `tests/perf/tables-30p.docx`, `EXPAND_LAYOUT` to the end, then
//! one-character `INSERT_TEXT`s in the intro paragraph above every table,
//! each timed end to end (`Engine::apply`, auto-repaint included). Run with:
//! `cargo test -p engine-wasm --release --lib profile_table_body_edit -- --ignored --nocapture`

use super::*;
use bridge::{FontPriority, PathStep, Script as BridgeScript};

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = Box::pin(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("Engine::apply suspended in a native test"),
    }
}

fn apply(e: &mut Engine, cmd: Command) -> Event {
    block_on(e.apply(cmd))
}

/// The shell's boot faces (`ts/public/fonts.json` `defaults` +
/// `substitutes`), under their manifest ids.
const EDITOR_FACES: &[(&str, &[u8])] = &[
    (
        "liberation",
        include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf"),
    ),
    (
        "amiri",
        include_bytes!("../../../ts/fonts/Amiri-Regular.ttf"),
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

/// An engine booted like the shell: every boot face loaded through
/// `LOAD_FONT`, the per-script priority sent, the `RENDER_PAGE` text
/// metrics (`App.tsx`: 24 px / 36 px) at device scale 1.
pub(crate) fn shell_engine() -> Engine {
    let mut e = assemble_engine(None, None);
    for (id, bytes) in EDITOR_FACES {
        let evt = apply(
            &mut e,
            Command::LoadFont {
                id: (*id).to_string(),
                bytes: bytes.to_vec(),
            },
        );
        assert!(
            !matches!(evt, Event::Error { .. }),
            "LoadFont {id}: {evt:?}"
        );
    }
    let evt = apply(
        &mut e,
        Command::SetFontPriority {
            priorities: vec![
                FontPriority {
                    script: BridgeScript::Latin,
                    ids: vec!["liberation".into()],
                },
                FontPriority {
                    script: BridgeScript::Arabic,
                    ids: vec!["amiri".into(), "noto-naskh".into()],
                },
            ],
        },
    );
    assert!(
        !matches!(evt, Event::Error { .. }),
        "SetFontPriority: {evt:?}"
    );
    e.layout_cfg = Some(RenderConfig {
        font_id: "liberation".to_string(),
        base_direction: ShapingDirection::Ltr,
        px_size: 24.0,
        line_height: 36.0,
        alignment: Alignment::Start,
        scale: 1.0,
        base_scale: 1.0,
        zoom: 1.0,
    });
    e
}

fn percentile(samples: &[f64], p: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

#[test]
#[ignore = "manual timing harness, not a correctness gate"]
fn profile_table_body_edit() {
    use std::time::Instant;
    const EDITS: usize = 40;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/perf/tables-30p.docx"
    );
    let bytes = std::fs::read(path).expect("read tables-30p.docx (run tools/perf-fixtures)");
    let mut e = shell_engine();
    let t = Instant::now();
    let evt = apply(&mut e, Command::LoadDocx { bytes });
    assert!(matches!(evt, Event::DocumentLoaded { .. }), "{evt:?}");
    eprintln!("[#497] LOAD_DOCX:            {:>9.2?}", t.elapsed());
    let t = Instant::now();
    let evt = apply(&mut e, Command::ExpandLayout { target_y: 1e9 });
    let pages = match &evt {
        Event::Painted { page_count, .. } => *page_count,
        other => panic!("EXPAND_LAYOUT: {other:?}"),
    };
    eprintln!(
        "[#497] EXPAND_LAYOUT 1e9:     {:>9.2?}  ({pages} pages)",
        t.elapsed()
    );
    let mut samples = Vec::with_capacity(EDITS);
    let mut mismatches = 0usize;
    for _ in 0..EDITS {
        let t = Instant::now();
        let evt = apply(
            &mut e,
            Command::InsertText {
                at: Some(bridge::LogicalPos {
                    path: bridge::BlockPath {
                        steps: vec![PathStep::Block { idx: 0 }],
                    },
                    offset: 0,
                }),
                text: "x".into(),
            },
        );
        samples.push(t.elapsed().as_secs_f64() * 1e3);
        assert!(!matches!(evt, Event::Error { .. }), "INSERT_TEXT: {evt:?}");
        mismatches += e
            .last_paint_dims
            .layout_degraded
            .iter()
            .filter(|d| format!("{d:?}").contains("CacheMismatch"))
            .count();
    }
    eprintln!(
        "[#497] INSERT_TEXT x{EDITS} (apply + repaint): p50 {:.2} ms  p95 {:.2} ms  max {:.2} ms  cache-mismatch notes {mismatches}",
        percentile(&samples, 50.0),
        percentile(&samples, 95.0),
        samples.iter().cloned().fold(0.0, f64::max),
    );
}

/// Issue #497 item 2 — font resolution with the shell's full face set: the
/// old 10,000-paragraph `500p` shape (`dense-10000`, mixed Latin / Arabic)
/// laid out cold with the first `n` boot faces, and `FontStack::resolve`
/// timed per call for the lookups a layout makes (no family, a substituted
/// family, an exact id). Run with:
/// `cargo test -p engine-wasm --release --lib profile_font_resolution -- --ignored --nocapture`
#[test]
#[ignore = "manual timing harness, not a correctness gate"]
fn profile_font_resolution() {
    use std::time::Instant;
    use text_pipeline::Script as S;
    const FILLER: &str = "The quick brown fox jumps over the lazy dog. \
        مرحبا بالعالم، هذا نص حشو لقياس استهلاك الذاكرة.";
    const CALLS: u32 = 200_000;
    let doc =
        DocumentTree::from_paragraphs((0..10_000).map(|i| format!("Paragraph {i:05}. {FILLER}")));
    for n in [2usize, 3, EDITOR_FACES.len()] {
        let mut engine = tests::test_engine_with_doc(doc.clone());
        engine.fonts.clear();
        for (id, bytes) in &EDITOR_FACES[..n] {
            let font = LoadedFont::parse((*id).to_string(), bytes.to_vec()).expect("face");
            engine.fonts.insert((*id).to_string(), Arc::new(font));
        }
        engine.font_priority = HashMap::from([
            (S::Latin, vec!["liberation".to_string()]),
            (
                S::Arabic,
                vec!["amiri".to_string(), "noto-naskh".to_string()],
            ),
        ]);
        if let Some(cfg) = engine.layout_cfg.as_mut() {
            cfg.font_id = "liberation".to_string();
        }
        let t = Instant::now();
        let (pages, ..) = engine
            .build_pages(2.0, false, None)
            .expect("cold full layout");
        let cold = t.elapsed();
        let t = Instant::now();
        let stack = engine.font_stack("liberation");
        let build = t.elapsed();
        let time = |script, family: Option<&str>| {
            let t = Instant::now();
            for _ in 0..CALLS {
                std::hint::black_box(stack.resolve(
                    script,
                    std::hint::black_box(family),
                    false,
                    false,
                ));
            }
            t.elapsed().as_nanos() as f64 / f64::from(CALLS)
        };
        eprintln!(
            "[#497] {n} faces: dense-10000 cold full layout {cold:>9.2?} ({} pages); \
             FontStack build {build:>8.2?}; resolve ns/call: Latin none {:.0}, Arabic none {:.0}, \
             Latin 'Calibri' {:.0}, Arabic 'Arial' {:.0}, exact 'liberation' {:.0}",
            pages.len(),
            time(S::Latin, None),
            time(S::Arabic, None),
            time(S::Latin, Some("Calibri")),
            time(S::Arabic, Some("Arial")),
            time(S::Latin, Some("liberation")),
        );
    }
}

/// Issue #497 — an edit repaints only the page canvases whose display list
/// changed, and a paint request only the canvases that are not up to date.
/// A marker drawn on each canvas shows which canvases a repaint touched (a
/// repaint clears the canvas first). Browser-only: an `OffscreenCanvas` needs a JS global scope
/// (`wasm-pack test --headless --chrome crates/engine-wasm`).
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
fn an_edit_repaints_only_the_pages_it_changed() {
    use web_sys::OffscreenCanvas;
    const MARK: [u8; 3] = [0xff, 0x00, 0xff];
    let mut e = tests::test_engine_with_doc(DocumentTree::from_paragraphs(
        (1..=200).map(|i| format!("Line {i}")),
    ));
    let evt = apply(&mut e, Command::ExpandLayout { target_y: 1e9 });
    let Event::Painted { page_count, .. } = evt else {
        panic!("EXPAND_LAYOUT: {evt:?}");
    };
    assert!(
        page_count >= 3,
        "fixture must span 3+ pages, got {page_count}"
    );
    let fresh_canvas = || {
        let canvas = OffscreenCanvas::new(1, 1).expect("OffscreenCanvas");
        let ctx = canvas
            .get_context("2d")
            .expect("2d")
            .expect("2d")
            .dyn_into::<OffscreenCanvasRenderingContext2d>()
            .expect("2d ctx");
        (canvas, ctx)
    };
    let mut ctxs: Vec<OffscreenCanvasRenderingContext2d> = (0..3)
        .map(|i| {
            let (canvas, ctx) = fresh_canvas();
            e.set_page_canvas(i, canvas).expect("register");
            ctx
        })
        .collect();
    fn mark_all(ctxs: &[OffscreenCanvasRenderingContext2d]) {
        for ctx in ctxs {
            ctx.set_fill_style_str("#ff00ff");
            ctx.fill_rect(0.0, 0.0, 2.0, 2.0);
        }
    }
    fn marked(ctxs: &[OffscreenCanvasRenderingContext2d]) -> Vec<bool> {
        ctxs.iter()
            .map(|ctx| {
                let px = ctx.get_image_data(0.0, 0.0, 1.0, 1.0).expect("read").data();
                px[..3] == MARK
            })
            .collect()
    }
    /* A paint request paints every freshly registered canvas. */
    let paint = Command::RequestPaint {
        viewport: BridgeRect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        },
        dirty: None,
    };
    assert!(matches!(
        apply(&mut e, paint.clone()),
        Event::Painted { .. }
    ));
    mark_all(&ctxs);
    assert_eq!(marked(&ctxs), vec![true, true, true]);
    /* A same-width edit on page 1 (the first) leaves pages 2 and 3 alone. */
    let evt = apply(
        &mut e,
        Command::InsertText {
            at: Some(bpos_top(0, 0)),
            text: "x".into(),
        },
    );
    assert!(!matches!(evt, Event::Error { .. }), "{evt:?}");
    assert_eq!(
        marked(&ctxs),
        vec![false, true, true],
        "only page 1 repaints"
    );
    /* A paint request with nothing changed keeps every canvas ... */
    mark_all(&ctxs);
    assert!(matches!(
        apply(&mut e, paint.clone()),
        Event::Painted { .. }
    ));
    assert_eq!(marked(&ctxs), vec![true, true, true]);
    /* ... but paints a canvas registered since (a fresh surface). */
    let (canvas, ctx) = fresh_canvas();
    mark_all(std::slice::from_ref(&ctx));
    e.set_page_canvas(2, canvas).expect("re-register");
    ctxs[2] = ctx;
    assert!(matches!(apply(&mut e, paint), Event::Painted { .. }));
    assert_eq!(marked(&ctxs), vec![true, true, false]);
}

/// Issue #502 — a canvas whose 2D context was lost and restored comes back
/// blank; `forget_canvas_memo(idx)` makes the next paint of THAT page
/// unconditional (the #497 memo would skip it, its display list being
/// unchanged) while the other pages keep skipping. The marker drawn on
/// each canvas stands in for the page's pixels: a repaint clears it, a
/// skipped canvas keeps it. Browser-only, like the test above.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen_test::wasm_bindgen_test]
fn forgetting_a_canvas_memo_repaints_only_that_page() {
    use web_sys::OffscreenCanvas;
    const MARK: [u8; 3] = [0xff, 0x00, 0xff];
    let mut e = tests::test_engine_with_doc(DocumentTree::from_paragraphs(
        (1..=200).map(|i| format!("Line {i}")),
    ));
    let evt = apply(&mut e, Command::ExpandLayout { target_y: 1e9 });
    let Event::Painted { page_count, .. } = evt else {
        panic!("EXPAND_LAYOUT: {evt:?}");
    };
    assert!(
        page_count >= 3,
        "fixture must span 3+ pages, got {page_count}"
    );
    let ctxs: Vec<OffscreenCanvasRenderingContext2d> = (0..3)
        .map(|i| {
            let canvas = OffscreenCanvas::new(1, 1).expect("OffscreenCanvas");
            let ctx = canvas
                .get_context("2d")
                .expect("2d")
                .expect("2d")
                .dyn_into::<OffscreenCanvasRenderingContext2d>()
                .expect("2d ctx");
            e.set_page_canvas(i, canvas).expect("register");
            ctx
        })
        .collect();
    let mark_all = || {
        for ctx in &ctxs {
            ctx.set_fill_style_str("#ff00ff");
            ctx.fill_rect(0.0, 0.0, 2.0, 2.0);
        }
    };
    let marked = || -> Vec<bool> {
        ctxs.iter()
            .map(|ctx| {
                let px = ctx.get_image_data(0.0, 0.0, 1.0, 1.0).expect("read").data();
                px[..3] == MARK
            })
            .collect()
    };
    let paint = Command::RequestPaint {
        viewport: BridgeRect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        },
        dirty: None,
    };
    assert!(matches!(
        apply(&mut e, paint.clone()),
        Event::Painted { .. }
    ));
    /* Everything is painted and up to date: a paint request keeps all. */
    mark_all();
    assert!(matches!(
        apply(&mut e, paint.clone()),
        Event::Painted { .. }
    ));
    assert_eq!(marked(), vec![true, true, true], "up-to-date pages skip");
    /* Page 2 (index 1) loses its context: forget it, the others skip. */
    assert!(e.forget_canvas_memo(1));
    assert!(matches!(
        apply(&mut e, paint.clone()),
        Event::Painted { .. }
    ));
    assert_eq!(
        marked(),
        vec![true, false, true],
        "only the forgotten page repaints"
    );
    /* The repaint re-recorded it: it skips again. */
    mark_all();
    assert!(matches!(apply(&mut e, paint), Event::Painted { .. }));
    assert_eq!(marked(), vec![true, true, true]);
    /* An index with no canvas registered is reported, not an error. */
    assert!(!e.forget_canvas_memo(40));
}
