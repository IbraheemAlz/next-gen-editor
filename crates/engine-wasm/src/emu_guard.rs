//! Issue #459 - the command-boundary EMU range guard.
//!
//! `Command::ResizeImage` / `MoveImage` carry integer EMU fields. The #407
//! finiteness guard cannot see an integer, so `i64::MAX` used to reach
//! layout as a `(i64::MAX as f32) / EMU_PER_PX` box. Since #407 the `.docx`
//! reader clamps every extent / offset it reads to Word's 22 in
//! ([`engine::MAX_DRAWING_EMU`], 20 116 800 EMU); this guard is the
//! command path's twin of that bound, run by `Engine::apply_command` right
//! after the text guard and before any gate or handler:
//!
//! - a picture extent (`width_emu`, `height_emu`) must lie in
//!   `1..=MAX_DRAWING_EMU` - a zero or negative size has no box to resize
//!   (the shell's resize handles never send one), like `InsertTextBox`;
//! - a fixed offset (`offset_h_emu`, `offset_v_emu`) must lie in
//!   `-MAX_DRAWING_EMU..=MAX_DRAWING_EMU` (`ST_Coordinate`, signed).
//!
//! A value outside is refused with `Event::Error { kind: OutOfRange }`
//! naming the command, the field and the value; nothing is applied (the
//! #341 "error => no mutation" invariant). The model's own setters clamp as
//! well (`engine::clamp_extent_emu` / `clamp_offset_emu`), so a direct
//! caller of the document API cannot get past the bound either.
//!
//! A NEW command field that carries a drawing extent or offset must be
//! added to [`check_command`].

use bridge::{Command, ErrorKind, Event};
use engine::MAX_DRAWING_EMU;

/// A refused field: its name and value, and the range it had to lie in.
struct Refused {
    field: &'static str,
    value: i64,
    min: i64,
}

fn extent(field: &'static str, value: i64) -> Option<Refused> {
    (!(1..=MAX_DRAWING_EMU).contains(&value)).then_some(Refused {
        field,
        value,
        min: 1,
    })
}

fn offset(field: &'static str, value: i64) -> Option<Refused> {
    (!(-MAX_DRAWING_EMU..=MAX_DRAWING_EMU).contains(&value)).then_some(Refused {
        field,
        value,
        min: -MAX_DRAWING_EMU,
    })
}

fn first_refused(cmd: &Command) -> Option<Refused> {
    match cmd {
        Command::ResizeImage {
            width_emu,
            height_emu,
            ..
        } => extent("width_emu", *width_emu).or_else(|| extent("height_emu", *height_emu)),
        Command::MoveImage {
            offset_h_emu,
            offset_v_emu,
            ..
        } => {
            offset("offset_h_emu", *offset_h_emu).or_else(|| offset("offset_v_emu", *offset_v_emu))
        }
        _ => None,
    }
}

/// `Ok` when every drawing extent / offset `cmd` carries is inside the 22 in
/// bound; otherwise the typed `OutOfRange` reply (the dispatcher stamps the
/// wire name on it).
pub(crate) fn check_command(cmd: &Command) -> Result<(), Box<Event>> {
    match first_refused(cmd) {
        None => Ok(()),
        Some(r) => Err(Box::new(Event::error_kind(
            ErrorKind::OutOfRange,
            format!(
                "{}: {} {} EMU is out of range ({}..={} EMU, Word's 22 in)",
                cmd.kind().name(),
                r.field,
                r.value,
                r.min,
                MAX_DRAWING_EMU
            ),
        ))),
    }
}
