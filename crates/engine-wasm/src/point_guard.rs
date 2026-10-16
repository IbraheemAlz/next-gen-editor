//! Issue #503 (item 2) - the command-boundary range guard for the page /
//! paragraph point fields.
//!
//! The #407 finiteness guard only refuses `NaN` / infinity, so
//! `SetPageMargins` took a `1e33` pt margin, `SetParagraphIndent` an indent
//! of `f32::MAX`, `SetColumns` a gutter wider than the universe and
//! `SetTabStops` a stop at `-1e30`: finite, absurd, and a page with no text
//! area for the layout to survive. Since #503 the `.docx` reader CLAMPS into
//! Word's limits (a file is not the user's to re-type); this guard is the
//! command path's twin, run by `Engine::apply_command` right after the
//! font-size guard and before any gate or handler:
//!
//! | command | field | accepted |
//! | --- | --- | --- |
//! | `SetPageMargins` | `top_pt` / `right_pt` / `bottom_pt` / `left_pt` | `0 ..= 1584` |
//! | `SetColumns` | `gutter_pt` | `0 ..= 1584` |
//! | `SetParagraphIndent` | `start_pt` / `end_pt` / `first_line_pt` | `-1584 ..= 1584` |
//! | `SetLineSpacing` | `multiplier` | `<= 0` (clear) or `0.06 ..= 132` |
//! | `SetTabStops` | `stops[i].position_pt` | `-1584 ..= 1584` |
//! | `ModifyStyle` | `properties.para_props.{indent_start_pt, indent_end_pt, first_line_pt, line_spacing_multiplier}` | as the direct commands |
//!
//! (1584 pt = 22 in, Word's largest margin / indent / spacing / tab
//! position; see `engine::point_bounds`.) What a single field cannot show -
//! the margins of ONE axis together leaving at least 0.1 in of text area on
//! the section's actual page - is checked by `Engine::do_set_page_margins`
//! (it knows the section), with the same `OutOfRange` kind.
//!
//! A value outside is refused with `Event::Error { kind: OutOfRange }`
//! naming the command, the field and the value; nothing is applied (the
//! #341 "error => no mutation" invariant).
//!
//! A NEW command field that carries a page / paragraph measure in points
//! must be added to [`first_refused`].

use bridge::{BridgeParaPropertiesPatch, Command, ErrorKind, Event};
use engine::{
    MAX_LINE_SPACING_MULTIPLE, MAX_POINT_FIELD_PT, MIN_LINE_SPACING_MULTIPLE,
    column_gutter_in_bound, indent_in_bound, line_spacing_multiple_in_bound, margin_in_bound,
    tab_position_in_bound,
};

/// A refused field: its name, value and the range it should have held.
struct Refused {
    field: String,
    value: f32,
    range: String,
}

fn margin(field: &str, value: f32) -> Option<Refused> {
    (!margin_in_bound(value)).then(|| Refused {
        field: field.to_string(),
        value,
        range: format!("0..={MAX_POINT_FIELD_PT}, Word's margin limits"),
    })
}

fn gutter(field: &str, value: f32) -> Option<Refused> {
    (!column_gutter_in_bound(value)).then(|| Refused {
        field: field.to_string(),
        value,
        range: format!("0..={MAX_POINT_FIELD_PT}, Word's column spacing limits"),
    })
}

fn indent(field: &str, value: f32) -> Option<Refused> {
    (!indent_in_bound(value)).then(|| Refused {
        field: field.to_string(),
        value,
        range: format!(
            "-{MAX_POINT_FIELD_PT}..={MAX_POINT_FIELD_PT}, Word's indent limits (22 in)"
        ),
    })
}

fn tab(field: &str, value: f32) -> Option<Refused> {
    (!tab_position_in_bound(value)).then(|| Refused {
        field: field.to_string(),
        value,
        range: format!(
            "-{MAX_POINT_FIELD_PT}..={MAX_POINT_FIELD_PT}, Word's tab position limits (22 in)"
        ),
    })
}

fn multiple(field: &str, value: f32) -> Option<Refused> {
    (!line_spacing_multiple_in_bound(value)).then(|| Refused {
        field: field.to_string(),
        value,
        range: format!(
            "<= 0 clears, else {MIN_LINE_SPACING_MULTIPLE}..={MAX_LINE_SPACING_MULTIPLE}, \
             Word's Multiple line-spacing limits"
        ),
    })
}

fn para_patch(patch: &BridgeParaPropertiesPatch) -> Option<Refused> {
    let base = "properties.para_props";
    patch
        .indent_start_pt
        .and_then(|v| indent(&format!("{base}.indent_start_pt"), v))
        .or_else(|| {
            patch
                .indent_end_pt
                .and_then(|v| indent(&format!("{base}.indent_end_pt"), v))
        })
        .or_else(|| {
            patch
                .first_line_pt
                .and_then(|v| indent(&format!("{base}.first_line_pt"), v))
        })
        .or_else(|| {
            patch
                .line_spacing_multiplier
                .and_then(|v| multiple(&format!("{base}.line_spacing_multiplier"), v))
        })
}

fn first_refused(cmd: &Command) -> Option<Refused> {
    match cmd {
        Command::SetPageMargins {
            top_pt,
            right_pt,
            bottom_pt,
            left_pt,
            ..
        } => margin("top_pt", *top_pt)
            .or_else(|| margin("right_pt", *right_pt))
            .or_else(|| margin("bottom_pt", *bottom_pt))
            .or_else(|| margin("left_pt", *left_pt)),
        Command::SetColumns { gutter_pt, .. } => gutter("gutter_pt", *gutter_pt),
        Command::SetParagraphIndent {
            start_pt,
            end_pt,
            first_line_pt,
            ..
        } => indent("start_pt", *start_pt)
            .or_else(|| indent("end_pt", *end_pt))
            .or_else(|| indent("first_line_pt", *first_line_pt)),
        Command::SetLineSpacing { multiplier, .. } => multiple("multiplier", *multiplier),
        Command::SetTabStops { stops, .. } => stops
            .iter()
            .enumerate()
            .find_map(|(i, s)| tab(&format!("stops[{i}].position_pt"), s.position_pt)),
        Command::ModifyStyle { properties, .. } => {
            properties.para_props.as_ref().and_then(para_patch)
        }
        _ => None,
    }
}

/// `Ok` when every page / paragraph point field `cmd` carries is inside
/// Word's bound; otherwise the typed `OutOfRange` reply (the dispatcher
/// stamps the wire name on it).
pub(crate) fn check_command(cmd: &Command) -> Result<(), Box<Event>> {
    match first_refused(cmd) {
        None => Ok(()),
        Some(r) => Err(Box::new(Event::error_kind(
            ErrorKind::OutOfRange,
            format!(
                "{}: {} {} is out of range ({})",
                cmd.kind().name(),
                r.field,
                r.value,
                r.range
            ),
        ))),
    }
}
