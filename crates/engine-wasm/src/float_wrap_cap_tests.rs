//! Issue #347 - the paragraph float-wrap composer's defensive band cap is
//! an escape hatch, so it is REPORTED: `layout::layout_paragraph_wrapped`
//! hands back `DegradeReason::FloatWrapCapped`, the engine's one door to
//! it (`lay_out_wrapped`) turns that into a `LayoutDegradeReason::
//! FloatWrapCapped` note on `Event::Painted.layout_degraded`, and the
//! strict test sink (`StrictLayoutNotes`, the counterpart of
//! `Paginator::with_strict_watchdog`) makes it a hard failure.
//!
//! No finite wrap plan can reach the real cap (every band consumes text or
//! passes a cutout edge; the layout crate's `wrap_fixtures` pins the
//! composer itself), so the engine path is driven through the
//! `ForcedWrapBandCap` test seam. The paragraph is laid out beside a REAL
//! square-wrapped floating picture, through `Engine::build_pages`.

use super::*;
use crate::tests::test_engine_with_doc;

/// One long paragraph with a 1 x 0.5 in square-wrapped floating picture
/// 1.5 in into the column, 0.25 in below the paragraph top.
fn square_wrapped_doc() -> DocumentTree {
    let body = "Wrapped text flows on both sides of the picture while the \
                paragraph continues for several more lines of prose. "
        .repeat(6);
    let mut doc = DocumentTree::from_text(&format!("\u{FFFC}{body}"));
    if let Some(p) = doc.blocks[0].as_paragraph_mut() {
        p.inline_objects.push(engine::InlineObject {
            at: 0,
            kind: engine::InlineKind::Image {
                rel_id: "nge_float_1".to_string(),
                width_emu: 914_400,
                height_emu: 457_200,
                media_key: None,
            },
            anchor: Some(Box::new(engine::FloatAnchor {
                position_h: engine::HPosition {
                    relative_from: engine::HRelativeFrom::Column,
                    offset: engine::FloatOffset::Emu(1_371_600),
                },
                position_v: engine::VPosition {
                    relative_from: engine::VRelativeFrom::Paragraph,
                    offset: engine::FloatOffset::Emu(228_600),
                },
                dist_left_emu: 114_300,
                dist_right_emu: 114_300,
                wrap: engine::WrapKind::Square,
                ..engine::FloatAnchor::default()
            })),
            source_xml: None,
        });
    }
    doc
}

/// The bytes of the paragraph's source text its lines paint.
fn painted_bytes(pages: &[PageBox]) -> u32 {
    pages
        .iter()
        .flat_map(|p| p.blocks.iter())
        .filter_map(LayoutBlock::as_paragraph)
        .flat_map(|p| p.lines.iter())
        .flat_map(|l| l.runs.iter())
        .map(|r| r.source_range.end - r.source_range.start)
        .sum()
}

fn cut_line_count(pages: &[PageBox]) -> usize {
    pages
        .iter()
        .flat_map(|p| p.blocks.iter())
        .filter_map(LayoutBlock::as_paragraph)
        .flat_map(|p| p.lines.iter())
        .filter(|l| !l.segments.is_empty())
        .count()
}

fn capped_notes(degradations: &[LayoutDegraded]) -> usize {
    degradations
        .iter()
        .filter(|d| d.reason == LayoutDegradeReason::FloatWrapCapped)
        .count()
}

/// The nominal path notes nothing, even under the strict sink: the
/// float cuts the paragraph's bands and no cap is anywhere near.
#[test]
fn the_nominal_wrap_path_reports_no_cap() {
    let _strict = StrictLayoutNotes::on();
    let engine = test_engine_with_doc(square_wrapped_doc());
    let (pages, _, _, info) = engine.build_pages(2.0, false, None).expect("layout");
    assert!(info.degradations.is_empty(), "{:?}", info.degradations);
    assert!(
        cut_line_count(&pages) > 2,
        "the fixture must wrap beside the picture over several bands"
    );
}

/// A forced cap: the paragraph is still painted in full, the cutouts are
/// ignored past the cap, and the paint's degradation notes carry
/// `FloatWrapCapped` (the layout of the last pass; no page, like every
/// sub-paginator note).
#[test]
fn a_capped_wrap_paints_all_text_and_reports_float_wrap_capped() {
    let nominal = test_engine_with_doc(square_wrapped_doc());
    let (nominal_pages, _, _, nominal_info) =
        nominal.build_pages(2.0, false, None).expect("layout");
    assert_eq!(capped_notes(&nominal_info.degradations), 0);

    let _forced = ForcedWrapBandCap::to(2);
    let engine = test_engine_with_doc(square_wrapped_doc());
    let (pages, _, _, info) = engine.build_pages(2.0, false, None).expect("layout");

    assert_eq!(
        info.degradations,
        vec![LayoutDegraded {
            reason: LayoutDegradeReason::FloatWrapCapped,
            page: None,
        }],
        "exactly one note for the one capped paragraph"
    );
    assert_eq!(
        painted_bytes(&pages),
        painted_bytes(&nominal_pages),
        "a capped composer paints the rest of the text, it never drops it"
    );
    assert!(
        cut_line_count(&pages) < cut_line_count(&nominal_pages),
        "past the cap the cutouts are ignored ({} vs {} cut lines)",
        cut_line_count(&pages),
        cut_line_count(&nominal_pages)
    );
}

/// The strict sink turns the note into a hard failure, the way a strict
/// `layout::Watchdog` does for its own reasons - so a test (or CI corpus
/// run) that hits the cap fails instead of recovering silently.
#[test]
#[should_panic(expected = "layout watchdog (strict): FloatWrapCapped")]
fn a_capped_wrap_is_a_hard_failure_under_the_strict_sink() {
    let _forced = ForcedWrapBandCap::to(2);
    let _strict = StrictLayoutNotes::on();
    let engine = test_engine_with_doc(square_wrapped_doc());
    let _ = engine.build_pages(2.0, false, None);
}

/// The mapping is one-to-one: every layout reason has a bridge spelling
/// (`bridge_reason` is an exhaustive match, so a new `DegradeReason` does
/// not compile unclassified), and the layout crate's wire-friendly name
/// matches the bridge enum's serde spelling (pinned in `bridge`'s tests).
#[test]
fn the_new_reason_maps_one_to_one_onto_the_bridge() {
    assert_eq!(
        bridge_reason(layout::DegradeReason::FloatWrapCapped),
        LayoutDegradeReason::FloatWrapCapped
    );
    assert_eq!(
        layout::DegradeReason::FloatWrapCapped.as_str(),
        "FLOAT_WRAP_CAPPED"
    );
}
