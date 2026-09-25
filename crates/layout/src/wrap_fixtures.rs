//! Issue #82 — end-to-end text-wrap fixtures: real fonts, the real
//! paragraph composer, the real paginator + float resolver, driven through
//! the anchor → position → wrap → reflow loop ([`WrapConvergence`]) exactly
//! the way the engine drives it. One fixture per wrap mode, in an LTR and
//! an RTL paragraph, each pinned by [`geometry_fingerprint`]; plus the
//! oscillating case (an object whose wrap pushes its own anchor line).

use crate::boxes::{
    FloatOffsetPx, FloatSpec, FloatWrap, LayoutBlock, PageBox, ParagraphBox, StyleSpan, WrapSide,
};
use crate::page::A4Page;
use crate::paginate::{HeaderBands, PageGeometry, Paginator};
use crate::paragraph::{
    InlineObjectInfo, InlineObjectInfoKind, ParagraphConfig, layout_paragraph,
    layout_paragraph_wrapped,
};
use crate::watchdog::{DegradeReason, LayoutDegradation, geometry_fingerprint};
use crate::wrap::{WrapConvergence, WrapPlan, WrapVerdict};
use crate::{Point, WrapCutout};
use engine::{HRelativeFrom, VRelativeFrom, WrapKind};
use std::collections::HashMap;
use std::sync::Arc;
use text_pipeline::{Alignment, FontStack, LoadedFont, ShapingDirection};

const LINE_H: f32 = 16.0;
const OBJ_W: f32 = 120.0;
const OBJ_H: f32 = 80.0;

fn fonts() -> FontStack {
    let latin = LoadedFont::parse(
        "latin".into(),
        include_bytes!("../../../ts/fonts/LiberationSans-Regular.ttf").to_vec(),
    )
    .expect("parse LiberationSans");
    let arabic = LoadedFont::parse(
        "arabic".into(),
        include_bytes!("../../../ts/fonts/Amiri-Regular.ttf").to_vec(),
    )
    .expect("parse Amiri");
    let mut faces: HashMap<String, Arc<LoadedFont>> = HashMap::new();
    faces.insert("latin".into(), Arc::new(latin));
    faces.insert("arabic".into(), Arc::new(arabic));
    FontStack::from_faces(faces, "latin")
}

fn geometry() -> PageGeometry {
    let page = A4Page::a4();
    PageGeometry {
        width: page.width,
        height: page.height,
        margins: page.margin,
        header_offset: 36.0,
        footer_offset: 36.0,
    }
}

fn span(len: usize) -> StyleSpan {
    StyleSpan {
        start: 0,
        end: len as u32,
        px_size: 12.0,
        color: [0, 0, 0, 255],
        bold: false,
        italic: false,
        underline: engine::UnderlineStyle::None,
        strike: false,
        bg_color: None,
        font_family: None,
        caps_transform: false,
        baseline_shift_px: 0.0,
    }
}

fn ltr_text() -> String {
    let body = "The quick brown fox jumps over the lazy dog while the text \
                flows around a floating picture placed inside the column. ";
    format!("\u{FFFC}{}", body.repeat(5))
}

fn rtl_text() -> String {
    let body = "\u{0627}\u{0644}\u{0646}\u{0635} \u{064A}\u{0644}\u{062A}\u{0641} \
                \u{062D}\u{0648}\u{0644} \u{0627}\u{0644}\u{0635}\u{0648}\u{0631}\u{0629} \
                \u{0641}\u{064A} \u{0627}\u{0644}\u{0639}\u{0645}\u{0648}\u{062F} ";
    format!("\u{FFFC}{}", body.repeat(9))
}

/// One test document: a lead-in paragraph, the paragraph carrying the
/// float (sentinel at byte 0), and a trailing paragraph.
struct Doc {
    paras: Vec<DocPara>,
}

/// Paragraph text, direction, and the float it anchors (if any).
type DocPara = (String, ShapingDirection, Option<(FloatSpec, FloatWrap)>);

fn doc(dir: ShapingDirection, spec: FloatSpec, wrap: FloatWrap) -> Doc {
    let text = match dir {
        ShapingDirection::Ltr => ltr_text(),
        ShapingDirection::Rtl => rtl_text(),
    };
    let lead = match dir {
        ShapingDirection::Ltr => "A short lead-in paragraph.".to_string(),
        ShapingDirection::Rtl => "\u{0645}\u{0642}\u{062F}\u{0645}\u{0629}".to_string(),
    };
    Doc {
        paras: vec![
            (lead.clone(), dir, None),
            (text, dir, Some((spec, wrap))),
            (lead, dir, None),
        ],
    }
}

