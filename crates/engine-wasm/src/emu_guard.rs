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

/* The guard itself needs no engine: these run in a plain `cargo test`
(the engine-level twins in `emu_guard_tests.rs` are `fuzz-native`-gated and
drive the same refusals through `Engine::apply_sync`). */
#[cfg(test)]
mod tests {
    use super::*;
    use bridge::BlockPath;

    fn resize(w: i64, h: i64) -> Command {
        Command::ResizeImage {
            path: BlockPath::top(0),
            at: 1,
            width_emu: w,
            height_emu: h,
            story: Vec::new(),
        }
    }

    fn move_to(h: i64, v: i64) -> Command {
        Command::MoveImage {
            path: BlockPath::top(0),
            at: 1,
            offset_h_emu: h,
            offset_v_emu: v,
            story: Vec::new(),
        }
    }

    fn refusal(cmd: &Command) -> (Option<ErrorKind>, String) {
        match check_command(cmd) {
            Ok(()) => panic!("expected a refusal of {cmd:?}"),
            Err(evt) => match *evt {
                Event::Error { kind, message, .. } => (kind, message),
                other => panic!("expected Event::Error, got {other:?}"),
            },
        }
    }

    /// The command path's bound IS the reader's: one shared constant, 22 in.
    #[test]
    fn the_bound_is_words_22_inches() {
        assert_eq!(MAX_DRAWING_EMU, 22 * 914_400);
        assert_eq!(MAX_DRAWING_EMU, 20_116_800);
    }

    #[test]
    fn an_extent_must_lie_in_one_to_the_bound() {
        for ok in [1, 914_400, MAX_DRAWING_EMU] {
            assert!(check_command(&resize(ok, ok)).is_ok(), "{ok}");
        }
        for bad in [
            0,
            -1,
            i64::MIN,
            i64::MIN + 1,
            MAX_DRAWING_EMU + 1,
            i64::MAX - 1,
            i64::MAX,
        ] {
            let (kind, message) = refusal(&resize(bad, 914_400));
            assert_eq!(kind, Some(ErrorKind::OutOfRange), "{bad}");
            assert!(message.contains("width_emu"), "{message}");
            assert!(message.contains(&bad.to_string()), "{message}");
            let (kind, message) = refusal(&resize(914_400, bad));
            assert_eq!(kind, Some(ErrorKind::OutOfRange), "{bad}");
            assert!(message.contains("height_emu"), "{message}");
        }
    }

    #[test]
    fn an_offset_must_lie_within_plus_minus_the_bound() {
        for ok in [0, -1, 1, MAX_DRAWING_EMU, -MAX_DRAWING_EMU] {
            assert!(check_command(&move_to(ok, ok)).is_ok(), "{ok}");
        }
        for bad in [
            MAX_DRAWING_EMU + 1,
            -MAX_DRAWING_EMU - 1,
            i64::MAX,
            i64::MIN,
            i64::MAX - 1,
            i64::MIN + 1,
        ] {
            let (kind, message) = refusal(&move_to(bad, 0));
            assert_eq!(kind, Some(ErrorKind::OutOfRange), "{bad}");
            assert!(message.contains("offset_h_emu"), "{message}");
            let (kind, message) = refusal(&move_to(0, bad));
            assert_eq!(kind, Some(ErrorKind::OutOfRange), "{bad}");
            assert!(message.contains("offset_v_emu"), "{message}");
        }
    }

    /// The first offender is the one named, whichever axis it is on.
    #[test]
    fn the_first_offending_field_is_named() {
        let (_, message) = refusal(&resize(i64::MAX, i64::MAX));
        assert!(message.contains("width_emu"), "{message}");
        let (_, message) = refusal(&move_to(i64::MAX, i64::MAX));
        assert!(message.contains("offset_h_emu"), "{message}");
    }

    /// Only the drawing commands are inspected; every other command passes
    /// through untouched (their own guards run elsewhere).
    #[test]
    fn other_commands_are_not_inspected() {
        for cmd in [Command::Undo, Command::Redo, Command::SelectAll] {
            assert!(check_command(&cmd).is_ok(), "{cmd:?}");
        }
    }
}
