//! Issue #501 - the command-boundary font-size range guard.
//!
//! Word caps a character at 1638 pt and floors it at 1 pt
//! ([`engine::MAX_FONT_SIZE_PT`] / [`engine::MIN_FONT_SIZE_PT`]); the
//! #407 finiteness guard only refuses `NaN` / infinity, so
//! `ApplyFormatting` took `1.0951575e33` pt: a line taller than its page,
//! a paginator running height past every finite cull target, and the
//! #499 stall. Since #501 the `.docx` reader and the HTML paste door
//! CLAMP into the bound (a file or a clipboard is not the user's to
//! re-type); this guard is the command path's twin, run by
//! `Engine::apply_command` right after the EMU guard and before any gate
//! or handler:
//!
//! - `ApplyFormatting.attrs.font_size` - the run / mark / pending-format
//!   size;
//! - `ModifyStyle.properties.run_props.font_size` - a style's size;
//! - `px_size` of the Phase-1 harness commands (`RenderPage` - the
//!   document's default size - `RasterizeGlyph`, `ShapeAndRasterize`),
//!   in device px but the same `1..=1638` bound (1 pt = 1 px there).
//!
//! A value outside is refused with `Event::Error { kind: OutOfRange }`
//! naming the command, the field and the value; nothing is applied (the
//! #341 "error => no mutation" invariant).
//!
//! A NEW command field that sets a font size must be added to
//! [`first_refused`].

use bridge::{Command, ErrorKind, Event};
use engine::{MAX_FONT_SIZE_PT, MIN_FONT_SIZE_PT, font_size_in_bound};

/// A refused field: its name and value.
struct Refused {
    field: &'static str,
    value: f32,
}

fn size(field: &'static str, value: f32) -> Option<Refused> {
    (!font_size_in_bound(value)).then_some(Refused { field, value })
}

fn optional(field: &'static str, value: Option<f32>) -> Option<Refused> {
    value.and_then(|v| size(field, v))
}

fn first_refused(cmd: &Command) -> Option<Refused> {
    match cmd {
        Command::ApplyFormatting { attrs, .. } => optional("attrs.font_size", attrs.font_size),
        Command::ModifyStyle { properties, .. } => properties
            .run_props
            .as_ref()
            .and_then(|r| optional("properties.run_props.font_size", r.font_size)),
        Command::RenderPage { px_size, .. }
        | Command::RasterizeGlyph { px_size, .. }
        | Command::ShapeAndRasterize { px_size, .. } => size("px_size", *px_size),
        _ => None,
    }
}

/// `Ok` when every font size `cmd` carries is inside Word's 1 pt .. 1638 pt;
/// otherwise the typed `OutOfRange` reply (the dispatcher stamps the wire
/// name on it).
pub(crate) fn check_command(cmd: &Command) -> Result<(), Box<Event>> {
    match first_refused(cmd) {
        None => Ok(()),
        Some(r) => Err(Box::new(Event::error_kind(
            ErrorKind::OutOfRange,
            format!(
                "{}: {} {} is out of range ({}..={}, Word's font-size limits)",
                cmd.kind().name(),
                r.field,
                r.value,
                MIN_FONT_SIZE_PT,
                MAX_FONT_SIZE_PT
            ),
        ))),
    }
}