fn spec_at(h: f32, v: f32, v_frame: VRelativeFrom) -> FloatSpec {
    FloatSpec {
        h_frame: HRelativeFrom::Column,
        h_offset: FloatOffsetPx::Px(h),
        v_frame,
        v_offset: FloatOffsetPx::Px(v),
        simple_pos: None,
        z_order: 1,
        behind_doc: false,
        hidden: false,
    }
}

fn wrap(kind: WrapKind) -> FloatWrap {
    FloatWrap {
        kind,
        side: WrapSide::Both,
        dist_top: 4.0,
        dist_bottom: 4.0,
        dist_left: 8.0,
        dist_right: 8.0,
        polygon: None,
    }
}

/// One layout pass against `plan` — the engine's build, in miniature.
fn pass(d: &Doc, fonts: &FontStack, plan: &WrapPlan) -> Vec<PageBox> {
    let geom = geometry();
    let width = geom.width - geom.margins.left - geom.margins.right;
    let mut pag = Paginator::new(
        geom,
        HeaderBands::default(),
        HeaderBands::default(),
        false,
        false,
    )
    .with_strict_watchdog(true);
    for (id, (text, dir, float)) in d.paras.iter().enumerate() {
        let spans = [span(text.len())];
        let infos: Vec<InlineObjectInfo> = float
            .iter()
            .map(|(spec, w)| InlineObjectInfo {
                at: 0,
                width_px: OBJ_W,
                height_px: OBJ_H,
                kind: InlineObjectInfoKind::FloatingImage {
                    rel_id: "rId1".into(),
                    spec: *spec,
                    wrap: w.clone(),
                },
            })
            .collect();
        let cfg = ParagraphConfig {
            text,
            fonts,
            spans: &spans,
            base_direction: *dir,
            max_width: width,
            line_height: LINE_H,
            line_height_exact: false,
            alignment: Alignment::Justify,
            indent_start_px: 0.0,
            indent_end_px: 0.0,
            first_line_indent_px: 0.0,
            hanging_indent_px: 0.0,
            marker_text: None,
            px_size_for_marker: 12.0,
            inline_objects: &infos,
            tab_stops_px: &[],
        };
        let cuts: &[WrapCutout] = plan.get(&(id as u32)).map_or(&[], |v| v.as_slice());
        let mut p = layout_paragraph_wrapped(cfg, cuts);
        p.source_paragraph_id = id as u32;
        pag.push_block(LayoutBlock::Paragraph(p), 0.0, 6.0);
    }
    pag.finish()
}

/// Drive the loop to its verdict. Returns the final pages, the loop's
/// notes and the pass count.
fn converge(d: &Doc, strict: bool) -> (Vec<PageBox>, Vec<LayoutDegradation>, u32) {
    let fonts = fonts();
    let mut conv = WrapConvergence::new(LINE_H).strict(strict);
    let mut plan = WrapPlan::new();
    loop {
        let pages = pass(d, &fonts, &plan);
        match conv.observe(&pages, &plan) {
            WrapVerdict::Converged | WrapVerdict::Capped => {
                let passes = conv.passes();
                return (pages, conv.take_notes(), passes);
            }
            WrapVerdict::Continue(next) => plan = next,
        }
        assert!(conv.passes() <= 32, "the wrap loop must terminate");
    }
}

fn wrapped_para(pages: &[PageBox]) -> &ParagraphBox {
    pages[0]
        .blocks
        .iter()
        .filter_map(LayoutBlock::as_paragraph)
        .find(|p| p.source_paragraph_id == 1)
        .expect("float paragraph on page 0")
}

/// The float's rect in paragraph-box space, expanded by its distances.
fn float_rect(pages: &[PageBox], w: &FloatWrap) -> (f32, f32, f32, f32) {
    let page = &pages[0];
    let p = wrapped_para(pages);
    let f = &page.floats[0];
    let px = page.margins.left + p.origin.x;
    let py = page.margins.top + p.origin.y;
    (
        f.origin.x - px - w.dist_left,
        f.origin.x + f.size.width - px + w.dist_right,
        f.origin.y - py - w.dist_top,
        f.origin.y + f.size.height - py + w.dist_bottom,
    )
}

