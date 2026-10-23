//! Issue #553 — probe.
use super::*;

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

fn engine_with(doc: DocumentTree) -> Engine {
    let mut e = tests::test_engine_with_doc(doc);
    let bytes = include_bytes!("../../../ts/fonts/NotoNaskhArabic-Regular.ttf").to_vec();
    let font = LoadedFont::parse("test-arabic".to_string(), bytes).expect("arabic font");
    e.fonts.insert("test-arabic".to_string(), Arc::new(font));
    e
}

#[test]
fn probe() {
    let text = "Hello world مرحبا بالعالم";
    let mut e = engine_with(DocumentTree::from_text(text));
    let geom = e.document_geometry().expect("geometry");
    for l in &geom {
        eprintln!(
            "line start_x {} hit [{}, {}] y {} h {} bytes {}..{}",
            l.start_x,
            l.hit_left,
            l.hit_left + l.hit_width,
            l.y_top,
            l.height,
            l.start_byte,
            l.end_byte
        );
        for r in &l.runs {
            eprintln!(
                "  run {}..{} slots {:?}",
                r.src_start,
                r.src_end,
                r.slots.iter().map(|s| (s.x, s.byte)).collect::<Vec<_>>()
            );
        }
    }
    let l = &geom[0];
    for x in [l.hit_left + l.hit_width - 2.0, l.hit_left + 1.0] {
        let at = BridgePoint {
            x,
            y: l.y_top + l.height + 40.0,
        };
        let hit = apply(&mut e, Command::HitTest { at });
        eprintln!("hit {x}: {hit:?}");
        let sel = apply(&mut e, Command::SelectWordAt { at });
        if let Event::SelectionChanged { range, .. } = sel {
            let t = &text[range.start.offset as usize..range.end.offset as usize];
            eprintln!("selected {t:?}");
        }
    }
}
