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
        assert!(!matches!(evt, Event::Error { .. }), "LoadFont {id}: {evt:?}");
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
    assert!(!matches!(evt, Event::Error { .. }), "SetFontPriority: {evt:?}");
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
            (S::Arabic, vec!["amiri".to_string(), "noto-naskh".to_string()]),
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