/// No line's ink span may enter the (distance-expanded) object rect.
fn assert_no_line_enters(pages: &[PageBox], rect: (f32, f32, f32, f32)) {
    let (x0, x1, y0, y1) = rect;
    for l in &wrapped_para(pages).lines {
        let (ly0, ly1) = (l.origin.y, l.origin.y + l.height);
        if ly1 <= y0 + 0.01 || ly0 >= y1 - 0.01 {
            continue;
        }
        let (lx0, lx1) = (l.origin.x, l.origin.x + l.width);
        assert!(
            lx1 <= x0 + 0.5 || lx0 >= x1 - 0.5,
            "line [{lx0}, {lx1}] × [{ly0}, {ly1}] enters the object [{x0}, {x1}] × [{y0}, {y1}]"
        );
    }
}

fn unwrapped_lines(d: &Doc) -> Vec<(f32, f32, f32)> {
    let fonts = fonts();
    let pages = pass(d, &fonts, &WrapPlan::new());
    wrapped_para(&pages)
        .lines
        .iter()
        .map(|l| (l.origin.x, l.origin.y, l.width))
        .collect()
}

fn run_mode(dir: ShapingDirection, kind: WrapKind) -> (Vec<PageBox>, Vec<LayoutDegradation>, u32) {
    let w = wrap(kind);
    let d = doc(dir, spec_at(160.0, 24.0, VRelativeFrom::Paragraph), w);
    converge(&d, true)
}

#[test]
fn square_wrap_splits_bands_around_the_object_ltr_and_rtl() {
    for dir in [ShapingDirection::Ltr, ShapingDirection::Rtl] {
        let (pages, notes, passes) = run_mode(dir, WrapKind::Square);
        assert!(notes.is_empty(), "{notes:?}");
        assert!((2..=3).contains(&passes), "{dir:?}: {passes} passes");
        let w = wrap(WrapKind::Square);
        assert_no_line_enters(&pages, float_rect(&pages, &w));
        let p = wrapped_para(&pages);
        /* Some band is cut in two, and its two lines share one baseline. */
        let cut: Vec<_> = p.lines.iter().filter(|l| l.segments.len() == 2).collect();
        assert!(cut.len() >= 4, "{dir:?}: {} cut lines", cut.len());
        let first_band_y = cut[0].origin.y;
        let siblings: Vec<_> = cut.iter().filter(|l| l.origin.y == first_band_y).collect();
        assert_eq!(siblings.len(), 2, "{dir:?}: one line per segment");
        assert_eq!(siblings[0].baseline, siblings[1].baseline);
        /* Reading order: LTR fills the left segment first, RTL the right. */
        match dir {
            ShapingDirection::Ltr => assert_eq!((siblings[0].segment, siblings[1].segment), (0, 1)),
            ShapingDirection::Rtl => assert_eq!((siblings[0].segment, siblings[1].segment), (1, 0)),
        }
        /* Source order is preserved across the band. */
        assert!(siblings[0].source_start < siblings[1].source_start);
    }
}

#[test]
fn top_and_bottom_wrap_skips_the_band_ltr_and_rtl() {
    for dir in [ShapingDirection::Ltr, ShapingDirection::Rtl] {
        let (pages, notes, _) = run_mode(dir, WrapKind::TopAndBottom);
        assert!(notes.is_empty(), "{notes:?}");
        let w = wrap(WrapKind::TopAndBottom);
        let (_, _, y0, y1) = float_rect(&pages, &w);
        let p = wrapped_para(&pages);
        for l in &p.lines {
            let (ly0, ly1) = (l.origin.y, l.origin.y + l.height);
            assert!(
                ly1 <= y0 + 0.01 || ly0 >= y1 - 0.01,
                "{dir:?}: a line sits beside a top-and-bottom object"
            );
        }
        assert!(
            p.lines.iter().any(|l| l.origin.y >= y1 - 0.01),
            "text resumes below"
        );
    }
}

#[test]
fn tight_and_through_wrap_follow_the_polygon() {
    /* A left-pointing wedge: full width at the top, a point at the bottom
    left. Tight / through bands shrink toward the bottom, so the right
    segment's left edge moves LEFT as the bands descend. */
    for kind in [WrapKind::Tight, WrapKind::Through] {
        for dir in [ShapingDirection::Ltr, ShapingDirection::Rtl] {
            let mut w = wrap(kind);
            w.polygon = Some(vec![
                Point { x: 0.0, y: 0.0 },
                Point { x: 21600.0, y: 0.0 },
                Point { x: 0.0, y: 21600.0 },
                Point { x: 0.0, y: 0.0 },
            ]);
            let d = doc(
                dir,
                spec_at(160.0, 24.0, VRelativeFrom::Paragraph),
                w.clone(),
            );
            let (pages, notes, _) = converge(&d, true);
            assert!(notes.is_empty(), "{notes:?}");
            let p = wrapped_para(&pages);
            let right_starts: Vec<f32> = p
                .lines
                .iter()
                .filter(|l| l.segments.len() == 2)
                .map(|l| l.segments[1].x0)
                .collect();
            assert!(right_starts.len() >= 3, "{kind:?}/{dir:?}");
            assert!(
                right_starts.first() > right_starts.last(),
                "{kind:?}/{dir:?}: the polygon narrows downwards: {right_starts:?}"
            );
            /* Tighter than square: the lowest cut band leaves more room
            than the object's bounding box would. */
            let (_, bx1, _, _) = float_rect(&pages, &w);
            assert!(*right_starts.last().unwrap() < bx1 - 10.0);
        }
    }
}

#[test]
fn tight_without_polygon_falls_back_to_square_and_reports() {
    let d = doc(
        ShapingDirection::Ltr,
        spec_at(160.0, 24.0, VRelativeFrom::Paragraph),
        wrap(WrapKind::Tight),
    );
    let (pages, notes, _) = converge(&d, false);
    assert!(
        notes
            .iter()
            .all(|n| n.reason == DegradeReason::WrapPolygonFallback),
        "{notes:?}"
    );
    assert_eq!(notes.len(), 1, "one report, not one per pass: {notes:?}");
    assert_no_line_enters(&pages, float_rect(&pages, &wrap(WrapKind::Tight)));
}

#[test]
fn in_front_and_behind_objects_do_not_cut_text() {
    for dir in [ShapingDirection::Ltr, ShapingDirection::Rtl] {
        for behind in [false, true] {
            let mut spec = spec_at(160.0, 24.0, VRelativeFrom::Paragraph);
            spec.behind_doc = behind;
            let d = doc(dir, spec, wrap(WrapKind::None));
            let (pages, notes, passes) = converge(&d, true);
            assert!(notes.is_empty());
            assert_eq!(passes, 1, "no wrapping float ⇒ the loop never iterates");
            let p = wrapped_para(&pages);
            assert!(p.lines.iter().all(|l| l.segments.is_empty()));
            let got: Vec<_> = p
                .lines
                .iter()
                .map(|l| (l.origin.x, l.origin.y, l.width))
                .collect();
            assert_eq!(got, unwrapped_lines(&d));
        }
    }
}

#[test]
fn empty_cutouts_are_the_unwrapped_composer_byte_for_byte() {
    let fonts = fonts();
    for (text, dir) in [
        (ltr_text(), ShapingDirection::Ltr),
        (rtl_text(), ShapingDirection::Rtl),
    ] {
        let spans = [span(text.len())];
        let cfg = || ParagraphConfig {
            text: &text,
            fonts: &fonts,
            spans: &spans,
            base_direction: dir,
            max_width: 300.0,
            line_height: LINE_H,
            line_height_exact: false,
            alignment: Alignment::Justify,
            indent_start_px: 12.0,
            indent_end_px: 4.0,
            first_line_indent_px: 20.0,
            hanging_indent_px: 0.0,
            marker_text: None,
            px_size_for_marker: 12.0,
            inline_objects: &[],
            tab_stops_px: &[],
        };
        let a = layout_paragraph(cfg());
        let b = layout_paragraph_wrapped(cfg(), &[]);
        let page = |p: ParagraphBox| PageBox {
            size: crate::Size {
                width: 600.0,
                height: 800.0,
            },
            margins: crate::Margins::uniform(50.0),
            blocks: vec![LayoutBlock::Paragraph(p)],
            header: None,
            footer: None,
            header_offset: 20.0,
            footer_offset: 20.0,
            footnotes: crate::NoteBand::default(),
            endnotes: crate::NoteBand::default(),
            hf_role: crate::HeaderRole::Default,
            page_number: 1,
            floats: Vec::new(),
        };
        assert_eq!(
            geometry_fingerprint(&[page(a)]),
            geometry_fingerprint(&[page(b)])
        );
    }
}

/// The oscillation the issue names: a top-and-bottom object positioned
/// against its own anchor LINE. Its cutout pushes that line below the
/// object; the object follows the line; the cutout pushes it again… The
/// loop must end within its caps, report, and still paint every line.
#[test]
fn object_that_pushes_its_own_anchor_terminates_within_the_cap() {
    for dir in [ShapingDirection::Ltr, ShapingDirection::Rtl] {
        let d = doc(
            dir,
            spec_at(160.0, 0.0, VRelativeFrom::Line),
            wrap(WrapKind::TopAndBottom),
        );
        let (pages, notes, passes) = converge(&d, false);
        assert!(
            passes <= WrapConvergence::DEFAULT_MAX_PASSES,
            "{dir:?}: {passes} passes"
        );
        assert!(
            notes.iter().any(|n| matches!(
                n.reason,
                DegradeReason::WrapObjectFrozen | DegradeReason::WrapOscillation
            )),
            "{dir:?}: the escape hatch is reported: {notes:?}"
        );
        /* Nothing dropped: the float paragraph's text is fully laid out. */
        let text_len = d.paras[1].0.len() as u32;
        let covered: u32 = pages
            .iter()
            .flat_map(|pg| pg.blocks.iter())
            .filter_map(LayoutBlock::as_paragraph)
            .filter(|p| p.source_paragraph_id == 1)
            .flat_map(|p| p.lines.iter())
            .flat_map(|l| l.runs.iter())
            .map(|r| r.source_range.end - r.source_range.start)
            .sum();
        assert!(
            covered + 8 >= text_len,
            "{covered} of {text_len} bytes laid out"
        );
    }
}

/// Pinned geometry per wrap mode × direction. A change here moves the
/// wrap goldens — regenerate deliberately, never incidentally.
#[test]
fn wrap_fixtures_are_pinned_by_geometry_fingerprint() {
    let mut got = Vec::new();
    for dir in [ShapingDirection::Ltr, ShapingDirection::Rtl] {
        for kind in [
            WrapKind::None,
            WrapKind::Square,
            WrapKind::TopAndBottom,
            WrapKind::Tight,
            WrapKind::Through,
        ] {
            let mut w = wrap(kind);
            if matches!(kind, WrapKind::Tight | WrapKind::Through) {
                w.polygon = Some(vec![
                    Point { x: 0.0, y: 0.0 },
                    Point { x: 21600.0, y: 0.0 },
                    Point {
                        x: 10800.0,
                        y: 21600.0,
                    },
                ]);
            }
            let d = doc(dir, spec_at(160.0, 24.0, VRelativeFrom::Paragraph), w);
            let (pages, _, _) = converge(&d, true);
            got.push((format!("{dir:?}/{kind:?}"), geometry_fingerprint(&pages)));
        }
    }
    let pinned: &[(&str, u64)] = PINNED;
    if std::env::var_os("NGE_PRINT_WRAP_FINGERPRINTS").is_some() {
        for (k, v) in &got {
            eprintln!("    (\"{k}\", {v}),");
        }
    }
    for ((k, v), (pk, pv)) in got.iter().zip(pinned) {
        assert_eq!(k, pk);
        assert_eq!(v, pv, "{k}: wrap geometry moved");
    }
    assert_eq!(got.len(), pinned.len());
}

/// Tight and through agree: interior (concave) polygon regions are not
/// opened to text — the cutout per band is the polygon's horizontal
/// extent (a documented simplification of `wrapThrough`).
const PINNED: &[(&str, u64)] = &[
    ("Ltr/None", 18105673287643388339),
    ("Ltr/Square", 13899711935861691388),
    ("Ltr/TopAndBottom", 1022183598207855315),
    ("Ltr/Tight", 6914234635741952144),
    ("Ltr/Through", 6914234635741952144),
    ("Rtl/None", 11106573741201510221),
    ("Rtl/Square", 15148194970088276630),
    ("Rtl/TopAndBottom", 1622021226766814731),
    ("Rtl/Tight", 6020911245480317949),
    ("Rtl/Through", 6020911245480317949),
];
